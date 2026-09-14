# macOS WKWebView spike

The evidence behind `docs/research/05-macos-port.md` §3: a standalone binary
that checks, against the real system WKWebView through wry + tao, every
assumption the macOS shell (issue #1) rests on. Rerun it whenever wry, tao or
macOS moves — the focus-handoff result in particular is version-dependent
(tao#208, tao#940, wry#184).

```sh
cd docs/research/spikes/macos-wkwebview
cargo run
```

Opens a window, runs every check unattended in about four seconds, prints a
`PASS`/`FAIL`/`INFO` line per check, exits. Not part of the jumanji package
(the empty `[workspace]` in `Cargo.toml` keeps cargo from adopting it);
Linux CI never sees it.

## What it checks

| Check | Contract it backs |
|---|---|
| A — init script + IPC round trip: `window.__jmnj_post(name, payload)` over `window.ipc.postMessage`, posted at document start | the shared user scripts in `controller/scripts.rs` post through `__jmnj_post`; the mac shell defines it in an init script |
| B — `evaluate_script_with_callback` delivers a JSON-serialized object, routed back to the main thread via the tao `EventLoopProxy` | `Viewport::eval_json`, and the `Send`-callback parking pattern `toolkit.rs` describes |
| F — a custom protocol serving the document and a document-relative image; plus whether that `jmnj://` origin may load an absolute `file://` image | one of the two `Viewport::load_html` options, and its cost |
| D — CSS Custom Highlight API present and a `Highlight` over a `Range` registers | the JS find-in-page (`Viewport::find*`) |
| C — `WebView::zoom` reaches `WKWebView.pageZoom` and reflows the CSS viewport | `Viewport::set_zoom_level` |
| E — focus handoff: is the webview `NSWindow.firstResponder` before any click, and does a synthesized `NSEvent` keyDown sent through `NSApp.sendEvent` reach the in-page capture-phase `keydown` listener? | the capture-phase key path (DESIGN D4 on macOS) and `Viewport::focus` |
| G1 — control: objc `loadHTMLString:baseURL:` with a `file://` base; a relative image that exists only on disk, and an absolute `file://` image in a *different* directory | the other `Viewport::load_html` option, and whether read access is confined to the base dir |
| G2 — control: wry's own `load_html`, which has no base-URL parameter | why G1 needs the objc handle |

`controls/filebase.swift` is the same G1/G2 question asked without wry or
objc2 — a raw `WKWebView` on an untouched `WKWebViewConfiguration`, in Swift,
so the answer cannot be an artefact of the Rust stack:
`xcrun swift controls/filebase.swift src/img.png`.

## Results — 2026-09-14

macOS 26.6.2 (WebKit 605.1.15), wry 0.57.0, tao 0.37.0, rustc 1.90.0.

```
PASS  A ipc-roundtrip  init script posted via window.ipc, readyState=loading
PASS  B eval-json  object round-tripped as JSON: p=42 s="str" y=0
PASS  F custom-protocol  under jmnj://doc/: relative img.naturalWidth=1 (served by the protocol); absolute file:// img.naturalWidth=0
PASS  D css-highlight  Highlight + CSS.highlights present=true, registered highlight count=1
PASS  E focus (JS)  document.hasFocus()=true before any click
PASS  C zoom  innerWidth 900 -> 600 (ratio 1.500), WKWebView.pageZoom=1.5
PASS  E focus (AppKit)  firstResponder = WryWebView, inside webview = true — before any click or focus() call
PASS  E key-routing  in-page capture listener received key="j" (cold: no click, no focus() call)
INFO  G1 file:// base via objc loadHTMLString:baseURL:  relative img.naturalWidth=1, absolute file:// (other dir) img.naturalWidth=1
INFO  G2 wry load_html (no base)  relative img.naturalWidth=0, absolute file:// img.naturalWidth=0, baseURI="about:blank"
```

The Swift control (`controls/filebase.swift`, raw WKWebView, default
configuration) agrees: `rel=1 abs=1 missing=0` with a `file://` base;
`rel=0 abs=0 missing=0` with `baseURL: nil`. The `missing=0` row is the
control of the control — `naturalWidth` is 0 for anything that did not load.

Two things the run settled beyond "the assumptions hold":

- **`file://` base URLs work on macOS, and are the only origin that can
  reach local files.** `loadHTMLString:baseURL:` with a `file://` base loads
  document-relative *and* absolute `file://` images, from any directory —
  exactly WebKitGTK's behaviour. The "no local file read" claim (§3 of the
  research doc, from secondary sources) is about iOS / App-Sandboxed apps.
  From a non-file origin (`about:blank`, and the `jmnj://` custom protocol)
  an absolute `file://` image is blocked, so a custom-protocol shell would
  have to proxy *every* local resource, absolute paths included. What wry
  lacks is an API to pass the base — `WebView::load_html` takes only the
  string — so the mac shell's `load_html` is either one objc call through
  `WebViewExtMacOS` (G1) or a custom protocol that rewrites and serves all
  local references (F). wry sets no file-access preference on the
  configuration (checked in `wry/src/wkwebview/mod.rs`), so this is
  WKWebView's default. Tested unsandboxed only.
- **Focus handoff was clean on this version pair.** The webview is first
  responder as soon as the window is up and keys route without a click. The
  shell should still call `Viewport::focus()` on window activation; the
  tao/wry focus bugs are version-dependent and this is one data point.
