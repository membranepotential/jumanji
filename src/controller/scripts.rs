//! Shell viewport glue: the JavaScript every toolkit shell injects to drive
//! the document — scrolling, zoom-anchoring, selection copy, link hints,
//! reverse editor sync, the no-flash restore gate.
//!
//! This is DESIGN D12's sanctioned JS category, and only that category: never
//! content-pipeline JS (D3 forbids that — the document itself is rendered
//! 100% in Rust and stays CSP-locked, `default-src 'none'`). Every script here
//! is byte-identical on every toolkit; only how a shell *installs* a
//! `UserScript` and reads its postback differs (WebKitGTK's
//! `UserContentManager`, a WKWebView's `WKUserScript`, …).
//!
//! The one toolkit-specific surface these scripts would otherwise need is
//! `window.webkit.messageHandlers.<name>.postMessage(msg)`. WebKitGTK lets
//! a shell register handlers under any name there; a wry-hosted WKWebView
//! owns that object itself (its single `ipc` handler, reached as
//! `window.ipc.postMessage`) and registers no others. So every post here goes
//! through one indirection, [`POST_FN`], that each shell defines in its own
//! tiny prelude script before any of these run — never defined by these
//! scripts themselves.

use crate::controller::page::ZoomAnchor;
use crate::core::config::{DIAGRAM_FIT_CLASS, DIAGRAM_ZOOM_CLASS, DIAGRAM_ZOOM_VAR, WIDE_CLASS};

/// The function every shared script posts through:
/// `window.__jmnj_post(name, payload)`. Defined by each shell in its own
/// prelude — a document-start script installed *before* [`document_start`]'s
/// — never by these scripts. On WebKitGTK the prelude reads
/// `window.webkit.messageHandlers.jmnj.postMessage(name + ':' + payload)`
/// over a single registered handler; a wry/tao shell would define it over
/// `window.ipc.postMessage` instead. Either way these scripts never spell out
/// the toolkit's native bridge.
pub const POST_FN: &str = "__jmnj_post";

/// Message names the scripts post with, via [`POST_FN`]. One constant per
/// name so the Rust router and the JS caller can never drift apart.
pub mod message {
    /// The end of a pointer selection gesture (`mouseup`) with a non-empty
    /// selection — the payload is the selected text.
    pub const SELECTION: &str = "selection";
    /// A page scroll WebKit performed itself (wheel, touchpad, scrollbar) —
    /// the payload is `"<percent> <scrollY>"`.
    pub const SCROLL: &str = "scroll";
    /// The link-hint overlay's built label→href list — the payload is the
    /// `label\thref` lines, one per link, joined with `\n`.
    pub const HINTS: &str = "hints";
    /// A Ctrl+click reverse editor sync (DESIGN D7) — the payload is the
    /// clicked element's source line, as a decimal string.
    pub const EDITOR_SYNC: &str = "editorsync";
    /// The pointer entered or left a `.mermaid` box — the payload is the
    /// diagram's index in document order, as a decimal string, or `""` for
    /// "over no diagram".
    ///
    /// The controller caches this and routes the next `Ctrl`+wheel tick off the
    /// cached value. It has to be a *cached flag* rather than a query, because
    /// GTK dispatches the scroll capture-phase from the toplevel, before WebKit
    /// ever sees it (DESIGN D4): a `wheel` listener in the page would never
    /// fire, and asking the page per tick would put an IPC round trip inside the
    /// gesture. See `Controller::on_wheel_zoom`.
    pub const DIAGRAM_HOVER: &str = "diagramhover";
    /// A click on a document-graph item (DESIGN D14), or on a route node in
    /// the panel's breadcrumb — the payload is the item's key (`0.4.b`, see
    /// `core::graph::ItemKey`). A key, not an index: indices change with every
    /// re-layout, and a click can land after one it never saw.
    pub const GRAPH_SELECT: &str = "graphselect";
    /// A double-click on a document-graph item — the payload is its key.
    pub const GRAPH_OPEN: &str = "graphopen";
    /// A click on a node's fold handle (`+n` / `−`): select it and flip its
    /// fold — the payload is its key.
    pub const GRAPH_FOLD: &str = "graphfold";
}

/// Build a `window.__jmnj_post('<name>', <payload_expr>);` statement. Keeps
/// [`POST_FN`] and a `message::*` name from ever being hand-duplicated at a
/// call site.
fn post_call(name: &str, payload_expr: &str) -> String {
    format!("window.{POST_FN}('{name}', {payload_expr});")
}

/// JS that captures the anchor element into `window.__jmnj_anchor` (element +
/// its current viewport-top offset) for the given probe point. Paired with
/// [`RESTORE_ANCHOR_JS`], which runs after the zoom change reflows the page.
pub fn capture_anchor_js(anchor: &ZoomAnchor) -> String {
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
pub const RESTORE_ANCHOR_JS: &str = "(() => { const a = window.__jmnj_anchor; \
    if (a && a.el) { const nt = a.el.getBoundingClientRect().top; \
      window.scrollBy({ top: nt - a.top, left: 0, behavior: 'instant' }); } \
    window.__jmnj_anchor = null; \
    if (window.__jmnj_rebase) window.__jmnj_rebase(); })();";

/// The page global [`resize_anchor_js`] exposes to re-take its reading anchor
/// from the page as it is now. Called after every controller-driven anchored
/// change ([`RESTORE_ANCHOR_JS`]) and by the no-flash gate's reveal, the two
/// moments the position changes with no scroll event the tracker could trust.
const REBASE_GLOBAL: &str = "window.__jmnj_rebase";

/// Hold the reading position across a viewport resize (DESIGN D5a.0): a window
/// going fullscreen, an i3 re-tile, the zoom level changing the CSS viewport.
///
/// A resize is the one height change with no "before" the controller sees —
/// the shell learns of it after the engine has already re-laid the page out —
/// so the anchor cannot be captured on demand the way [`capture_anchor_js`]
/// is. Instead this script keeps one *continuously*: every scroll re-takes it,
/// and a `resize` scrolls it back to where it was. Resize steps run before
/// scroll steps in a rendering update, so the handler always sees the anchor
/// from before the reflow.
///
/// The anchor is the character under the probe, not the element: a resize
/// re-wraps prose, and pinning a long paragraph's *top* would move the line the
/// reader was on. The element (same probe points and exclusions as
/// [`capture_anchor_js`]) is the fallback where no text is under the probe —
/// a diagram, an image.
///
/// Scrolls the tracker causes itself (the restore, and the engine clamping a
/// document that got shorter) must not re-take the anchor, or a trip to
/// fullscreen and back at the end of a document would come back somewhere
/// else. `settled` holds the offset the last restore left; a scroll that lands
/// exactly there is ours. Every resize of a burst (fullscreen can deliver
/// several) restores from the same anchor.
///
/// An unscrolled page takes no anchor, so the top stays exactly the top. While
/// the no-flash gate hides the body, or the graph overlay covers it, the probe
/// would hit nothing useful, so the anchor is left as it was.
fn resize_anchor_js() -> String {
    format!(
        "(function () {{\n\
        let anchor = null, settled = null;\n\
        const probe = () => {{\n\
          const d = document.documentElement, b = document.body;\n\
          if (!b || d.classList.contains('{restoring}') \
              || document.getElementById('__jmnj_graph')) return;\n\
          anchor = null;\n\
          if (window.scrollY <= 0) return;\n\
          const m = document.querySelector('main') || b;\n\
          const r = m.getBoundingClientRect();\n\
          const cx = Math.max(1, Math.min(innerWidth - 1, r.left + r.width / 2));\n\
          for (const py of [8, 40, 80, 140]) {{\n\
            const c = document.elementFromPoint(cx, py);\n\
            if (!c || c === b || c === d || c.tagName === 'MAIN') continue;\n\
            const at = document.caretRangeFromPoint ? document.caretRangeFromPoint(cx, py) : null;\n\
            const n = at && at.startContainer;\n\
            if (n && n.nodeType === 3 && c.contains(n) && n.length > 0) {{\n\
              const range = document.createRange();\n\
              const o = Math.min(at.startOffset, n.length - 1);\n\
              const top = () => {{ range.setStart(n, o); range.setEnd(n, o + 1); \
                const rs = range.getClientRects(); \
                return rs.length ? rs[0].top : c.getBoundingClientRect().top; }};\n\
              anchor = {{ top: top, at: top() }};\n\
            }} else {{\n\
              anchor = {{ top: () => c.getBoundingClientRect().top, \
                          at: c.getBoundingClientRect().top }};\n\
            }}\n\
            return;\n\
          }}\n\
        }};\n\
        {rebase} = () => {{ probe(); settled = window.scrollY; }};\n\
        window.addEventListener('scroll', function () {{\n\
          if (settled !== null && Math.abs(window.scrollY - settled) < 1) return;\n\
          settled = null;\n\
          probe();\n\
        }}, {{ passive: true }});\n\
        window.addEventListener('resize', function () {{\n\
          if (!anchor) return;\n\
          window.scrollBy({{ top: anchor.top() - anchor.at, left: 0, behavior: 'instant' }});\n\
          settled = window.scrollY;\n\
        }});\n\
      }})();",
        restoring = RESTORING_CLASS,
        rebase = REBASE_GLOBAL,
    )
}

/// The nearest-`data-sourcepos` search, as a JS *expression* yielding the
/// element whose source line is the greatest at-or-before the line `line_expr`
/// evaluates to, or `null`.
///
/// `data-sourcepos` (comrak's, plus the pipeline's injected ones) opens with
/// `startLine:…`, so `parseInt` reads the start line directly; document order
/// makes those lines non-decreasing, so the last match ≤ the target is the
/// nearest block at-or-above it. Factored out because forward editor sync needs
/// it twice — once for a jump inside the loaded document
/// (`Page::goto_source_line`) and once from [`scroll_restore_js`], before the
/// document has ever painted — and two copies of a rule this fiddly would drift.
pub fn nearest_source_element_js(line_expr: &str) -> String {
    format!(
        "(t => {{ let best = null; \
           for (const el of document.querySelectorAll('[data-sourcepos]')) {{ \
             const l = parseInt(el.getAttribute('data-sourcepos'), 10); \
             if (!Number.isNaN(l) && l <= t) best = el; }} \
           return best; }})({line_expr})"
    )
}

/// The `<html>` attribute a shell's document loader writes the opening
/// position into, and [`scroll_restore_js`] reads it back out of.
pub const OPEN_ATTRIBUTE: &str = "data-jmnj-open";

/// The class that hides the body until the opening position has landed; the
/// rule lives in `core/assets/style.css` beside `html.dark`, the other
/// shell-toggled class.
pub const RESTORING_CLASS: &str = "jmnj-restoring";

/// Flip the wide-block breakout master class on `<html>` (DESIGN D5a).
///
/// The whole of `Action::ToggleWide`: the per-kind `jmnj-wide-<kind>` classes
/// the pipeline emitted say what *may* break out, and this one class says
/// whether it does — so the toggle costs a class flip and no re-render, and
/// nothing in the document moves vertically. The class name is
/// [`WIDE_CLASS`](crate::core::config::WIDE_CLASS), owned by core because the
/// pipeline emits it too (core cannot depend on the controller).
pub fn wide_class_js(on: bool) -> String {
    format!("document.documentElement.classList.toggle('{WIDE_CLASS}', {on});")
}

/// Flip the diagram fit-to-width class on `<html>` (DESIGN D5a.2).
///
/// The whole of `Action::ToggleDiagramFit`, and the same shape
/// [`wide_class_js`] has: one document-wide class the stylesheet reads, flipped
/// in place, so the toggle costs no re-render. The class name is
/// [`DIAGRAM_FIT_CLASS`](crate::core::config::DIAGRAM_FIT_CLASS), owned by core
/// because the pipeline emits it too.
pub fn diagram_fit_class_js(on: bool) -> String {
    format!("document.documentElement.classList.toggle('{DIAGRAM_FIT_CLASS}', {on});")
}

/// The smallest per-diagram Ctrl+wheel scale. Below a quarter of intrinsic a
/// diagram's labels are unreadable, so going further only loses the reader
/// their place.
pub const DIAGRAM_ZOOM_MIN: f64 = 0.25;

/// The largest per-diagram Ctrl+wheel scale. Eight times intrinsic is already
/// past the point where merman's vector output stops adding detail, and the
/// zoomed box (bounded height, both scrollers) gets harder to navigate the
/// larger the canvas inside it grows.
pub const DIAGRAM_ZOOM_MAX: f64 = 8.0;

/// Scale the `index`-th `.mermaid` box by `factor`, clamped to
/// [`DIAGRAM_ZOOM_MIN`]…[`DIAGRAM_ZOOM_MAX`].
///
/// **Multiplicative**, so successive ticks feel evenly spaced (an additive step
/// is a huge jump near 0.25 and imperceptible near 8) and so zooming out exactly
/// undoes zooming in. The current scale is read back off the element rather than
/// tracked in the session: it is transient DOM state that a reload is *supposed*
/// to drop (a look-closer gesture, unlike D5a's session-scoped page zoom), so
/// the DOM is its only honest home.
///
/// The box also gains [`DIAGRAM_ZOOM_CLASS`](crate::core::config::DIAGRAM_ZOOM_CLASS)
/// whenever the scale is off 1, which is what bounds its height and gives it
/// scrollers — a diagram at 400% must not make the document four times taller
/// or push the page sideways.
pub fn diagram_zoom_js(index: usize, factor: f64) -> String {
    format!(
        "(() => {{ const el = document.querySelectorAll('.mermaid')[{index}];            if (!el) return;            const cur = parseFloat(el.style.getPropertyValue('{DIAGRAM_ZOOM_VAR}'));            const base = Number.isFinite(cur) && cur > 0 ? cur : 1;            let next = base * {factor};            if (!Number.isFinite(next)) next = {DIAGRAM_ZOOM_MAX};            next = Math.min({DIAGRAM_ZOOM_MAX}, Math.max({DIAGRAM_ZOOM_MIN}, next));            el.style.setProperty('{DIAGRAM_ZOOM_VAR}', String(next));            el.classList.toggle('{DIAGRAM_ZOOM_CLASS}', Math.abs(next - 1) > 0.001); }})();"
    )
}

/// Clear every per-diagram scale, returning all diagrams to intrinsic width and
/// their boxes to unzoomed geometry.
///
/// Part of `=` (`Action::ZoomReset`): D5a makes `=` the reset for *both* zoom
/// axes, and leaving a diagram stuck at 4× after it would be a lie.
pub fn diagram_zoom_reset_js() -> String {
    format!(
        "document.querySelectorAll('.mermaid').forEach(el => {{ \
           el.style.removeProperty('{DIAGRAM_ZOOM_VAR}'); \
           el.classList.remove('{DIAGRAM_ZOOM_CLASS}'); }});"
    )
}

/// Wire the pointer-over-diagram flag: a capture-phase `mouseover` / `mouseout`
/// pair posts the index of the `.mermaid` box under the pointer (or `""` for
/// none) via [`message::DIAGRAM_HOVER`], and only when it changes.
///
/// This exists because the obvious implementation is impossible. GTK dispatches
/// scroll events capture-phase from the toplevel, *before* WebKit sees them
/// (DESIGN D4) — which is what makes `Ctrl`+wheel page zoom work at all — so a
/// `wheel` listener inside the page would never fire, and the routing decision
/// has to be made shell-side and synchronously. Posting the flag on pointer
/// movement (rare, and already coalesced by the change check) lets the
/// controller decide from a cached value with no round trip inside the gesture.
fn diagram_hover_js() -> String {
    format!(
        "(function () {{
        let last = null;
        const report = (v) => {{ if (v === last) return; last = v; {post} }};
        const boxOf = (node) => (node && node.closest ? node.closest('.mermaid') : null);
        const indexOf = (box) => {{
          const all = document.querySelectorAll('.mermaid');
          for (let i = 0; i < all.length; i++) if (all[i] === box) return String(i);
          return '';
        }};
        document.addEventListener('mouseover', function (e) {{
          const box = boxOf(e.target);
          report(box ? indexOf(box) : '');
        }}, true);
        document.addEventListener('mouseout', function (e) {{
          // Moving between two elements *inside* the same box is not a leave.
          if (boxOf(e.relatedTarget)) return;
          report('');
        }}, true);
      }})();",
        post = post_call(message::DIAGRAM_HOVER, "v")
    )
}

/// The page global the restore script records its first painted offset in; read
/// back by a shell's scroll-state snapshot into `ViewportState::first_frame_scroll_y`.
/// A load gets a fresh JS context, so it is `undefined` again on every document.
pub const FIRST_FRAME_GLOBAL: &str = "window.__jmnj_first_frame";

/// The page global the restore script records the reveal in: `{y, failsafe}`,
/// the scroll offset the body was *unhidden* at and whether the unconditional
/// timer was what unhid it. Read back by a shell's scroll-state snapshot into
/// `ViewportState::reveal_scroll_y` / `ViewportState::revealed_by_failsafe`.
pub const REVEAL_GLOBAL: &str = "window.__jmnj_reveal";

/// The page global the restore script parks its `apply` on, so the shell can
/// re-run *the same* placement once the load has fully finished (see
/// `Page::settle_initial_position`) without a second copy of the rules.
pub const APPLY_GLOBAL: &str = "window.__jmnj_apply_open";

/// Consecutive frames the document height must hold steady before the restore
/// loop concedes an offset is unreachable and reveals anyway.
///
/// `readyState === 'complete'` is not a statement that layout is final — the
/// height keeps growing after it while late boxes settle — and `apply` places
/// the position by clamping against the height it finds. Conceding on
/// `complete` alone therefore revealed the body at a clamped, near-top offset
/// for any deep position, which is the document-switch flash: the page appears
/// at ~0, then the shell's post-load settle then visibly jumps it down. Three
/// frames (~50ms at 60Hz) is enough to tell "still laying out" from "genuinely
/// too short", and the unconditional failsafe still bounds the whole affair.
pub const STABLE_FRAMES: u32 = 3;

/// The permanent document-start script that places every freshly loaded
/// document at the position a shell's document loader wrote into
/// [`OPEN_ATTRIBUTE`]. Installed once by every shell, as part of
/// [`document_start`].
///
/// This is the whole no-flash mechanism, and it is deliberately split in two:
/// the *position* travels as an inert `data-` attribute in the HTML, and the
/// *behaviour* is a shell user-script that never changes. Applying the position
/// from Rust after the load has finished is inherently too late — the document
/// has been parsed, laid out and composited at scroll 0 by then, and the
/// correction needs a further UI→web IPC hop, so the unscrolled top is on
/// screen for the whole of that window. That is the flash the reader reports
/// when walking the jumplist.
///
/// Why an attribute rather than an inline `<script>`: DESIGN D3 makes the
/// webview a *"dumb, static renderer … the same pipeline can later feed an
/// export path (PDF/HTML) or a different front end"*. A `data-` attribute is
/// inert markup that survives such an export untouched; a `<script>` would
/// break D3 *and* the page CSP (`default-src 'none'`, `core::pipeline`), which
/// has no `script-src`. WebKit user scripts are exempt from the page CSP —
/// which is precisely the sanctioned category this module's doc comment
/// already names: shell viewport glue, not content-pipeline JS.
///
/// The rest is timing. `requestAnimationFrame` callbacks run *before* the frame
/// they belong to is painted, so applying there means the first frame that
/// shows content already shows it at the right offset; the loop re-arms on
/// `DOMContentLoaded` and `load` for a document still growing (late-laid-out
/// images have no intrinsic size until then, so the document is shorter and
/// `scrollTo` clamps). It is bounded: it stops when the target is reached, or
/// after one final attempt once `readyState === 'complete'` — a document too
/// short to honour the offset must not leave an rAF chain spinning forever.
///
/// Belt and braces on top of the timing, [`RESTORING_CLASS`] hides the body
/// until the position lands, so *no* frame can show the wrong one even if a
/// paint sneaks in. The reveal is unconditional: it runs when the loop
/// finishes, and on a timer regardless, because a page left permanently blank
/// would be a far worse bug than the flash (CLAUDE.md: rendering failures
/// degrade gracefully, never a blank page). The hidden interval is normally a
/// single frame and shows the page's own `--bg`, so it is invisible.
pub fn scroll_restore_js() -> String {
    format!(
        "(function () {{\n\
           const root = document.documentElement;\n\
           // Injection is at document-start, which is after the document\n\
           // element exists — so the attribute the loader wrote is already\n\
           // readable here. No position, nothing to hide, nothing to do.\n\
           const spec = root.getAttribute('{attr}');\n\
           if (!spec) return;\n\
           const sep = spec.indexOf(':');\n\
           const kind = spec.slice(0, sep), arg = spec.slice(sep + 1);\n\
           root.classList.add('{cls}');\n\
           let revealed = false;\n\
           // `failsafe` records *why* the body was unhidden. The offset is read\n\
           // here and nowhere else: this is the frame the reader's eye first\n\
           // gets, so it — not the first laid-out frame, which the gate hides —\n\
           // is what an e2e must assert on to see a flash at all.\n\
           const reveal = (failsafe) => {{ if (revealed) return; \
             revealed = true; \
             {reveal_global} = {{ y: window.scrollY, failsafe: failsafe }}; \
             root.classList.remove('{cls}'); \
             if ({rebase}) {rebase}(); }};\n\
           // The failsafe, and the reason the gate is safe to have at all: it\n\
           // is not conditional on anything above working.\n\
           setTimeout(() => reveal(true), 400);\n\
           // Returns whether the position is now actually reached; anything\n\
           // short of that keeps the loop running.\n\
           const apply = () => {{\n\
             if (kind === 'offset') {{\n\
               const y = parseFloat(arg);\n\
               window.scrollTo(0, y);\n\
               return Math.abs(window.scrollY - y) < 0.5;\n\
             }}\n\
             if (kind === 'anchor') {{\n\
               const e = document.getElementById(arg);\n\
               if (!e) return false;\n\
               e.scrollIntoView();\n\
               return true;\n\
             }}\n\
             if (kind === 'line') {{\n\
               const best = {nearest};\n\
               if (!best) return false;\n\
               best.scrollIntoView({{behavior: 'instant', block: 'start'}});\n\
               return true;\n\
             }}\n\
             return true;\n\
           }};\n\
           // Parked for the shell's post-load settle, so the late-layout\n\
           // correction and the pre-paint placement are the same code.\n\
           {apply_global} = apply;\n\
           let done = false, pending = false, stable = 0, lastHeight = -1;\n\
           const schedule = () => {{ if (done || pending) return; \
             pending = true; requestAnimationFrame(tick); }};\n\
           function tick() {{\n\
             pending = false;\n\
             if (done) return;\n\
             // A body with no layout paints nothing, so such a frame is neither\n\
             // a chance to place the position nor one the reader could see it in.\n\
             const laidOut = document.body && document.body.getBoundingClientRect().height > 0;\n\
             let reached = false;\n\
             if (laidOut) {{\n\
               reached = apply();\n\
               // Read *after* applying: this is the offset this frame would\n\
               // paint with, and the first one is what the e2e asserts on.\n\
               if ({first} === undefined) {first} = window.scrollY;\n\
             }}\n\
             // Giving up is gated on the document having stopped GROWING, not\n\
             // merely on `readyState === 'complete'`. `apply` scrolls by\n\
             // clamping against the current height, so while the document is\n\
             // still laying out a deep offset clamps to near the top; revealing\n\
             // there is the flash — the body appears at ~0 and the post-load\n\
             // settle then visibly jumps it down. `complete` does not mean the\n\
             // height is final, so one extra frame of grace was far too little.\n\
             const h = document.documentElement.scrollHeight;\n\
             if (h === lastHeight) stable++; else {{ stable = 0; lastHeight = h; }}\n\
             if (reached || (document.readyState === 'complete' && stable >= {stable_frames})) {{\n\
               done = true;\n\
               // Next frame, so the frame that lands the position is still the\n\
               // hidden one and the first visible frame is already correct.\n\
               requestAnimationFrame(() => reveal(false));\n\
               return;\n\
             }}\n\
             schedule();\n\
           }}\n\
           schedule();\n\
           document.addEventListener('DOMContentLoaded', schedule);\n\
           window.addEventListener('load', schedule);\n\
         }})();",
        attr = OPEN_ATTRIBUTE,
        cls = RESTORING_CLASS,
        nearest = nearest_source_element_js("parseInt(arg, 10)"),
        apply_global = APPLY_GLOBAL,
        first = FIRST_FRAME_GLOBAL,
        reveal_global = REVEAL_GLOBAL,
        rebase = REBASE_GLOBAL,
        stable_frames = STABLE_FRAMES,
    )
}

/// Wire zathura-style copy-on-select: posts the current non-empty selection on
/// **`mouseup`** (the end of a pointer selection gesture) via
/// [`message::SELECTION`]. Keying off `mouseup` — not `selectionchange` — is
/// deliberate: WebKit's find highlight sets the DOM selection programmatically,
/// so a `selectionchange` listener would copy every search match (and each
/// `n`/`N` step), which is not what a copy-*on-select* feature should do. A
/// find never synthesises `mouseup`, so search leaves the clipboard alone. An
/// empty selection posts nothing, so a plain click (which collapses any
/// selection) never clobbers the clipboard with `""`.
fn selection_copy_js() -> String {
    format!(
        "(function () {{\n\
        document.addEventListener('mouseup', function () {{\n\
          const sel = window.getSelection ? window.getSelection().toString() : '';\n\
          if (sel && sel.length > 0) {{\n\
            {post}\n\
          }}\n\
        }}, true);\n\
      }})();",
        post = post_call(message::SELECTION, "sel")
    )
}

/// Wire the in-page scroll listener: a passive `scroll` listener, coalesced to
/// one report per animation frame and only firing when the *rounded* percent
/// changes, posts `"<percent> <scrollY>"` back to the shell via
/// [`message::SCROLL`]. This is the only signal for scrolls WebKit performs
/// itself (wheel, touchpad, scrollbar) — keyboard scrolls are shell-driven and
/// refresh the statusbar directly. The percent formula mirrors the shell's
/// scroll-state snapshot so the two agree. Posting `scrollY` alongside the
/// percent lets the shell update the statusbar directly from the payload — no
/// eval round trip back into the page just to ask for the number it already
/// has.
fn scroll_notify_js() -> String {
    format!(
        "(function () {{\n\
        let ticking = false, last = -1;\n\
        window.addEventListener('scroll', function () {{\n\
          if (ticking) return;\n\
          ticking = true;\n\
          requestAnimationFrame(function () {{\n\
            ticking = false;\n\
            const d = document.documentElement, b = document.body;\n\
            const max = (b.scrollHeight || d.scrollHeight) - window.innerHeight;\n\
            const p = max > 0 ? Math.min(100, Math.max(0, Math.round((window.scrollY / max) * 100))) : 0;\n\
            if (p !== last) {{\n\
              last = p;\n\
              {post}\n\
            }}\n\
          }});\n\
        }}, {{ passive: true }});\n\
      }})();",
        post = post_call(message::SCROLL, "p + ' ' + window.scrollY")
    )
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
/// the *rendering* pipeline; a shell already drives the page with JS). Posts
/// nothing — purely local DOM behaviour.
const DRAG_SELECT_RESET: &str = "(function () {\n\
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

/// Wire reverse editor sync (DESIGN D7): a capture-phase click listener that, on
/// a Ctrl + primary-button click, walks up from the target to the nearest
/// `[data-sourcepos]` ancestor and posts its source line back to the shell via
/// [`message::EDITOR_SYNC`]. It acts *only* on Ctrl+click (a plain click is
/// untouched, so link routing and text selection are unaffected), and swallows
/// the event so a Ctrl+click on a link syncs to the editor instead of following
/// the link.
fn editor_sync_js() -> String {
    format!(
        "(function () {{\n\
        document.addEventListener('click', function (e) {{\n\
          if (e.button !== 0 || !e.ctrlKey) return;\n\
          let el = e.target;\n\
          while (el && el.nodeType === 1) {{\n\
            if (el.hasAttribute('data-sourcepos')) {{\n\
              const line = parseInt(el.getAttribute('data-sourcepos'), 10);\n\
              if (!Number.isNaN(line) && line > 0) {{\n\
                e.preventDefault();\n\
                e.stopPropagation();\n\
                {post}\n\
              }}\n\
              return;\n\
            }}\n\
            el = el.parentElement;\n\
          }}\n\
        }}, true);\n\
      }})();",
        post = post_call(message::EDITOR_SYNC, "String(line)")
    )
}

/// The scripts every shell installs at document start in the top frame, in
/// this order, on every document: selection copy, drag-select reset, editor
/// sync, the pointer-over-diagram flag, scroll notify, the resize anchor, then
/// the scroll-restore no-flash gate. Order matters only in that scripts run in
/// insertion order; the gate calls the resize anchor's rebase if it exists, so
/// the anchor goes first. Otherwise each is independent of the others, and
/// this is simply the one canonical order every shell uses.
///
/// Not included: the shell's own [`POST_FN`] prelude (toolkit-specific, must
/// be installed *before* these) and [`hints_build_js`] (built on demand by
/// `Page::request_hints`, not installed once at document start).
pub fn document_start() -> Vec<String> {
    vec![
        selection_copy_js(),
        DRAG_SELECT_RESET.to_string(),
        editor_sync_js(),
        diagram_hover_js(),
        scroll_notify_js(),
        resize_anchor_js(),
        scroll_restore_js(),
    ]
}

/// The document-graph overlay (DESIGN D14): a function expression taking
/// `(svgMarkup, css, selected, post)`. Kept as a real `.js` file so it reads as
/// JavaScript; [`graph_show_js`] calls it.
const GRAPH_JS: &str = include_str!("assets/graph.js");

/// The overlay's stylesheet, injected with it rather than shipped in every
/// document's CSS: a document that never opens the graph pays nothing.
const GRAPH_CSS: &str = include_str!("assets/graph.css");

/// Draw the document graph over the page: `svg` from `core::graph`, with item
/// `selected` selected and centred.
pub fn graph_show_js(svg: &str, selected: usize) -> String {
    let post = format!(
        "{{select: key => {{ {} }}, open: key => {{ {} }}, fold: key => {{ {} }}}}",
        post_call(message::GRAPH_SELECT, "key"),
        post_call(message::GRAPH_OPEN, "key"),
        post_call(message::GRAPH_FOLD, "key"),
    );
    format!(
        "({GRAPH_JS})({svg}, {css}, {selected}, {post});",
        svg = js_string(svg),
        css = js_string(GRAPH_CSS),
    )
}

/// Swap the open graph's scene for `svg` (a re-layout): item `anchor` kept
/// where it was on screen, item `selected` selected.
pub fn graph_update_js(svg: &str, selected: usize, anchor: usize) -> String {
    graph_call_js(&format!("update({}, {selected}, {anchor})", js_string(svg)))
}

/// Call one method of the open graph overlay; a no-op when it is gone.
pub fn graph_call_js(call: &str) -> String {
    format!("if (window.__jmnj_graph) window.__jmnj_graph.{call};")
}

/// The overlay-building script for `Page::request_hints`. Finds visible
/// links, assigns home-row-alphabet labels (`a`..`z`, then `aa`,`ab`,… past 26),
/// draws a fixed-position tag over each, and posts the label→href map to the
/// shell via [`message::HINTS`].
pub fn hints_build_js() -> String {
    format!(
        "(() => {{\n\
    const old=document.getElementById('__jmnj_hints'); if(old) old.remove();\n\
    const vw=window.innerWidth, vh=window.innerHeight;\n\
    const links=Array.prototype.slice.call(document.querySelectorAll('a[href]')).filter(a=>{{\n\
      const r=a.getBoundingClientRect();\n\
      if(r.width<=0||r.height<=0) return false;\n\
      if(r.bottom<0||r.top>vh||r.right<0||r.left>vw) return false;\n\
      const s=getComputedStyle(a);\n\
      return s.visibility!=='hidden'&&s.display!=='none';\n\
    }});\n\
    const A='abcdefghijklmnopqrstuvwxyz', n=links.length, labels=[];\n\
    if(n<=A.length){{ for(let i=0;i<n;i++) labels.push(A[i]); }}\n\
    else {{ for(let i=0;i<A.length&&labels.length<n;i++) for(let j=0;j<A.length&&labels.length<n;j++) labels.push(A[i]+A[j]); }}\n\
    const overlay=document.createElement('div');\n\
    overlay.id='__jmnj_hints';\n\
    overlay.style.cssText='position:fixed;left:0;top:0;width:0;height:0;z-index:2147483647;';\n\
    const out=[];\n\
    links.forEach((a,i)=>{{\n\
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
    }});\n\
    document.documentElement.appendChild(overlay);\n\
    {post}\n\
  }})();",
        post = post_call(message::HINTS, "out.join('\\n')")
    )
}

/// Encode a string as a JS single-quoted string literal.
pub fn js_string(s: &str) -> String {
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
