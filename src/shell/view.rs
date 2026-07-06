//! The WebKit content view: a thin, typed wrapper around `WebView`.
//!
//! All document interaction the shell needs — loading, scrolling, anchor
//! jumps, zoom, find, recolor, scroll-position round-tripping for reload — is
//! exposed as small methods that translate to `webkit6` calls and `window.*`
//! JavaScript snippets. Content itself is rendered 100% in Rust (see
//! `core::pipeline`); JS here only drives the viewport.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use gtk::gdk::RGBA;
use gtk::prelude::*;
use webkit6::prelude::*;
use webkit6::{
    FindController, FindOptions, NavigationPolicyDecision, NavigationType, PolicyDecisionType,
    UserContentInjectedFrames, UserContentManager, UserScript, UserScriptInjectionTime, WebView,
};

use crate::core::RenderedDocument;
use crate::core::config::SelectionClipboard;

/// A shell-supplied sink, installed after construction. Both link hints and
/// navigation routing hand a single string back to the shell (a JSON hint list
/// and a resolved target URI, respectively).
type Sink = Rc<RefCell<Option<Box<dyn Fn(String)>>>>;

/// Where a reflow-preserving zoom keeps the reading position pinned.
///
/// Both geometric and text zoom now reflow the page, so an anchor is captured
/// before the change and scrolled back into view after — this picks the anchor
/// element. One mechanism ([`capture_anchor_js`] + [`RESTORE_ANCHOR_JS`]),
/// parameterised by the probe point.
#[derive(Clone, Copy)]
pub enum ZoomAnchor {
    /// Keep the element at the top of the viewport fixed (keyboard / D-Bus
    /// zoom, and text zoom). Only anchors when scrolled, so an exact top stays
    /// exactly at the top.
    Top,
    /// Keep the element under a viewport point (CSS px) fixed — the cursor, for
    /// Ctrl+wheel zoom ("zoom towards the cursor").
    Point { x: f64, y: f64 },
}

/// A single, consistent viewport snapshot read by `GetState` and the statusbar.
/// All widths are CSS px.
#[derive(Debug, Clone)]
pub struct ViewportState {
    pub scroll_y: f64,
    /// Scroll progress 0..=100.
    pub scroll_percent: u32,
    /// Layout width of the content column (`main`). Reflows with geometric zoom
    /// now (it tracks the CSS viewport when the window is narrower than the
    /// column), unlike the old reflow-free design.
    pub content_width: f64,
    /// `window.innerWidth` — the CSS viewport width.
    pub viewport_width: f64,
    /// `document.scrollWidth` — must stay ≤ `viewport_width` (no page h-scroll).
    pub doc_scroll_width: f64,
    /// Rendered width of the first `.mermaid svg` (0 if none). CSS px, so its
    /// device size is `diagram_width × zoom`.
    pub diagram_width: f64,
    /// Rendered width of the first `<math>` element (0 if none). CSS px. Lets
    /// e2e assert MathML actually laid out with nonzero geometry.
    pub math_width: f64,
    /// Computed `color` of the first python function-name span
    /// (`.entity.name.function.python`), as a CSS `rgb(...)` string ("" if the
    /// document has no python code). Lets e2e assert the dark-mode syntax-CSS
    /// scoping fix: this must not be near-black (`rgb(50, 50, 50)`, i.e.
    /// `InspiredGithub`'s light colour) once dark mode is on.
    pub fn_color: String,
}

/// JS that captures the anchor element into `window.__jmnj_anchor` (element +
/// its current viewport-top offset) for the given probe point. Paired with
/// [`RESTORE_ANCHOR_JS`], which runs after the zoom change reflows the page.
fn capture_anchor_js(anchor: &ZoomAnchor) -> String {
    // (x expression, y-probe list, guard) — Top probes a few px down the column
    // centre and only when scrolled; Point probes exactly the given point.
    let (cx, ys, guard_open, guard_close) = match anchor {
        ZoomAnchor::Top => (
            "(() => { const m = document.querySelector('main') || document.body; \
              const r = m.getBoundingClientRect(); return r.left + r.width / 2; })()"
                .to_string(),
            "[8, 40, 80, 140]".to_string(),
            "if (window.scrollY > 0) {",
            "}",
        ),
        ZoomAnchor::Point { x, y } => (
            format!("Math.max(1, Math.min(innerWidth - 1, {x}))"),
            format!("[Math.max(1, Math.min(innerHeight - 1, {y}))]"),
            "",
            "",
        ),
    };
    format!(
        "(() => {{ window.__jmnj_anchor = null; {guard_open} \
           const cx = Math.max(1, Math.min(innerWidth - 1, {cx})); \
           for (const py of {ys}) {{ \
             const c = document.elementFromPoint(cx, py); \
             if (c && c !== document.body && c !== document.documentElement \
                 && c.tagName !== 'MAIN') {{ \
               window.__jmnj_anchor = {{ el: c, top: c.getBoundingClientRect().top }}; \
               break; }} }} {guard_close} }})();"
    )
}

/// JS that restores the reading position: scroll so the captured anchor returns
/// to the same viewport y it had before the reflow. No-op if nothing was
/// captured (e.g. an unscrolled Top anchor).
const RESTORE_ANCHOR_JS: &str = "(() => { const a = window.__jmnj_anchor; \
    if (a && a.el) { const nt = a.el.getBoundingClientRect().top; \
      window.scrollBy({ top: nt - a.top, left: 0, behavior: 'instant' }); } \
    window.__jmnj_anchor = null; })();";

/// Native WebView background painted behind the document, matched to the theme
/// so unpainted regions never flash a mismatched colour (light `#ffffff`,
/// dark `#1a1a1a` — the same values `style.css` uses for `--bg`).
const BG_LIGHT: RGBA = RGBA::WHITE;
const BG_DARK: RGBA = RGBA::new(0.101, 0.101, 0.101, 1.0);

/// The script-message handler name the selection user-script posts to.
const SELECTION_HANDLER: &str = "selection";
/// The script-message handler the link-hint overlay posts its `[{label,href}]`
/// list to.
const HINTS_HANDLER: &str = "hints";

#[derive(Clone)]
pub struct View {
    webview: WebView,
    /// The desired recolor (dark) state, tracked so `load_document` can pre-apply
    /// the `dark` class on `<html>` and paint dark from the very first frame.
    dark: Rc<Cell<bool>>,
    /// Called with the JSON `[{label,href}]` list the hint overlay posts back.
    hints_cb: Sink,
    /// Called with a resolved target URI when the webview tries to navigate
    /// (a link click); the shell decides whether to scroll, open, or delegate.
    navigate_cb: Sink,
}

impl View {
    pub fn new(selection_clipboard: SelectionClipboard) -> Self {
        let ucm = UserContentManager::new();
        install_selection_copy(&ucm, selection_clipboard);
        install_drag_select_reset(&ucm);
        let hints_cb: Sink = Rc::new(RefCell::new(None));
        install_hints(&ucm, hints_cb.clone());

        let webview = WebView::builder().user_content_manager(&ucm).build();
        webview.set_vexpand(true);
        webview.set_hexpand(true);
        webview.set_background_color(&BG_LIGHT);

        let navigate_cb: Sink = Rc::new(RefCell::new(None));
        install_navigation_policy(&webview, navigate_cb.clone());

        if let Some(settings) = WebViewExt::settings(&webview) {
            // We drive the viewport with `window.*` JS, so JavaScript stays on,
            // but the document itself is static and CSP-locked by the pipeline.
            settings.set_enable_javascript(true);
            // A local reader needs none of the network/storage/dev surface.
            settings.set_enable_developer_extras(false);
            settings.set_enable_page_cache(false);
            settings.set_enable_html5_database(false);
            settings.set_enable_html5_local_storage(false);
            settings.set_enable_offline_web_application_cache(false);
            settings.set_javascript_can_access_clipboard(false);
            settings.set_javascript_can_open_windows_automatically(false);
            // Zathura semantics: scrolling is immediate. Smooth scrolling makes
            // WebKit animate every wheel tick (~100 ms each), which reads as lag
            // on large documents; keyboard scrolls are JS `scrollBy` and are
            // instant either way.
            settings.set_enable_smooth_scrolling(false);
        }

        Self {
            webview,
            dark: Rc::new(Cell::new(false)),
            hints_cb,
            navigate_cb,
        }
    }

    pub fn widget(&self) -> &WebView {
        &self.webview
    }

    /// Install the shell's handler for the hint list the overlay posts back.
    pub fn set_hints_handler(&self, f: impl Fn(String) + 'static) {
        *self.hints_cb.borrow_mut() = Some(Box::new(f));
    }

    /// Install the shell's handler for attempted navigations (link clicks). The
    /// argument is the resolved absolute target URI.
    pub fn set_navigate_handler(&self, f: impl Fn(String) + 'static) {
        *self.navigate_cb.borrow_mut() = Some(Box::new(f));
    }

    /// Load a rendered document. `base` is the source file; its URI becomes the
    /// base against which document-relative images resolve. When dark mode is
    /// desired the `dark` class is pre-applied to `<html>` so the very first
    /// painted frame is already dark — no light flash on reloads or a
    /// `default-recolor = true` start.
    pub fn load_document(&self, doc: &RenderedDocument, base: &Path) {
        let base_uri = gtk::gio::File::for_path(base).uri();
        let html = if self.dark.get() {
            doc.html
                .replacen("<html lang=\"en\">", "<html lang=\"en\" class=\"dark\">", 1)
        } else {
            doc.html.clone()
        };
        self.webview.load_html(&html, Some(base_uri.as_str()));
    }

    fn run_js(&self, script: &str) {
        self.webview.evaluate_javascript(
            script,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            |_res| {},
        );
    }

    pub fn scroll_by(&self, dx: i64, dy: i64) {
        // `behavior: 'instant'` pins the zathura-instant semantics regardless of
        // the engine's smooth-scrolling setting: a repeated key must never
        // restart an in-flight scroll animation.
        self.run_js(&format!(
            "window.scrollBy({{left: {dx}, top: {dy}, behavior: 'instant'}});"
        ));
    }

    /// Scroll by a fraction of the viewport height (half-page navigation).
    pub fn scroll_half_page(&self, down: bool, times: u32) {
        let sign = if down { 1.0 } else { -1.0 };
        self.run_js(&format!(
            "window.scrollBy({{top: {sign} * (window.innerHeight / 2) * {times}, behavior: 'instant'}});"
        ));
    }

    pub fn scroll_to_top(&self) {
        self.run_js("window.scrollTo(0, 0);");
    }

    pub fn scroll_to_bottom(&self) {
        self.run_js("window.scrollTo(0, document.body.scrollHeight);");
    }

    /// Scroll a heading anchor into view. Accepts `#id` or a bare `id`.
    pub fn scroll_to_anchor(&self, anchor: &str) {
        let id = anchor.trim_start_matches('#');
        self.run_js(&format!(
            "{{ const e = document.getElementById({}); if (e) e.scrollIntoView(); }}",
            js_string(id)
        ));
    }

    /// Build the link-hint overlay: label every visible `<a href>` with a
    /// home-row-alphabet tag and post the `[{label,href}]` list back to the
    /// shell via the `hints` handler. `href` is the *resolved* absolute URI, so
    /// the shell's routing sees the same value a real click would.
    pub fn request_hints(&self) {
        self.run_js(HINTS_BUILD_JS);
    }

    /// Narrow the visible hints to those whose label starts with `typed`.
    pub fn filter_hints(&self, typed: &str) {
        self.run_js(&format!(
            "(() => {{ const o=document.getElementById('__jmnj_hints'); if(!o) return; \
               const t={typed}; \
               for (const el of o.querySelectorAll('.__jmnj_hint')) {{ \
                 el.style.display = el.getAttribute('data-label').indexOf(t)===0 ? '' : 'none'; }} }})();",
            typed = js_string(typed)
        ));
    }

    /// Remove the hint overlay.
    pub fn clear_hints(&self) {
        self.run_js(
            "(() => { const o=document.getElementById('__jmnj_hints'); if(o) o.remove(); })();",
        );
    }

    /// Geometric zoom without anchoring: set webkit full-page native zoom. The
    /// native `zoom_level` is a property of the WebView and survives a document
    /// reload, so this is used where the reading position is restored by other
    /// means — quickmark/history restores, which set the scroll offset
    /// explicitly. Diagrams scale with zoom by construction: WebKit multiplies
    /// their pinned CSS width (`--dw`) into device px (see `style.css`).
    pub fn set_zoom(&self, level: f64) {
        let level = level.max(0.2);
        self.webview.set_zoom_level(level);
    }

    /// Geometric zoom anchored at `anchor`. Because zoom now reflows the page,
    /// the reading position drifts unless pinned.
    ///
    /// `set_zoom_level` is a native call and cannot be issued from JS, so the
    /// sequence is race-free by construction: capture the anchor (async JS), and
    /// only in its completion callback set the native zoom and restore the
    /// position (a second JS eval). The two evals share `window.__jmnj_anchor`
    /// and can never interleave for one call, since the second is scheduled from
    /// the first's callback.
    pub fn zoom_to(&self, level: f64, anchor: ZoomAnchor) {
        let level = level.max(0.2);
        let webview = self.webview.clone();
        let capture = capture_anchor_js(&anchor);
        self.webview.evaluate_javascript(
            &capture,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |_res| {
                webview.set_zoom_level(level);
                webview.evaluate_javascript(
                    RESTORE_ANCHOR_JS,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |_| {},
                );
            },
        );
    }

    /// Reset both zoom axes to 100%, anchored once at the top of the viewport.
    /// A single capture spans both changes (geometric + text) so the reflow from
    /// each is corrected together rather than fighting two anchors.
    pub fn reset_zoom(&self, font_base_px: f64) {
        let webview = self.webview.clone();
        let capture = capture_anchor_js(&ZoomAnchor::Top);
        self.webview.evaluate_javascript(
            &capture,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |_res| {
                webview.set_zoom_level(1.0);
                let restore = format!(
                    "document.documentElement.style.setProperty('--font-size', '{font_base_px}px');\
                     {RESTORE_ANCHOR_JS}"
                );
                webview.evaluate_javascript(
                    &restore,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |_| {},
                );
            },
        );
    }

    /// Text zoom: set the effective body font size (px) via the `--font-size`
    /// custom property on `<html>`, reflowing prose. Re-applied after each load
    /// (the inline style is lost when the document reloads).
    ///
    /// Reflow moves content, so the top-of-viewport anchor is captured before the
    /// change and the position restored after. Pure JS (no native call), so
    /// capture → apply → restore fit in one eval — the same anchoring mechanism
    /// the geometric zoom uses, just applied inline.
    pub fn set_text_zoom_px(&self, px: f64) {
        let capture = capture_anchor_js(&ZoomAnchor::Top);
        self.run_js(&format!(
            "{capture}\
             document.documentElement.style.setProperty('--font-size', '{px}px');\
             {RESTORE_ANCHOR_JS}"
        ));
    }

    /// Record the desired recolor state and apply it: toggle the `dark` class on
    /// `<html>` (matching the pipeline's recolor CSS) and switch the native
    /// WebView background so unpainted regions match the theme.
    pub fn set_dark(&self, dark: bool) {
        self.dark.set(dark);
        self.webview
            .set_background_color(if dark { &BG_DARK } else { &BG_LIGHT });
        self.run_js(&format!(
            "document.documentElement.classList.toggle('dark', {dark});"
        ));
    }

    fn find_controller(&self) -> Option<FindController> {
        self.webview.find_controller()
    }

    pub fn find(&self, text: &str) {
        // Case-insensitive, wrapping search — the vim/zathura default.
        let opts = FindOptions::CASE_INSENSITIVE | FindOptions::WRAP_AROUND;
        if let Some(fc) = self.find_controller() {
            fc.search(text, opts.bits(), u32::MAX);
        }
    }

    pub fn find_next(&self) {
        if let Some(fc) = self.find_controller() {
            fc.search_next();
        }
    }

    pub fn find_previous(&self) {
        if let Some(fc) = self.find_controller() {
            fc.search_previous();
        }
    }

    pub fn find_clear(&self) {
        if let Some(fc) = self.find_controller() {
            fc.search_finish();
        }
    }

    /// Query the current scroll offset (px), delivering it to `callback` on the
    /// main loop. Used to preserve position across a reload.
    pub fn scroll_position<F: FnOnce(f64) + 'static>(&self, callback: F) {
        self.webview.evaluate_javascript(
            "window.scrollY",
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |res| {
                let y = res.ok().map(|v| v.to_double()).unwrap_or(0.0);
                callback(y);
            },
        );
    }

    /// Snapshot the viewport in one JS round-trip, delivering a [`ViewportState`]
    /// to `callback`. Used by the D-Bus `GetState` method (and the statusbar) so
    /// a single reply reflects one consistent snapshot. The extra widths let
    /// tests assert the reflow invariants: `doc_scroll_width ≤ viewport_width`
    /// (no page h-scroll) and diagram device growth (`diagram_width × zoom`).
    pub fn scroll_state<F: FnOnce(ViewportState) + 'static>(&self, callback: F) {
        let script = "(() => { const d = document.documentElement, b = document.body; \
             const max = (b.scrollHeight || d.scrollHeight) - window.innerHeight; \
             const p = max > 0 ? Math.round((window.scrollY / max) * 100) : 0; \
             const m = document.querySelector('main') || b; \
             const svg = document.querySelector('.mermaid svg'); \
             const math = document.querySelector('math'); \
             const fn = document.querySelector('.entity.name.function.python'); \
             return { y: window.scrollY, p: Math.min(100, Math.max(0, p)), \
                      w: m.offsetWidth, vw: window.innerWidth, \
                      dw: Math.max(d.scrollWidth, b.scrollWidth), \
                      gw: svg ? svg.getBoundingClientRect().width : 0, \
                      mw: math ? math.getBoundingClientRect().width : 0, \
                      fc: fn ? getComputedStyle(fn).color : '' }; })()";
        self.webview.evaluate_javascript(
            script,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |res| match res {
                Ok(v) => {
                    let num = |k| v.object_get_property(k).map_or(0.0, |n| n.to_double());
                    let fn_color = v
                        .object_get_property("fc")
                        .map(|s| s.to_str().to_string())
                        .unwrap_or_default();
                    callback(ViewportState {
                        scroll_y: num("y"),
                        scroll_percent: num("p").clamp(0.0, 100.0) as u32,
                        content_width: num("w"),
                        viewport_width: num("vw"),
                        doc_scroll_width: num("dw"),
                        diagram_width: num("gw"),
                        math_width: num("mw"),
                        fn_color,
                    });
                }
                Err(_) => callback(ViewportState {
                    scroll_y: 0.0,
                    scroll_percent: 0,
                    content_width: 0.0,
                    viewport_width: 0.0,
                    doc_scroll_width: 0.0,
                    diagram_width: 0.0,
                    math_width: 0.0,
                    fn_color: String::new(),
                }),
            },
        );
    }

    /// Restore a scroll offset (px) after a reload once layout is available.
    pub fn restore_scroll(&self, y: f64) {
        self.run_js(&format!("window.scrollTo(0, {y});"));
    }
}

/// Wire zathura-style copy-on-select: a user-script listens for
/// `selectionchange` (debounced ~200 ms) and posts the current non-empty
/// selection string to a script-message handler; the Rust side writes it to the
/// configured clipboard. An empty selection posts nothing, so we never clobber
/// the clipboard with `""`.
fn install_selection_copy(ucm: &UserContentManager, target: SelectionClipboard) {
    // register_script_message_handler returns false if the name is already
    // taken; on a fresh manager it always succeeds. The user-script only reaches
    // `postMessage` after this registers the handler.
    ucm.register_script_message_handler(SELECTION_HANDLER, None);

    const SOURCE: &str = "(function () {\n\
        let timer = null;\n\
        document.addEventListener('selectionchange', function () {\n\
          if (timer) clearTimeout(timer);\n\
          timer = setTimeout(function () {\n\
            const sel = window.getSelection ? window.getSelection().toString() : '';\n\
            if (sel && sel.length > 0) {\n\
              window.webkit.messageHandlers.selection.postMessage(sel);\n\
            }\n\
          }, 200);\n\
        });\n\
      })();";
    let script = UserScript::new(
        SOURCE,
        UserContentInjectedFrames::TopFrame,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    );
    ucm.add_script(&script);

    ucm.connect_script_message_received(Some(SELECTION_HANDLER), move |_, value| {
        let text = value.to_str();
        if text.is_empty() {
            return;
        }
        if let Some(display) = gtk::gdk::Display::default() {
            let clipboard = match target {
                SelectionClipboard::Primary => display.primary_clipboard(),
                SelectionClipboard::Clipboard => display.clipboard(),
            };
            clipboard.set_text(&text);
        }
    });
}

/// Make text selection behave like zathura / a plain text area: a press inside
/// an existing selection starts a *fresh* drag-selection instead of dragging the
/// selected text (WebKit's default). Two capture-phase listeners, installed as a
/// document-start user-script:
///
/// - `mousedown`: if the primary button presses inside the current non-collapsed
///   selection (tested against the selection range's client rects), collapse it
///   with `removeAllRanges()` so the native selection gesture restarts from this
///   point rather than picking up a text drag.
/// - `dragstart`: `preventDefault()` unconditionally — belt-and-braces so a text
///   drag can never begin even if the mousedown hit-test misses.
///
/// This is shell viewport glue, not content-pipeline JS (DESIGN D3 forbids JS in
/// the *rendering* pipeline; the shell already drives the page with JS).
fn install_drag_select_reset(ucm: &UserContentManager) {
    const SOURCE: &str = "(function () {\n\
        document.addEventListener('mousedown', function (e) {\n\
          if (e.button !== 0) return;\n\
          const sel = window.getSelection();\n\
          if (!sel || sel.isCollapsed || sel.rangeCount === 0) return;\n\
          const rects = sel.getRangeAt(0).getClientRects();\n\
          const x = e.clientX, y = e.clientY;\n\
          for (let i = 0; i < rects.length; i++) {\n\
            const r = rects[i];\n\
            if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) {\n\
              sel.removeAllRanges();\n\
              break;\n\
            }\n\
          }\n\
        }, true);\n\
        document.addEventListener('dragstart', function (e) { e.preventDefault(); }, true);\n\
      })();";
    let script = UserScript::new(
        SOURCE,
        UserContentInjectedFrames::TopFrame,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    );
    ucm.add_script(&script);
}

/// The overlay-building script for [`View::request_hints`]. Finds visible
/// links, assigns home-row-alphabet labels (`a`..`z`, then `aa`,`ab`,… past 26),
/// draws a fixed-position tag over each, and posts the label→href map to Rust.
const HINTS_BUILD_JS: &str = "(() => {\n\
    const old=document.getElementById('__jmnj_hints'); if(old) old.remove();\n\
    const vw=window.innerWidth, vh=window.innerHeight;\n\
    const links=Array.prototype.slice.call(document.querySelectorAll('a[href]')).filter(a=>{\n\
      const r=a.getBoundingClientRect();\n\
      if(r.width<=0||r.height<=0) return false;\n\
      if(r.bottom<0||r.top>vh||r.right<0||r.left>vw) return false;\n\
      const s=getComputedStyle(a);\n\
      return s.visibility!=='hidden'&&s.display!=='none';\n\
    });\n\
    const A='abcdefghijklmnopqrstuvwxyz', n=links.length, labels=[];\n\
    if(n<=A.length){ for(let i=0;i<n;i++) labels.push(A[i]); }\n\
    else { for(let i=0;i<A.length&&labels.length<n;i++) for(let j=0;j<A.length&&labels.length<n;j++) labels.push(A[i]+A[j]); }\n\
    const overlay=document.createElement('div');\n\
    overlay.id='__jmnj_hints';\n\
    overlay.style.cssText='position:fixed;left:0;top:0;width:0;height:0;z-index:2147483647;';\n\
    const out=[];\n\
    links.forEach((a,i)=>{\n\
      const r=a.getBoundingClientRect();\n\
      const tag=document.createElement('span');\n\
      tag.className='__jmnj_hint';\n\
      tag.setAttribute('data-label',labels[i]);\n\
      tag.textContent=labels[i];\n\
      tag.style.cssText='position:fixed;left:'+Math.max(0,r.left)+'px;top:'+Math.max(0,r.top)+'px;'+\n\
        'background:#ffd400;color:#000;font:bold 11px monospace;padding:0 3px;border-radius:3px;'+\n\
        'border:1px solid #806b00;pointer-events:none;box-shadow:0 1px 2px rgba(0,0,0,.4);';\n\
      overlay.appendChild(tag);\n\
      out.push(labels[i]+'\\t'+a.href);\n\
    });\n\
    document.documentElement.appendChild(overlay);\n\
    window.webkit.messageHandlers.hints.postMessage(out.join('\\n'));\n\
  })();";

/// Register the `hints` script-message handler: the overlay posts a JSON
/// `[{label,href}]` string, which we forward to the shell-installed sink.
fn install_hints(ucm: &UserContentManager, sink: Sink) {
    ucm.register_script_message_handler(HINTS_HANDLER, None);
    ucm.connect_script_message_received(Some(HINTS_HANDLER), move |_, value| {
        let json = value.to_str();
        if let Some(cb) = sink.borrow().as_ref() {
            cb(json.to_string());
        }
    });
}

/// Deny every webview-initiated navigation except the programmatic document
/// load (`load_html`/reload, which arrive as `NavigationType::Other`). A link
/// click is routed to the shell instead — the app itself never navigates
/// (DESIGN.md: offline-only, CSP-locked). See `set_navigate_handler`.
fn install_navigation_policy(webview: &WebView, sink: Sink) {
    webview.connect_decide_policy(move |_wv, decision, dtype| {
        if !matches!(
            dtype,
            PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction
        ) {
            return false; // resource-response decisions: default handling.
        }
        let Some(nav) = decision.downcast_ref::<NavigationPolicyDecision>() else {
            return false;
        };
        let Some(action) = nav.navigation_action() else {
            return false;
        };
        if matches!(action.navigation_type(), NavigationType::Other) {
            return false; // our own load_html / reload — allow.
        }
        decision.ignore();
        if let Some(uri) = action.request().and_then(|r| r.uri())
            && let Some(cb) = sink.borrow().as_ref()
        {
            cb(uri.to_string());
        }
        true
    });
}

/// Encode a string as a JS single-quoted string literal.
fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '<' => out.push_str("\\x3c"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}
