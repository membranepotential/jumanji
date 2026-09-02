//! The shared viewport behaviour: everything the reader does to a document
//! that is JS composition rather than an engine primitive.
//!
//! [`Page`] is an extension trait over [`Viewport`] with a blanket impl, so
//! every toolkit gets scrolling, link hints, zoom anchoring, the document-load
//! HTML rewrite and the state snapshot *by construction* — implement the six
//! primitives in [`Viewport`] and the behaviour follows, byte-identical, on
//! GTK and on anything else. Content itself is rendered 100% in Rust (see
//! `core::pipeline`); the JS here only drives the viewport (DESIGN D12).

use std::path::Path;

use serde::Deserialize;

use crate::controller::scripts::{
    APPLY_GLOBAL, FIRST_FRAME_GLOBAL, OPEN_ATTRIBUTE, RESTORE_ANCHOR_JS, RESTORING_CLASS,
    REVEAL_GLOBAL, capture_anchor_js, hints_build_js, js_string, nearest_source_element_js,
};
use crate::controller::toolkit::Viewport;
use crate::core::RenderedDocument;

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

/// Where a document opens: the reading position a load must land on *before*
/// its first painted frame.
///
/// One value per load, resolved by the controller where the load is initiated,
/// so the precedence between the three ways a position can be asked for is
/// decided exactly once instead of by the order of three `Option` fields: a
/// `--forward` line (the editor pointed at it explicitly) beats a link
/// fragment, which beats a remembered scroll offset. [`Top`](Self::Top) is not
/// "no position" — it is the position, and it needs no work, which is why it
/// carries no data.
#[derive(Debug, Clone, PartialEq)]
pub enum InitialPosition {
    /// The top of the document — a first read, or a jump that asked for it.
    Top,
    /// An absolute scroll offset in CSS px (history, jumplist, live reload).
    Offset(f64),
    /// A heading id from a link fragment (`other.md#section`, `[[Note#H]]`).
    Anchor(String),
    /// A source line (`--forward`), resolved to the nearest block at-or-above.
    SourceLine(u32),
}

impl InitialPosition {
    /// Serialize into the `data-jmnj-open` attribute value the restore script
    /// parses, or `None` when the document needs no placing at all.
    ///
    /// One tagged attribute rather than one attribute per variant, so the
    /// markup cannot express two positions at once any more than the enum can.
    /// The value is split on its *first* colon, so an anchor id containing one
    /// survives; [`Top`](Self::Top) and a non-positive offset both emit
    /// nothing, because scroll 0 is where a document already opens and hiding
    /// the page to "restore" it would be pure cost.
    fn open_attribute(&self) -> Option<String> {
        match self {
            InitialPosition::Top => None,
            InitialPosition::Offset(y) if *y > 0.0 => Some(format!("offset:{y}")),
            InitialPosition::Offset(_) => None,
            InitialPosition::Anchor(id) => Some(format!("anchor:{}", id.trim_start_matches('#'))),
            InitialPosition::SourceLine(line) => Some(format!("line:{line}")),
        }
    }
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
    /// Vertical superscript shift of the first `<msup>`, as a fraction of the
    /// base box height: `(base.top − sup.top) / base.height` (0 if no msup). A
    /// sane superscript sits a little above the base top, so this is a small
    /// positive number (< 1). The `mathjax2`-shadowing bug drove it to ~6 (the
    /// superscript flung line-heights above the base); the e2e asserts it stays
    /// well under one base-height.
    pub msup_shift_ratio: f64,
    /// Rendered width of the first external-renderer output (`.rendered-fence
    /// svg`), 0 if none. CSS px. Lets e2e assert a configured fence renderer
    /// (DESIGN D6.2) actually produced visible output.
    pub fence_width: f64,
    /// Rendered width of the shown frontmatter panel (`.frontmatter`), 0 when
    /// the document has none *or* it is hidden — which is the default. Lets e2e
    /// assert the `:frontmatter` toggle from outside the page (DESIGN D11).
    pub frontmatter_width: f64,
    /// The scroll offset the *first painted frame* of the current document was
    /// placed at, as recorded from inside the page by the restore user-script
    /// (see [`scroll_restore_js`](crate::controller::scripts::scroll_restore_js)); `-1` when the document carried no opening
    /// position ([`InitialPosition::Top`]) or nothing has been painted yet.
    ///
    /// The cheap in-process observable for the no-flash property — the final
    /// offset was always right, the bug was the frame before it — so an e2e
    /// reading back `0` here after returning to a document last read half-way
    /// down is watching the page place its top first. It measures placement,
    /// not visibility: the `jmnj-restoring` gate means that frame was hidden
    /// anyway, which is exactly why the *visual* proof is the frame-capture
    /// harness and this is the regression sentinel.
    pub first_frame_scroll_y: f64,
    /// The scroll offset the body was **revealed** at — the first frame the
    /// reader can actually see — or `-1` while still hidden / for a document
    /// that opened at the top and installed no restore script.
    ///
    /// The companion to [`first_frame_scroll_y`](Self::first_frame_scroll_y),
    /// and the sharper of the two: the gate hides the early frames, so a
    /// document that is still growing legitimately paints its first (hidden)
    /// frames at a clamped, near-top offset. Only the offset at reveal says
    /// whether the reader saw the flash.
    pub reveal_scroll_y: f64,
    /// Whether the reveal came from the unconditional 400 ms failsafe rather
    /// than from the position being reached. `true` means the page was unhidden
    /// wherever it happened to be — correct as a last resort (a blank page is
    /// worse), and the thing a restore-gate e2e must assert did *not* happen.
    pub revealed_by_failsafe: bool,
    /// Whether `<html>` still carries `jmnj-restoring`, i.e. the body is still
    /// hidden waiting for its opening position. Transient by design — it must
    /// be `false` on any settled document, and an e2e that finds it `true` has
    /// caught the one failure mode the hide-until-restored gate can introduce:
    /// a page left permanently blank.
    pub restoring: bool,
    /// Computed `color` of the first python function-name span
    /// (`.entity.name.function.python`), as a CSS `rgb(...)` string ("" if the
    /// document has no python code). Lets e2e assert the dark-mode syntax-CSS
    /// scoping fix: this must not be near-black (`rgb(50, 50, 50)`, i.e.
    /// `InspiredGithub`'s light colour) once dark mode is on.
    pub fn_color: String,
}

/// The snapshot as it comes back over [`Viewport::eval_json`]: the short keys
/// the state IIFE builds, one field per key.
///
/// `#[serde(default)]` reproduces the old per-property read exactly — a key the
/// page did not produce reads as `0` / `""` / `false`, not as a failed parse.
/// Only a wholly absent or unparseable *result* falls back to the sentinel
/// state in [`ViewportState::unavailable`].
#[derive(Deserialize, Default)]
#[serde(default)]
struct Snapshot {
    y: f64,
    p: f64,
    w: f64,
    vw: f64,
    dw: f64,
    gw: f64,
    mw: f64,
    ms: f64,
    rw: f64,
    fw: f64,
    fc: String,
    ff: f64,
    rv: f64,
    rt: bool,
    rs: bool,
}

impl From<Snapshot> for ViewportState {
    fn from(s: Snapshot) -> Self {
        ViewportState {
            scroll_y: s.y,
            scroll_percent: s.p.clamp(0.0, 100.0) as u32,
            content_width: s.w,
            viewport_width: s.vw,
            doc_scroll_width: s.dw,
            diagram_width: s.gw,
            math_width: s.mw,
            msup_shift_ratio: s.ms,
            fence_width: s.rw,
            frontmatter_width: s.fw,
            first_frame_scroll_y: s.ff,
            reveal_scroll_y: s.rv,
            revealed_by_failsafe: s.rt,
            restoring: s.rs,
            fn_color: s.fc,
        }
    }
}

impl ViewportState {
    /// The state reported when the page could not be asked at all (the eval
    /// failed, or its value had no JSON form): everything zero except the two
    /// restore observables, which carry the same `-1` sentinel the in-page
    /// script uses for "nothing was recorded".
    fn unavailable() -> Self {
        ViewportState {
            scroll_y: 0.0,
            scroll_percent: 0,
            content_width: 0.0,
            viewport_width: 0.0,
            doc_scroll_width: 0.0,
            diagram_width: 0.0,
            math_width: 0.0,
            msup_shift_ratio: 0.0,
            fence_width: 0.0,
            frontmatter_width: 0.0,
            // Matches the in-page sentinel: nothing was recorded.
            first_frame_scroll_y: -1.0,
            reveal_scroll_y: -1.0,
            revealed_by_failsafe: false,
            restoring: false,
            fn_color: String::new(),
        }
    }
}

/// Everything the reader does to a document that is not an engine primitive.
///
/// A blanket-implemented extension trait, so no toolkit can accidentally get a
/// *different* scroll step, hint alphabet or restore gate: implement
/// [`Viewport`] and this comes with it.
///
/// [`Clone`] is a supertrait because the anchored zoom paths sequence a native
/// call *inside* an eval callback — `zoom_to` captures a handle to the viewport
/// so it can set the native zoom level once the anchor has been captured — and
/// `'static` because that callback outlives the call.
pub trait Page: Viewport + Clone + 'static {
    /// Forward editor sync (DESIGN D7) *within the loaded document*: scroll to
    /// the element nearest at-or-before `line` (see
    /// [`nearest_source_element_js`] for how "nearest" is decided), falling back
    /// to the top when the document has no source positions at all. A line the
    /// document must *open* at is not this — that is
    /// [`InitialPosition::SourceLine`], applied before the first frame.
    fn goto_source_line(&self, line: u32) {
        self.eval(&format!(
            "{{ const best = {}; \
               if (best) best.scrollIntoView({{behavior: 'instant', block: 'start'}}); \
               else window.scrollTo(0, 0); }}",
            nearest_source_element_js(&line.to_string())
        ));
    }

    /// Load a rendered document, opening it at `at`. `base` is the source file;
    /// document-relative images resolve against its directory.
    /// `font_size_px` is the effective body font size when text zoom is off its
    /// 100% base, and `None` when it is not.
    ///
    /// Everything that decides what the first painted frame looks like rides
    /// *into* the load as markup on `<html>`, never after it: the `dark` class,
    /// the text-zoom `--font-size` (an inline style, which beats the
    /// stylesheet's `:root` rule exactly as [`Page::set_text_zoom_px`]'s
    /// `style.setProperty` does), and the reading position (as
    /// [`OPEN_ATTRIBUTE`], which [`scroll_restore_js`](crate::controller::scripts::scroll_restore_js) acts on before the first
    /// paint). Applying any of them from Rust once the load has finished is too
    /// late, and that window is what the reader sees as a flash of the
    /// unscrolled, base-size top of the page.
    ///
    /// All three go in one `replacen` on the opening tag — the same one-shot
    /// rewrite the `dark` class has always used. The position belongs *here*,
    /// in the controller, and not in `core::pipeline`: a viewport offset is not
    /// part of a document's rendering, and putting it in the pure core would
    /// breach the functional-core boundary.
    fn load_document(
        &self,
        doc: &RenderedDocument,
        base: &Path,
        at: &InitialPosition,
        dark: bool,
        font_size_px: Option<f64>,
    ) {
        let mut attrs = String::new();
        if dark {
            attrs.push_str(" class=\"dark\"");
        }
        if let Some(px) = font_size_px {
            attrs.push_str(&format!(" style=\"--font-size: {px}px\""));
        }
        if let Some(open) = at.open_attribute() {
            // An anchor id is document-supplied, so it is escaped like any other
            // attribute value; it can no more break out of the quotes than a
            // heading's text can.
            attrs.push_str(&format!(" {OPEN_ATTRIBUTE}=\"{}\"", html_attribute(&open)));
        }
        let html = if attrs.is_empty() {
            doc.html.clone()
        } else {
            doc.html.replacen(
                "<html lang=\"en\">",
                &format!("<html lang=\"en\"{attrs}>"),
                1,
            )
        };
        self.load_html(&html, base);
    }

    /// Re-run the opening placement once the load has fully finished — the final
    /// authority on where the document sits.
    ///
    /// The pre-paint pass runs while images without intrinsic dimensions have
    /// not laid out yet, so the document is shorter than it will be and a deep
    /// offset clamps. This corrects that. It reads as a small settle rather than
    /// a jump from the top precisely because the content was already revealed
    /// near the right place. It re-runs the script's *own* `apply`
    /// ([`APPLY_GLOBAL`]) rather than a second copy of the rules, is idempotent
    /// by construction, and no-ops on a document that opened at the top (no
    /// attribute ⇒ the script returned early and never parked anything).
    fn settle_initial_position(&self) {
        self.eval(&format!("{APPLY_GLOBAL} && {APPLY_GLOBAL}();"));
    }

    fn scroll_by(&self, dx: i64, dy: i64) {
        // `behavior: 'instant'` pins the zathura-instant semantics regardless of
        // the engine's smooth-scrolling setting: a repeated key must never
        // restart an in-flight scroll animation.
        self.eval(&format!(
            "window.scrollBy({{left: {dx}, top: {dy}, behavior: 'instant'}});"
        ));
    }

    /// Scroll by a fraction of the viewport height (half-page navigation).
    fn scroll_half_page(&self, down: bool, times: u32) {
        let sign = if down { 1.0 } else { -1.0 };
        self.eval(&format!(
            "window.scrollBy({{top: {sign} * (window.innerHeight / 2) * {times}, behavior: 'instant'}});"
        ));
    }

    fn scroll_to_top(&self) {
        self.eval("window.scrollTo(0, 0);");
    }

    fn scroll_to_bottom(&self) {
        self.eval("window.scrollTo(0, document.body.scrollHeight);");
    }

    /// Scroll a heading anchor into view. Accepts `#id` or a bare `id`.
    fn scroll_to_anchor(&self, anchor: &str) {
        let id = anchor.trim_start_matches('#');
        self.eval(&format!(
            "{{ const e = document.getElementById({}); if (e) e.scrollIntoView(); }}",
            js_string(id)
        ));
    }

    /// Build the link-hint overlay: label every visible `<a href>` with a
    /// home-row-alphabet tag and post the `[{label,href}]` list back to the
    /// controller via the `hints` message. `href` is the *resolved* absolute
    /// URI, so the routing sees the same value a real click would.
    fn request_hints(&self) {
        self.eval(&hints_build_js());
    }

    /// Narrow the visible hints to those whose label starts with `typed`.
    fn filter_hints(&self, typed: &str) {
        self.eval(&format!(
            "(() => {{ const o=document.getElementById('__jmnj_hints'); if(!o) return; \
               const t={typed}; \
               for (const el of o.querySelectorAll('.__jmnj_hint')) {{ \
                 el.style.display = el.getAttribute('data-label').indexOf(t)===0 ? '' : 'none'; }} }})();",
            typed = js_string(typed)
        ));
    }

    /// Remove the hint overlay.
    fn clear_hints(&self) {
        self.eval(
            "(() => { const o=document.getElementById('__jmnj_hints'); if(o) o.remove(); })();",
        );
    }

    /// Geometric zoom without anchoring: set the engine's full-page native
    /// zoom. The native zoom level is a property of the view and survives a
    /// document reload, so this is used where the reading position is restored
    /// by other means — quickmark/history restores, which set the scroll offset
    /// explicitly. Diagrams scale with zoom by construction: the engine
    /// multiplies their pinned CSS width (`--dw`) into device px (see
    /// `style.css`).
    fn set_zoom(&self, level: f64) {
        self.set_zoom_level(level.max(0.2));
    }

    /// Geometric zoom anchored at `anchor`. Because zoom now reflows the page,
    /// the reading position drifts unless pinned.
    ///
    /// Setting the zoom level is a native call and cannot be issued from JS, so
    /// the sequence is race-free by construction: capture the anchor (async JS),
    /// and only in its completion callback set the native zoom and restore the
    /// position (a second JS eval). The two evals share `window.__jmnj_anchor`
    /// and can never interleave for one call, since the second is scheduled from
    /// the first's callback.
    fn zoom_to(&self, level: f64, anchor: ZoomAnchor) {
        let level = level.max(0.2);
        let view = self.clone();
        let capture = capture_anchor_js(&anchor);
        self.eval_json(&capture, move |_| {
            view.set_zoom_level(level);
            view.eval(RESTORE_ANCHOR_JS);
        });
    }

    /// Reset both zoom axes to 100%, anchored once at the top of the viewport.
    /// A single capture spans both changes (geometric + text) so the reflow from
    /// each is corrected together rather than fighting two anchors.
    fn reset_zoom(&self, font_base_px: f64) {
        let view = self.clone();
        let capture = capture_anchor_js(&ZoomAnchor::Top);
        self.eval_json(&capture, move |_| {
            view.set_zoom_level(1.0);
            view.eval(&format!(
                "document.documentElement.style.setProperty('--font-size', '{font_base_px}px');\
                 {RESTORE_ANCHOR_JS}"
            ));
        });
    }

    /// Text zoom: set the effective body font size (px) via the `--font-size`
    /// custom property on `<html>`, reflowing prose. This is the *interactive*
    /// path only; the inline style is lost when the document reloads, and
    /// [`Page::load_document`] writes it back into the HTML rather than
    /// re-applying it afterwards (which would reflow the first painted frames
    /// from the base size up to the real one — a visible size jump on every
    /// reload).
    ///
    /// Reflow moves content, so the top-of-viewport anchor is captured before the
    /// change and the position restored after. Pure JS (no native call), so
    /// capture → apply → restore fit in one eval — the same anchoring mechanism
    /// the geometric zoom uses, just applied inline.
    fn set_text_zoom_px(&self, px: f64) {
        let capture = capture_anchor_js(&ZoomAnchor::Top);
        self.eval(&format!(
            "{capture}\
             document.documentElement.style.setProperty('--font-size', '{px}px');\
             {RESTORE_ANCHOR_JS}"
        ));
    }

    /// Apply the recolor state: toggle the `dark` class on `<html>` (matching
    /// the pipeline's recolor CSS) and switch the native background so
    /// unpainted regions match the theme.
    fn set_dark(&self, dark: bool) {
        self.set_background_dark(dark);
        self.eval(&format!(
            "document.documentElement.classList.toggle('dark', {dark});"
        ));
    }

    /// Query the current scroll offset (px), delivering it to `callback` on the
    /// main loop. Used to preserve position across a reload.
    fn scroll_position<F: FnOnce(f64) + 'static>(&self, callback: F) {
        self.eval_json("window.scrollY", move |json| {
            let y = json
                .and_then(|s| serde_json::from_str::<f64>(&s).ok())
                .unwrap_or(0.0);
            callback(y);
        });
    }

    /// Snapshot the viewport in one JS round-trip, delivering a [`ViewportState`]
    /// to `callback`. Used by the D-Bus `GetState` method (and the statusbar) so
    /// a single reply reflects one consistent snapshot. The extra widths let
    /// tests assert the reflow invariants: `doc_scroll_width ≤ viewport_width`
    /// (no page h-scroll) and diagram device growth (`diagram_width × zoom`).
    fn scroll_state<F: FnOnce(ViewportState) + 'static>(&self, callback: F) {
        // Split so the first-frame global has a single spelling: everything up
        // to `fc` is fixed text, and the last field interpolates the name the
        // restore script writes.
        const HEAD: &str = "(() => { const d = document.documentElement, b = document.body; \
             const max = (b.scrollHeight || d.scrollHeight) - window.innerHeight; \
             const p = max > 0 ? Math.round((window.scrollY / max) * 100) : 0; \
             const m = document.querySelector('main') || b; \
             const svg = document.querySelector('.mermaid svg'); \
             const math = document.querySelector('math'); \
             const rf = document.querySelector('.rendered-fence svg'); \
             const fm = document.querySelector('.frontmatter'); \
             const fn = document.querySelector('.entity.name.function.python'); \
             const msup = document.querySelector('math msup'); \
             let ms = 0; \
             if (msup && msup.children.length >= 2) { \
               const bb = msup.children[0].getBoundingClientRect(); \
               const sb = msup.children[1].getBoundingClientRect(); \
               if (bb.height > 0) ms = (bb.top - sb.top) / bb.height; } \
             return { y: window.scrollY, p: Math.min(100, Math.max(0, p)), \
                      w: m.offsetWidth, vw: window.innerWidth, \
                      dw: Math.max(d.scrollWidth, b.scrollWidth), \
                      gw: svg ? svg.getBoundingClientRect().width : 0, \
                      mw: math ? math.getBoundingClientRect().width : 0, \
                      ms: ms, \
                      rw: rf ? rf.getBoundingClientRect().width : 0, \
                      fw: fm ? fm.getBoundingClientRect().width : 0, \
                      fc: fn ? getComputedStyle(fn).color : ''";
        let script = format!(
            "{HEAD}, ff: typeof {FIRST_FRAME_GLOBAL} === 'number' \
             ? {FIRST_FRAME_GLOBAL} : -1, \
             rv: {REVEAL_GLOBAL} ? {REVEAL_GLOBAL}.y : -1, \
             rt: {REVEAL_GLOBAL} ? {REVEAL_GLOBAL}.failsafe : false, \
             rs: d.classList.contains('{RESTORING_CLASS}') }}; }})()"
        );
        self.eval_json(&script, move |json| {
            let state = json
                .and_then(|s| serde_json::from_str::<Snapshot>(&s).ok())
                .map_or_else(ViewportState::unavailable, ViewportState::from);
            callback(state);
        });
    }

    /// Scroll to an absolute offset (px) in the *already loaded* document — a
    /// quickmark jump, or a jumplist hop that stays inside this document. An
    /// offset that has to survive a load is [`InitialPosition::Offset`] instead,
    /// which lands before the first frame rather than after it.
    fn restore_scroll(&self, y: f64) {
        self.eval(&format!("window.scrollTo(0, {y});"));
    }
}

impl<V: Viewport + Clone + 'static> Page for V {}

/// Encode a string as the body of a double-quoted HTML attribute value.
///
/// Local rather than borrowed from `core::highlight`: the controller writing an
/// attribute into the tag it is itself rewriting is not rendering, and reaching
/// into a private core rendering module for eight lines would couple the layers
/// for nothing. Same reasoning as [`js_string`], the JS-literal
/// counterpart.
fn html_attribute(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}
