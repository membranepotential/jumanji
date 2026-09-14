//! jumanji macOS spike: verifies the assumptions the mac shell (issue #1)
//! rests on, against the real system WKWebView through wry + tao.
//!
//! Checks, in the order they run:
//!   A  init-script + IPC round trip (`window.__jmnj_post` over `window.ipc`)
//!   B  `evaluate_script_with_callback` delivers a JSON-serialized object
//!   F  custom protocol serves the document *and* a document-relative image;
//!      also: can that jmnj:// origin load an absolute file:// image? (no)
//!   D  CSS Custom Highlight API present and functional
//!   C  `WebView::zoom` reaches `WKWebView.pageZoom` and reflows the viewport
//!   E  focus handoff: is the webview first responder without a click, and
//!      does a synthesized keyDown reach the in-page capture listener?
//!   G1 (control) objc `loadHTMLString:baseURL:` with a file:// base — do a
//!      document-relative image and an absolute file:// image in another
//!      directory load? (Result on macOS 26: yes, both.)
//!   G2 (control) wry `load_html`, which has no base-URL parameter (neither)
//!
//! `controls/filebase.swift` asks the G1/G2 question again without wry or
//! objc2, on a raw WKWebView with a default configuration.
//!
//! Eval results and timers are routed back to the main thread through the
//! tao `EventLoopProxy`, the same parking pattern `controller/toolkit.rs`
//! describes for the non-`Send` controller callbacks.

use std::borrow::Cow;
use std::path::PathBuf;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{ClassType, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSView};
use objc2_foundation::{NSPoint, NSString, NSURL};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::http::{Response, header::CONTENT_TYPE};
use wry::{WebView, WebViewBuilder, WebViewExtMacOS};

const PNG: &[u8] = include_bytes!("img.png");

const HTML: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<style>body{font:16px system-ui;margin:0;padding:24px} main{max-width:40em}
::highlight(jmnj-find){background:#ff0;color:#000}</style></head>
<body><main>
<h1 id="top">Spike document</h1>
<p>The quick brown fox jumps over the lazy dog. Find me: <b>needle</b>.</p>
<img id="rel" src="img.png" alt="one pixel, document-relative">
<img id="abs" src="__ABS__" alt="one pixel, absolute file:// in another directory">
<p>Second paragraph with another needle in it.</p>
</main></body></html>"#;

const INIT_JS: &str = r#"
(function () {
  const post = (name, payload) => {
    if (window.ipc && window.ipc.postMessage) {
      window.ipc.postMessage(name + '\u001f' + String(payload));
    } else {
      // wry's own `window.ipc` shim was not there yet — remember that fact.
      setTimeout(() => window.ipc.postMessage(name + '\u001fdeferred:' + String(payload)), 0);
    }
  };
  Object.defineProperty(window, '__jmnj_post', { value: post });
  post('init', document.readyState);
  document.addEventListener('keydown', e => post('key', e.key), true);
})();
"#;

enum Msg {
    Ipc(String, String),
    Eval(u32, String),
    Tick(u32),
}

struct Results {
    lines: Vec<String>,
}

impl Results {
    fn record(&mut self, id: &str, ok: Option<bool>, detail: impl Into<String>) {
        let mark = match ok {
            Some(true) => "PASS",
            Some(false) => "FAIL",
            None => "INFO",
        };
        let line = format!("{mark}  {id}  {}", detail.into());
        eprintln!("{line}");
        self.lines.push(line);
    }
}

fn after(proxy: &EventLoopProxy<Msg>, ms: u64, n: u32) {
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(ms));
        let _ = proxy.send_event(Msg::Tick(n));
    });
}

fn eval_json(webview: &WebView, proxy: &EventLoopProxy<Msg>, id: u32, js: &str) {
    let proxy = proxy.clone();
    webview
        .evaluate_script_with_callback(js, move |json| {
            let _ = proxy.send_event(Msg::Eval(id, json));
        })
        .expect("evaluate_script_with_callback");
}

fn first_responder_report(webview: &WebView) -> (bool, String) {
    let wk = webview.webview();
    let window = webview.ns_window();
    let Some(fr) = window.firstResponder() else {
        return (false, "firstResponder = nil".into());
    };
    let class = fr.class().name().to_string_lossy().into_owned();
    if !fr.isKindOfClass(NSView::class()) {
        return (false, format!("firstResponder = {class} (not a view)"));
    }
    // Safety: isKindOfClass(NSView) just confirmed the cast.
    let view: &NSView = unsafe { &*(Retained::as_ptr(&fr) as *const NSView) };
    let wk_view: &NSView = &wk;
    let inside = view.isDescendantOf(wk_view);
    (
        inside,
        format!("firstResponder = {class}, inside webview = {inside}"),
    )
}

fn send_key(webview: &WebView, ch: &str, code: u16) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    let window = webview.ns_window();
    let wnum = window.windowNumber();
    let chars = NSString::from_str(ch);
    let ev = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::ZERO,
        NSEventModifierFlags(0),
        0.0,
        wnum,
        None,
        &chars,
        &chars,
        false,
        code,
    )
    .expect("keyEvent");
    app.sendEvent(&ev);
}

/// `naturalWidth` of the relative (`#rel`) and absolute (`#abs`) test images
/// (-1 if an element is missing), plus whether every image has settled.
/// Synchronous on purpose: wry evaluates through
/// `evaluateJavaScript:completionHandler:`, which does not await a Promise;
/// the probes run several hundred ms after the load, which is plenty for a
/// 70-byte local PNG, and `settled` says so explicitly.
fn img_width_js(extra: &str) -> String {
    format!(
        "(() => {{ \
           const w = id => {{ const i = document.getElementById(id); return i ? i.naturalWidth : -1; }}; \
           return {{img: w('rel'), abs: w('abs'), settled: [...document.images].every(i => i.complete){extra}}}; \
         }})()"
    )
}

fn main() {
    // Negative-control fixture: a directory with img.png next to the "document".
    let fixture_dir: PathBuf = std::env::temp_dir().join("jumanji-mac-spike");
    std::fs::create_dir_all(&fixture_dir).unwrap();
    // A *different* file name than the protocol serves, so a cache hit on
    // jmnj://doc/img.png cannot masquerade as a file:// load.
    std::fs::write(fixture_dir.join("img2.png"), PNG).unwrap();
    // The absolute image lives in a *different* directory than the base, so
    // it also tells whether read access is confined to the base directory.
    let other_dir = fixture_dir.join("other");
    std::fs::create_dir_all(&other_dir).unwrap();
    std::fs::write(other_dir.join("abs.png"), PNG).unwrap();
    let abs_url = format!("file://{}", other_dir.join("abs.png").display());
    let html_proto: &'static str = Box::leak(HTML.replace("__ABS__", &abs_url).into_boxed_str());
    let html_file_base = HTML
        .replace("img.png", "img2.png")
        .replace("__ABS__", &abs_url);

    let event_loop: EventLoop<Msg> = EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("jumanji mac spike")
        .with_inner_size(tao::dpi::LogicalSize::new(900.0, 600.0))
        .build(&event_loop)
        .unwrap();

    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_initialization_script(INIT_JS)
        .with_ipc_handler(move |req| {
            let body = req.body();
            let (name, payload) = body.split_once('\u{1f}').unwrap_or((body.as_str(), ""));
            let _ = ipc_proxy.send_event(Msg::Ipc(name.to_string(), payload.to_string()));
        })
        .with_custom_protocol("jmnj".into(), |_id, req| {
            let path = req.uri().path().to_string();
            let (status, body, mime): (u16, Cow<'static, [u8]>, &str) = match path.as_str() {
                "/index.html" | "/" => (
                    200,
                    Cow::Borrowed(html_proto.as_bytes()),
                    "text/html; charset=utf-8",
                ),
                "/img.png" => (200, Cow::Borrowed(PNG), "image/png"),
                _ => (404, Cow::Borrowed(b"not found"), "text/plain"),
            };
            Response::builder()
                .status(status)
                .header(CONTENT_TYPE, mime)
                .body(body)
                .unwrap()
        })
        .with_url("jmnj://doc/index.html")
        .build(&window)
        .unwrap();

    let mut results = Results { lines: Vec::new() };
    let mut innerwidth_before = 0.0f64;
    let mut key_received = false;
    let mut key_phase = 0u8; // 0 = not sent, 1 = sent cold, 2 = sent after focus()
    let mut init_seen = false;

    // Fallback: if the init IPC never arrives, say so and still run the rest.
    after(&proxy, 3000, 0);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                let ver = std::process::Command::new("sw_vers")
                    .arg("-productVersion")
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                eprintln!("macOS {ver}, wry 0.57, tao 0.37");
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(Msg::Ipc(name, payload)) => match name.as_str() {
                "init" if !init_seen => {
                    init_seen = true;
                    let deferred = payload.starts_with("deferred:");
                    results.record(
                        "A ipc-roundtrip",
                        Some(true),
                        format!(
                            "init script posted via window.ipc, readyState={payload}{}",
                            if deferred { " (window.ipc was NOT yet defined at document-start!)" } else { "" }
                        ),
                    );
                    after(&proxy, 500, 1);
                }
                "init" => {} // the negative-control reload posts again; ignore
                "key" => {
                    if !key_received {
                        key_received = true;
                        results.record(
                            "E key-routing",
                            Some(true),
                            format!(
                                "in-page capture listener received key={payload:?} ({})",
                                if key_phase == 1 { "cold: no click, no focus() call" } else { "only after webview.focus()" }
                            ),
                        );
                    }
                }
                other => eprintln!("ipc {other}: {payload}"),
            },
            Event::UserEvent(Msg::Tick(0)) => {
                if !init_seen {
                    results.record("A ipc-roundtrip", Some(false), "no init message within 3 s");
                    after(&proxy, 0, 1);
                }
            }
            Event::UserEvent(Msg::Tick(1)) => {
                let js = img_width_js(
                    r#",
                    p: 42, s: 'str', y: window.scrollY, iw: window.innerWidth,
                    hlApi: typeof Highlight === 'function' && !!(window.CSS && CSS.highlights),
                    hlSize: (() => { try {
                        const p = document.querySelector('p'); const r = new Range();
                        r.setStart(p.firstChild, 4); r.setEnd(p.firstChild, 9);
                        CSS.highlights.set('jmnj-find', new Highlight(r));
                        return CSS.highlights.size; } catch (e) { return 'error: ' + e; } })(),
                    hasFocus: document.hasFocus(),
                    ua: navigator.userAgent"#,
                );
                eval_json(&webview, &proxy, 1, &js);
            }
            Event::UserEvent(Msg::Eval(1, json)) => {
                let v: serde_json::Value = match serde_json::from_str(&json) {
                    Ok(v) => v,
                    Err(e) => {
                        results.record("B eval-json", Some(false), format!("not JSON: {e}; raw={json:?}"));
                        after(&proxy, 0, 9);
                        return;
                    }
                };
                results.record(
                    "B eval-json",
                    Some(v["p"] == 42 && v["s"] == "str"),
                    format!("object round-tripped as JSON: p={} s={} y={}", v["p"], v["s"], v["y"]),
                );
                results.record(
                    "F custom-protocol",
                    Some(v["img"] == 1),
                    format!(
                        "under jmnj://doc/: relative img.naturalWidth={} (served by the protocol); absolute file:// img.naturalWidth={} (0 = a jmnj:// origin cannot load file:// resources)",
                        v["img"], v["abs"]
                    ),
                );
                results.record(
                    "D css-highlight",
                    Some(v["hlApi"] == true && v["hlSize"] == 1),
                    format!(
                        "Highlight + CSS.highlights present={}, registered highlight count={}",
                        v["hlApi"], v["hlSize"]
                    ),
                );
                results.record(
                    "E focus (JS)",
                    Some(v["hasFocus"] == true),
                    format!("document.hasFocus()={} before any click", v["hasFocus"]),
                );
                results.record("ua", None, v["ua"].as_str().unwrap_or("").to_string());
                innerwidth_before = v["iw"].as_f64().unwrap_or(0.0);
                webview.zoom(1.5).expect("zoom");
                after(&proxy, 300, 2);
            }
            Event::UserEvent(Msg::Tick(2)) => {
                eval_json(&webview, &proxy, 2, "window.innerWidth");
            }
            Event::UserEvent(Msg::Eval(2, json)) => {
                let after_w: f64 = json.trim().parse().unwrap_or(0.0);
                let ratio = if after_w > 0.0 { innerwidth_before / after_w } else { 0.0 };
                let native = unsafe { webview.webview().pageZoom() };
                results.record(
                    "C zoom",
                    Some((ratio - 1.5).abs() < 0.05 && (native - 1.5).abs() < 1e-6),
                    format!(
                        "innerWidth {innerwidth_before} -> {after_w} (ratio {ratio:.3}), WKWebView.pageZoom={native}"
                    ),
                );
                webview.zoom(1.0).unwrap();

                let (inside, detail) = first_responder_report(&webview);
                results.record("E focus (AppKit)", Some(inside), format!("{detail} — before any click or focus() call"));
                key_phase = 1;
                send_key(&webview, "j", 38);
                after(&proxy, 500, 3);
            }
            Event::UserEvent(Msg::Tick(3)) => {
                if key_received {
                    after(&proxy, 0, 5);
                } else {
                    results.record(
                        "E key-routing",
                        Some(false),
                        "synthesized keyDown 'j' did not reach the page cold; retrying after webview.focus()",
                    );
                    webview.focus().unwrap();
                    let (inside, detail) = first_responder_report(&webview);
                    results.record("E focus (after focus())", Some(inside), detail);
                    key_phase = 2;
                    send_key(&webview, "j", 38);
                    after(&proxy, 500, 4);
                }
            }
            Event::UserEvent(Msg::Tick(4)) => {
                if !key_received {
                    results.record("E key-routing", Some(false), "keyDown still not delivered after focus()");
                }
                after(&proxy, 0, 5);
            }
            Event::UserEvent(Msg::Tick(5)) => {
                // G: negative control — file:// base URL, relative image.
                let base = NSURL::fileURLWithPath(&NSString::from_str(&format!("{}/", fixture_dir.display())));
                unsafe {
                    webview
                        .webview()
                        .loadHTMLString_baseURL(&NSString::from_str(&html_file_base), Some(&base));
                }
                after(&proxy, 800, 6);
            }
            Event::UserEvent(Msg::Tick(6)) => {
                eval_json(&webview, &proxy, 3, &img_width_js(", base: document.baseURI"));
            }
            Event::UserEvent(Msg::Eval(3, json)) => {
                let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
                results.record(
                    "G1 file:// base via objc loadHTMLString:baseURL:",
                    None,
                    format!(
                        "relative img.naturalWidth={}, absolute file:// (other dir) img.naturalWidth={}, baseURI={} (1/1 = WKWebView loads local images from a file:// base, not confined to the base dir)",
                        v["img"], v["abs"], v["base"]
                    ),
                );
                // G2: wry's own load_html has no base-URL parameter at all.
                webview.load_html(&html_file_base).unwrap();
                after(&proxy, 800, 7);
            }
            Event::UserEvent(Msg::Tick(7)) => {
                eval_json(&webview, &proxy, 4, &img_width_js(", base: document.baseURI"));
            }
            Event::UserEvent(Msg::Eval(4, json)) => {
                let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
                results.record(
                    "G2 wry load_html (no base)",
                    None,
                    format!(
                        "relative img.naturalWidth={}, absolute file:// img.naturalWidth={}, baseURI={}",
                        v["img"], v["abs"], v["base"]
                    ),
                );
                after(&proxy, 0, 9);
            }
            Event::UserEvent(Msg::Tick(9)) => {
                eprintln!("\n==== SUMMARY ====");
                for l in &results.lines {
                    println!("{l}");
                }
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
