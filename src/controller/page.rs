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
    APPLY_GLOBAL, FIRST_FRAME_GLOBAL, OPEN_ATTRIBUTE, POINTER_GLOBAL, RESTORING_CLASS,
    REVEAL_GLOBAL, anchored_js, capture_anchor_js, diagram_fit_class_js, diagram_zoom_js,
    diagram_zoom_reset_js, graph_call_js, graph_show_js, graph_update_js, hints_build_js,
    js_string, nearest_source_element_js, rebase_anchor_js, restore_anchor_js, wide_class_js,
};
use crate::controller::toolkit::Viewport;
use crate::core::RenderedDocument;
use crate::core::pipeline::{HTML_CLASS_OPEN, HTML_OPEN};

/// Where a reflow-preserving change keeps the reading position pinned.
///
/// Both zoom axes and the block toggles reflow the page, so an anchor — a
/// point of content, the character there when there is text — is captured
/// before the change and put back at the same place on screen after. One
/// mechanism ([`capture_anchor_js`] + [`restore_anchor_js`]), parameterised by
/// the probe point.
#[derive(Clone, Copy)]
pub enum ZoomAnchor {
    /// Keep the line at the top of the viewport at the same height (keyboard /
    /// D-Bus zoom, text zoom, the block toggles). Only anchors when scrolled,
    /// so an exact top stays exactly at the top.
    Top,
    /// Keep the content under the pointer under it, on both axes — `Ctrl`+wheel
    /// zoom ("zoom towards the cursor"), including inside a horizontal
    /// scroller. The pointer as the page itself last saw it, for the reason
    /// [`GraphZoomAt::Pointer`] gives: no position from outside the page is in
    /// CSS px.
    Pointer,
}

/// The largest step one graph zoom takes, either way. The overlay keeps its
/// scale within 0.08 – 3 (`graph.js`), so no step past ×37.5 can change
/// anything; this only keeps the number finite.
pub const GRAPH_ZOOM_FACTOR: f64 = 100.0;

/// Which point a graph zoom keeps fixed on screen (DESIGN D14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphZoomAt {
    /// The pointer, as the page itself last saw it (`Ctrl`+wheel). Not a
    /// position the controller passes in: the shell's pointer is in toolkit
    /// logical px, and WebKitGTK can lay the page out at its own screen scale
    /// on top of the page zoom (seen: 2 logical px per CSS px at zoom 1), so no
    /// conversion from outside is safe. The page's `clientX/clientY` is CSS px
    /// by definition.
    Pointer,
    /// The selection's centre on screen (`+` / `-`): the keys zoom where the
    /// keyboard's attention is (docs/graph/interaction.md, "Zoom").
    Selection,
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
    /// Rendered width of the first `.mermaid` *box* — the scroll container, not
    /// the diagram inside it (0 if none). CSS px. The observable for the
    /// wide-block breakout (DESIGN D5a): it equals the reading column's content
    /// box normally and the window minus the gutter when the breakout is on, so
    /// an e2e can assert the diagram really got more usable width.
    pub diagram_box_width: f64,
    /// What is at the top of the reading column right now, and where: the
    /// first 48 characters of the element the anchor probe would pick, and that
    /// element's viewport-relative `top` in CSS px.
    ///
    /// The observable for "the reader's place did not move". Any change that
    /// alters a block's *height* — the wide-block breakout, diagram fit,
    /// per-diagram zoom, both zoom axes — shifts everything after it while
    /// `scrollY` stays put, so the only honest check is that the same content
    /// is still at the same offset afterwards. `scroll_y` cannot say that (it
    /// is *supposed* to change) and `scroll_percent` is a lossy proxy.
    pub probe_text: String,
    /// See [`probe_text`](Self::probe_text).
    pub probe_top: f64,
    /// What is under the pointer, as the page itself tracks it: the word
    /// there (whitespace-delimited) and the viewport-relative `top` of the
    /// character, or — over no text — the element's first 48 characters and
    /// its `top`. The observable for "`Ctrl`+wheel keeps what is under the
    /// cursor in place" (DESIGN D5a): a word, not its paragraph, because the
    /// paragraph's top holding still says nothing about the line under a
    /// pointer low in it. Empty / 0 over nothing.
    pub pointer_text: String,
    /// See [`pointer_text`](Self::pointer_text).
    pub pointer_top: f64,
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
    /// The document graph's selected item on screen, in CSS px: its pill's
    /// centre (`-1` when no graph is open), width and height (0). The
    /// observable for "a zoom about the pointer keeps what is under it in
    /// place" (DESIGN D14), and — through the size — for "the graph's scale
    /// changed". The height is the vertical scale exactly; the width stops
    /// shrinking at the horizontal scale's floor.
    pub graph_sel_x: f64,
    pub graph_sel_y: f64,
    pub graph_sel_width: f64,
    pub graph_sel_height: f64,
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
    gb: f64,
    mw: f64,
    ms: f64,
    rw: f64,
    fw: f64,
    pt: String,
    py: f64,
    qt: String,
    qy: f64,
    fc: String,
    ff: f64,
    rv: f64,
    rt: bool,
    rs: bool,
    sx: f64,
    sy: f64,
    sw: f64,
    sh: f64,
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
            diagram_box_width: s.gb,
            math_width: s.mw,
            msup_shift_ratio: s.ms,
            fence_width: s.rw,
            frontmatter_width: s.fw,
            probe_text: s.pt,
            probe_top: s.py,
            pointer_text: s.qt,
            pointer_top: s.qy,
            first_frame_scroll_y: s.ff,
            reveal_scroll_y: s.rv,
            revealed_by_failsafe: s.rt,
            restoring: s.rs,
            fn_color: s.fc,
            graph_sel_x: s.sx,
            graph_sel_y: s.sy,
            graph_sel_width: s.sw,
            graph_sel_height: s.sh,
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
            diagram_box_width: 0.0,
            math_width: 0.0,
            msup_shift_ratio: 0.0,
            fence_width: 0.0,
            frontmatter_width: 0.0,
            probe_text: String::new(),
            probe_top: 0.0,
            pointer_text: String::new(),
            pointer_top: 0.0,
            // Matches the in-page sentinel: nothing was recorded.
            first_frame_scroll_y: -1.0,
            reveal_scroll_y: -1.0,
            revealed_by_failsafe: false,
            restoring: false,
            fn_color: String::new(),
            graph_sel_x: -1.0,
            graph_sel_y: -1.0,
            graph_sel_width: 0.0,
            graph_sel_height: 0.0,
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
        if let Some(px) = font_size_px {
            attrs.push_str(&format!(" style=\"--font-size: {px}px\""));
        }
        if let Some(open) = at.open_attribute() {
            // An anchor id is document-supplied, so it is escaped like any other
            // attribute value; it can no more break out of the quotes than a
            // heading's text can.
            attrs.push_str(&format!(" {OPEN_ATTRIBUTE}=\"{}\"", html_attribute(&open)));
        }
        // The recolor class joins the class list the pipeline already emitted
        // (the wide-block classes) rather than a second `class` attribute — a
        // duplicate would be dropped by the parser, taking the breakout with it.
        let dark_class = if dark { "dark " } else { "" };
        let html = if attrs.is_empty() && dark_class.is_empty() {
            doc.html.clone()
        } else {
            doc.html.replacen(
                &format!("{HTML_OPEN}{HTML_CLASS_OPEN}"),
                &format!("{HTML_OPEN}{attrs}{HTML_CLASS_OPEN}{dark_class}"),
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

    /// Draw the document graph (DESIGN D14) over the page, item `selected`
    /// selected and centred. The overlay stamps its posts with `generation`.
    fn show_graph(&self, svg: &str, selected: usize, generation: u64) {
        self.eval(&graph_show_js(svg, selected, generation));
    }

    /// Replace the open graph's scene after a re-layout: item `anchor` (the
    /// one the reader acted on) stays where it is on screen, and item
    /// `selected` is selected.
    fn graph_update(&self, svg: &str, selected: usize, anchor: usize) {
        self.eval(&graph_update_js(svg, selected, anchor));
    }

    /// Move the graph's selection to item `index`, panning it into view.
    fn graph_select(&self, index: usize) {
        self.eval(&graph_call_js(&format!("select({index})")));
    }

    /// Scale the graph by `factor`, keeping the point `at` names fixed.
    ///
    /// `factor` is clamped to [`GRAPH_ZOOM_FACTOR`]'s range first: a huge
    /// count makes it overflow to `inf` (or `0`), which formats into the
    /// script as something JavaScript cannot parse.
    fn graph_zoom(&self, factor: f64, at: GraphZoomAt) {
        let factor = factor.clamp(1.0 / GRAPH_ZOOM_FACTOR, GRAPH_ZOOM_FACTOR);
        let at = match at {
            GraphZoomAt::Pointer => "'pointer'",
            GraphZoomAt::Selection => "'selection'",
        };
        self.eval(&graph_call_js(&format!("zoom({factor}, {at})")));
    }

    /// Back to 1:1, on the current node.
    fn graph_reset(&self) {
        self.eval(&graph_call_js("reset()"));
    }

    /// Remove the graph overlay, and re-take the resize anchor from the page
    /// it uncovers: the page may have moved behind the overlay (an editor
    /// jump), and the anchor could not be probed through it.
    fn hide_graph(&self) {
        self.eval(&format!(
            "{}{}",
            graph_call_js("close()"),
            rebase_anchor_js()
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
    /// this is a transaction across two evals: capture the anchor (async JS,
    /// replying with its token), and only in the completion callback set the
    /// native zoom and restore *that token's* anchor. Transactions can overlap
    /// — a coalesced `Ctrl`+wheel burst issues the next capture before the
    /// previous restore has run — and each still restores its own anchor.
    fn zoom_to(&self, level: f64, anchor: ZoomAnchor) {
        let level = level.max(0.2);
        let view = self.clone();
        self.eval_json(&capture_anchor_js(&anchor), move |token| {
            view.set_zoom_level(level);
            view.eval(&restore_anchor_js(anchor_token(token)));
        });
    }

    /// Reset both zoom axes to 100% *and* every per-diagram Ctrl+wheel scale,
    /// anchored once at the top of the viewport. A single capture spans all
    /// three changes so their combined reflow is corrected together rather than
    /// three anchors fighting over it.
    ///
    /// The diagram scales belong here rather than in a call of their own for
    /// exactly that reason, and they belong in `=` at all because D5a makes it
    /// the reset for zoom as a whole — a diagram left at 4× after a reset would
    /// be a lie.
    fn reset_zoom(&self, font_base_px: f64) {
        let view = self.clone();
        let clear_diagrams = diagram_zoom_reset_js();
        self.eval_json(&capture_anchor_js(&ZoomAnchor::Top), move |token| {
            view.set_zoom_level(1.0);
            view.eval(&format!(
                "document.documentElement.style.setProperty('--font-size', '{font_base_px}px');\
                 {clear_diagrams}{}",
                restore_anchor_js(anchor_token(token))
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
        self.eval(&anchored_js(
            &ZoomAnchor::Top,
            &format!("document.documentElement.style.setProperty('--font-size', '{px}px');"),
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

    /// Apply the wide-block breakout state: flip the master class on `<html>`
    /// (DESIGN D5a.1).
    ///
    /// **Anchored.** No re-render happens — the rule is already in the
    /// stylesheet — but "no re-render" is not "nothing moves": widening a block
    /// changes its *height* (a table re-wraps at the new measure, a code block
    /// loses wrapped rows), so every block after it shifts while `scrollY`
    /// stays put, and the reader's place slides out from under them. Same
    /// capture → apply → restore as [`Page::set_text_zoom_px`]; pure JS, so it
    /// is one eval.
    fn set_wide(&self, wide: bool) {
        self.eval(&anchored_js(&ZoomAnchor::Top, &wide_class_js(wide)));
    }

    /// Apply the diagram fit-to-width state: flip the fit class on `<html>`
    /// (DESIGN D5a.2).
    ///
    /// **Anchored**, and more obviously so than [`Page::set_wide`]: fitting
    /// scales a diagram down by whatever factor its box demands, and an SVG
    /// with a `viewBox` scales its height by the same factor — a tall diagram
    /// can shed hundreds of pixels, pulling everything below it up past the
    /// reader's eye. Capture → apply → restore, one eval.
    fn set_diagram_fit(&self, fit: bool) {
        self.eval(&anchored_js(&ZoomAnchor::Top, &diagram_fit_class_js(fit)));
    }

    /// Scale the `index`-th diagram by `factor` (multiplicative, clamped in the
    /// script). The per-diagram half of `Ctrl`+wheel: the controller routes the
    /// tick here instead of to [`Page::zoom_to`] when the pointer is over a
    /// diagram.
    ///
    /// **Anchored at the cursor**, like the page zoom this tick would otherwise
    /// have driven. A scaled diagram mostly scrolls inside its own box, but the
    /// box itself is only height-capped while a scale is applied: the step off
    /// 1.0 caps it and the step back to 1.0 uncaps it, and either way the box's
    /// height changes and the page below it moves. Anchoring at the pointer
    /// keeps what is under the cursor under the cursor.
    ///
    /// Still deliberately *not* coalesced — a style write on one element is
    /// nothing like the full-page reflow a geometric zoom step costs.
    fn zoom_diagram(&self, index: usize, factor: f64, anchor: ZoomAnchor) {
        self.eval(&anchored_js(&anchor, &diagram_zoom_js(index, factor)));
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
             const dia = document.querySelector('.mermaid'); \
             const svg = dia ? dia.querySelector('svg') : null; \
             const math = document.querySelector('math'); \
             const rf = document.querySelector('.rendered-fence svg'); \
             const fm = document.querySelector('.frontmatter'); \
             const fn = document.querySelector('.entity.name.function.python'); \
             const msup = document.querySelector('math msup'); \
             const gs = document.querySelector('#__jmnj_graph .jg-node.sel rect'); \
             const gr = gs ? gs.getBoundingClientRect() : null; \
             const mr = m.getBoundingClientRect(); \
             /* The reading-position probe. Mirrors `capture_anchor_js` exactly \
                — same probe points, same exclusions — so what it reports is \
                precisely the element the anchor would pin. Falling back to \
                `main` would be worse than useless: main's box starts at the top \
                of the document, so its `top` is just `-scrollY` and the check \
                would silently invert into 'scrollY did not change', which is the \
                opposite of the invariant (DESIGN D5a.0). */ \
             let pn = null; \
             const pcx = Math.max(1, Math.min(innerWidth - 1, mr.left + mr.width / 2)); \
             for (const ppy of [8, 40, 80, 140]) { \
               const c = document.elementFromPoint(pcx, ppy); \
               if (c && c !== b && c !== d && c.tagName !== 'MAIN') { pn = c; break; } } \
             /* An SVG <path> identifies nothing by text; name it by tag instead \
                of climbing, which would walk right up into main. */ \
             const label = (n) => { \
               const t = (n.textContent || '').replace(/\\s+/g, ' ').trim(); \
               return t ? t.slice(0, 48) : '<' + n.tagName.toLowerCase() + '>'; }; \
             const pt = pn ? label(pn) : ''; \
             const py = pn ? pn.getBoundingClientRect().top : 0; \
             let ms = 0; \
             if (msup && msup.children.length >= 2) { \
               const bb = msup.children[0].getBoundingClientRect(); \
               const sb = msup.children[1].getBoundingClientRect(); \
               if (bb.height > 0) ms = (bb.top - sb.top) / bb.height; } \
             return { y: window.scrollY, p: Math.min(100, Math.max(0, p)), \
                      w: m.offsetWidth, vw: window.innerWidth, \
                      dw: Math.max(d.scrollWidth, b.scrollWidth), \
                      gw: svg ? svg.getBoundingClientRect().width : 0, \
                      gb: dia ? dia.getBoundingClientRect().width : 0, \
                      mw: math ? math.getBoundingClientRect().width : 0, \
                      ms: ms, \
                      pt: pt, py: py, \
                      rw: rf ? rf.getBoundingClientRect().width : 0, \
                      fw: fm ? fm.getBoundingClientRect().width : 0, \
                      fc: fn ? getComputedStyle(fn).color : '', \
                      sx: gr ? gr.left + gr.width / 2 : -1, \
                      sy: gr ? gr.top + gr.height / 2 : -1, \
                      sw: gr ? gr.width : 0, \
                      sh: gr ? gr.height : 0";
        let script = format!(
            "{HEAD}, ff: typeof {FIRST_FRAME_GLOBAL} === 'number' \
             ? {FIRST_FRAME_GLOBAL} : -1, \
             rv: {REVEAL_GLOBAL} ? {REVEAL_GLOBAL}.y : -1, \
             rt: {REVEAL_GLOBAL} ? {REVEAL_GLOBAL}.failsafe : false, \
             rs: d.classList.contains('{RESTORING_CLASS}'), \
             ...(() => {{ const q = {POINTER_GLOBAL} ? {POINTER_GLOBAL}() : null; \
               const n = q ? document.elementFromPoint(q.x, q.y) : null; \
               if (!n) return {{ qt: '', qy: 0 }}; \
               const at = document.caretRangeFromPoint \
                 ? document.caretRangeFromPoint(q.x, q.y) : null; \
               const tn = at && at.startContainer; \
               if (tn && tn.nodeType === 3 && n.contains(tn) && tn.length > 0) {{ \
                 const s = tn.data, ws = /\\s/; \
                 let o = Math.min(at.startOffset, s.length - 1); \
                 if (ws.test(s[o]) && o > 0) o -= 1; \
                 let i = o, j = o; \
                 while (i > 0 && !ws.test(s[i - 1])) i -= 1; \
                 while (j < s.length && !ws.test(s[j])) j += 1; \
                 const r = document.createRange(); r.setStart(tn, o); r.setEnd(tn, o + 1); \
                 const rs = r.getClientRects(); \
                 if (i < j) return {{ qt: s.slice(i, j), \
                   qy: rs.length ? rs[0].top : n.getBoundingClientRect().top }}; }} \
               return {{ qt: label(n), qy: n.getBoundingClientRect().top }}; }})() }}; }})()"
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
    ///
    /// Re-takes the resize anchor at the new offset in the same eval. A
    /// quickmark set at another zoom level changes the native zoom just before
    /// this, and the `resize` that zoom fires runs before the jump's `scroll`
    /// event in the next rendering update — so without the re-take the resize
    /// anchor still holds the pre-jump place and scrolls the page back to it.
    fn restore_scroll(&self, y: f64) {
        self.eval(&format!("window.scrollTo(0, {y});{}", rebase_anchor_js()));
    }
}

impl<V: Viewport + Clone + 'static> Page for V {}

/// The anchor token a capture replied with — `None` when it captured nothing
/// (`null`) or the reply was lost, which the restore treats alike.
fn anchor_token(reply: Option<String>) -> Option<u64> {
    reply.and_then(|json| serde_json::from_str::<Option<u64>>(&json).ok().flatten())
}

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
