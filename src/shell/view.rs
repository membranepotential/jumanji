//! The WebKit content view: a thin, typed wrapper around `WebView`.
//!
//! All document interaction the shell needs — loading, scrolling, anchor
//! jumps, zoom, find, recolor, placing a freshly loaded document at the
//! reading position it should open at — is exposed as small methods that
//! translate to `webkit6` calls and `window.*` JavaScript snippets. Content
//! itself is rendered 100% in Rust (see `core::pipeline`); JS here only drives
//! the viewport.

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

use crate::controller::scripts::{
    self, APPLY_GLOBAL, FIRST_FRAME_GLOBAL, OPEN_ATTRIBUTE, POST_FN, RESTORE_ANCHOR_JS,
    RESTORING_CLASS, REVEAL_GLOBAL, capture_anchor_js, hints_build_js, js_string,
    nearest_source_element_js,
};
use crate::core::RenderedDocument;
use crate::core::config::SelectionClipboard;

/// Where a reflow-preserving zoom keeps the reading position pinned. Defined
/// in [`scripts`] (toolkit-agnostic: [`scripts::capture_anchor_js`] takes it),
/// re-exported here because it is otherwise a `View`-only concern.
pub use crate::controller::scripts::ZoomAnchor;

/// A shell-supplied sink, installed after construction. Both link hints and
/// navigation routing hand a single string back to the shell (a JSON hint list
/// and a resolved target URI, respectively).
type Sink = Rc<RefCell<Option<Box<dyn Fn(String)>>>>;

/// Where a document opens: the reading position a load must land on *before*
/// its first painted frame.
///
/// One value per load, resolved by the shell where the load is initiated, so
/// the precedence between the three ways a position can be asked for is decided
/// exactly once instead of by the order of three `Option` fields: a `--forward`
/// line (the editor pointed at it explicitly) beats a link fragment, which
/// beats a remembered scroll offset. [`Top`](Self::Top) is not "no position" —
/// it is the position, and it needs no work, which is why it carries no data.
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
    /// (see [`scripts::scroll_restore_js`]); `-1` when the document carried no opening
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

/// Native WebView background painted behind the document, matched to the theme
/// so unpainted regions never flash a mismatched colour (light `#ffffff`,
/// dark `#1a1a1a` — the same values `style.css` uses for `--bg`).
const BG_LIGHT: RGBA = RGBA::WHITE;
const BG_DARK: RGBA = RGBA::new(0.101, 0.101, 0.101, 1.0);

/// The single script-message handler every shared script
/// ([`scripts::document_start`], [`hints_build_js`]) posts through, via the
/// `window.__jmnj_post` prelude installed in [`View::new`]. One handler
/// instead of one per message name: WebKitGTK is the toolkit that owns
/// `window.webkit.messageHandlers`, so the seam that lets a non-WebKit shell
/// share these scripts unmodified is exactly this — a single postMessage
/// point the GTK shell demultiplexes by name (see
/// [`View::connect_script_router`]).
const POST_HANDLER: &str = "jmnj";

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
    /// Called with the source line (as a string) of a Ctrl+clicked element, so
    /// the shell can spawn the editor (reverse sync, DESIGN D7).
    editor_sync_cb: Sink,
    /// Called with `"<percent> <scrollY>"` whenever the page scrolls by any
    /// means WebKit handles itself — wheel, touchpad, scrollbar drag — so the
    /// shell can refresh the statusbar percent without an eval round trip.
    scroll_cb: Sink,
}

impl View {
    pub fn new(selection_clipboard: SelectionClipboard) -> Self {
        let ucm = UserContentManager::new();
        let last_selection: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let hints_cb: Sink = Rc::new(RefCell::new(None));
        let editor_sync_cb: Sink = Rc::new(RefCell::new(None));
        let scroll_cb: Sink = Rc::new(RefCell::new(None));

        // register_script_message_handler returns false if the name is
        // already taken; on a fresh manager it always succeeds. The prelude
        // script below only reaches `postMessage` after this registers the
        // handler.
        ucm.register_script_message_handler(POST_HANDLER, None);
        // The prelude must be the *first* user script: scripts run in
        // insertion order, and every script in `scripts::document_start`
        // calls `window.__jmnj_post`, which this defines.
        install_document_start_script(&ucm, &post_prelude_js());
        for js in scripts::document_start() {
            install_document_start_script(&ucm, &js);
        }
        connect_message_router(
            &ucm,
            selection_clipboard,
            last_selection.clone(),
            hints_cb.clone(),
            editor_sync_cb.clone(),
            scroll_cb.clone(),
        );

        let webview = WebView::builder().user_content_manager(&ucm).build();
        // WebKitGTK copies the find match into PRIMARY as it selects it. `found-text`
        // fires after that write, so restoring PRIMARY here — to the user's last real
        // selection, or empty — reliably undoes it (see the field doc). This is the
        // only reliable hook: clearing the DOM selection does not retract the write,
        // and a plain post-find eval races WebKit and loses.
        if let Some(fc) = webview.find_controller() {
            let last = last_selection.clone();
            fc.connect_found_text(move |_, _| {
                if let Some(display) = gtk::gdk::Display::default() {
                    let text = last.borrow().clone().unwrap_or_default();
                    display.primary_clipboard().set_text(&text);
                }
            });
        }
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
            editor_sync_cb,
            scroll_cb,
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

    /// Install the shell's handler for a reverse-editor-sync Ctrl+click. The
    /// argument is the clicked element's source line, as a decimal string.
    pub fn set_editor_sync_handler(&self, f: impl Fn(String) + 'static) {
        *self.editor_sync_cb.borrow_mut() = Some(Box::new(f));
    }

    /// Install the shell's handler for native-scroll pings (wheel / touchpad /
    /// scrollbar). The argument is `"<percent> <scrollY>"`.
    pub fn set_scroll_handler(&self, f: impl Fn(String) + 'static) {
        *self.scroll_cb.borrow_mut() = Some(Box::new(f));
    }

    /// Forward editor sync (DESIGN D7) *within the loaded document*: scroll to
    /// the element nearest at-or-before `line` (see
    /// [`nearest_source_element_js`] for how "nearest" is decided), falling back
    /// to the top when the document has no source positions at all. A line the
    /// document must *open* at is not this — that is
    /// [`InitialPosition::SourceLine`], applied before the first frame.
    pub fn goto_source_line(&self, line: u32) {
        self.run_js(&format!(
            "{{ const best = {}; \
               if (best) best.scrollIntoView({{behavior: 'instant', block: 'start'}}); \
               else window.scrollTo(0, 0); }}",
            nearest_source_element_js(&line.to_string())
        ));
    }

    /// Load a rendered document, opening it at `at`. `base` is the source file;
    /// its URI becomes the base against which document-relative images resolve.
    /// `font_size_px` is the effective body font size when text zoom is off its
    /// 100% base, and `None` when it is not.
    ///
    /// Everything that decides what the first painted frame looks like rides
    /// *into* the load as markup on `<html>`, never after it: the `dark` class,
    /// the text-zoom `--font-size` (an inline style, which beats the
    /// stylesheet's `:root` rule exactly as [`View::set_text_zoom_px`]'s
    /// `style.setProperty` does), and the reading position (as
    /// [`OPEN_ATTRIBUTE`], which [`scripts::scroll_restore_js`] acts on before the first
    /// paint). Applying any of them from Rust once the load has finished is too
    /// late, and that window is what the reader sees as a flash of the
    /// unscrolled, base-size top of the page.
    ///
    /// All three go in one `replacen` on the opening tag — the same one-shot
    /// rewrite the `dark` class has always used. The position belongs *here*,
    /// in the shell, and not in `core::pipeline`: a viewport offset is not part
    /// of a document's rendering, and putting it in the pure core would breach
    /// the functional-core boundary.
    pub fn load_document(
        &self,
        doc: &RenderedDocument,
        base: &Path,
        at: &InitialPosition,
        font_size_px: Option<f64>,
    ) {
        let base_uri = gtk::gio::File::for_path(base).uri();
        let mut attrs = String::new();
        if self.dark.get() {
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
        self.webview.load_html(&html, Some(base_uri.as_str()));
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
    pub fn settle_initial_position(&self) {
        self.run_js(&format!("{APPLY_GLOBAL} && {APPLY_GLOBAL}();"));
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
        self.run_js(&hints_build_js());
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
    /// custom property on `<html>`, reflowing prose. This is the *interactive*
    /// path only; the inline style is lost when the document reloads, and
    /// [`View::load_document`] writes it back into the HTML rather than
    /// re-applying it afterwards (which would reflow the first painted frames
    /// from the base size up to the real one — a visible size jump on every
    /// reload).
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

    /// Search the document. WebKit highlights every match and selects the first;
    /// the `found-text` handler installed in [`View::new`] then restores PRIMARY,
    /// so the highlight stays but the match never lands on the clipboard.
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
        self.webview.evaluate_javascript(
            &script,
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
                        msup_shift_ratio: num("ms"),
                        fence_width: num("rw"),
                        frontmatter_width: num("fw"),
                        first_frame_scroll_y: num("ff"),
                        reveal_scroll_y: num("rv"),
                        revealed_by_failsafe: v
                            .object_get_property("rt")
                            .is_some_and(|b| b.to_boolean()),
                        restoring: v.object_get_property("rs").is_some_and(|b| b.to_boolean()),
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
                    msup_shift_ratio: 0.0,
                    fence_width: 0.0,
                    frontmatter_width: 0.0,
                    // Matches the in-page sentinel: nothing was recorded.
                    first_frame_scroll_y: -1.0,
                    reveal_scroll_y: -1.0,
                    revealed_by_failsafe: false,
                    restoring: false,
                    fn_color: String::new(),
                }),
            },
        );
    }

    /// Scroll to an absolute offset (px) in the *already loaded* document — a
    /// quickmark jump, or a jumplist hop that stays inside this document. An
    /// offset that has to survive a load is [`InitialPosition::Offset`] instead,
    /// which lands before the first frame rather than after it.
    pub fn restore_scroll(&self, y: f64) {
        self.run_js(&format!("window.scrollTo(0, {y});"));
    }
}

/// The prelude every shared [`scripts::document_start`] script funnels
/// through: defines `window.__jmnj_post` over the single [`POST_HANDLER`]
/// script-message handler by joining the message name and payload into one
/// string, since a WebKit script-message channel carries exactly one. This is
/// the seam DESIGN's macOS-port research names: WKWebView-based toolkits own
/// `window.webkit.messageHandlers` themselves, so a script that wants to be
/// byte-identical on every toolkit cannot call it directly — it calls
/// [`scripts::POST_FN`] instead, and only this prelude (never a shared
/// script) knows how that maps onto WebKitGTK's bridge.
fn post_prelude_js() -> String {
    format!(
        "window.{POST_FN} = (name, payload) => \
         window.webkit.messageHandlers.{POST_HANDLER}.postMessage(name + ':' + payload);"
    )
}

/// Install `source` as a permanent document-start, top-frame user script —
/// the shape every [`scripts::document_start`] script (and the
/// [`post_prelude_js`] prelude) is installed with.
fn install_document_start_script(ucm: &UserContentManager, source: &str) {
    let script = UserScript::new(
        source,
        UserContentInjectedFrames::TopFrame,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    );
    ucm.add_script(&script);
}

/// Register [`POST_HANDLER`] and route every message the shared scripts post
/// through it to exactly what four separate WebKitGTK handlers did before
/// this seam existed: `selection` writes the configured clipboard and
/// remembers the last real selection (so a later search can restore PRIMARY
/// after WebKit clobbers it with the find match — see the `found-text` hook
/// in [`View::new`]); `scroll` and `hints` forward their payload to the
/// shell-installed sink; `editorsync` does too, behind the same empty-payload
/// guard the old handler had. An unrecognised name is ignored.
///
/// The payload is split on the *first* `:` only — [`scripts::message::HINTS`]
/// posts `label\thref` lines, which themselves contain newlines and tabs, so
/// only the separator the prelude inserted may be significant.
fn connect_message_router(
    ucm: &UserContentManager,
    target: SelectionClipboard,
    last_selection: Rc<RefCell<Option<String>>>,
    hints_cb: Sink,
    editor_sync_cb: Sink,
    scroll_cb: Sink,
) {
    ucm.connect_script_message_received(Some(POST_HANDLER), move |_, value| {
        let payload = value.to_str();
        let Some((name, msg)) = payload.split_once(':') else {
            return;
        };
        match name {
            scripts::message::SELECTION => {
                if msg.is_empty() {
                    return;
                }
                *last_selection.borrow_mut() = Some(msg.to_string());
                if let Some(display) = gtk::gdk::Display::default() {
                    let clipboard = match target {
                        SelectionClipboard::Primary => display.primary_clipboard(),
                        SelectionClipboard::Clipboard => display.clipboard(),
                    };
                    clipboard.set_text(msg);
                }
            }
            scripts::message::SCROLL => {
                if let Some(cb) = scroll_cb.borrow().as_ref() {
                    cb(msg.to_string());
                }
            }
            scripts::message::HINTS => {
                if let Some(cb) = hints_cb.borrow().as_ref() {
                    cb(msg.to_string());
                }
            }
            scripts::message::EDITOR_SYNC => {
                if msg.is_empty() {
                    return;
                }
                if let Some(cb) = editor_sync_cb.borrow().as_ref() {
                    cb(msg.to_string());
                }
            }
            _ => {}
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

/// Encode a string as the body of a double-quoted HTML attribute value.
///
/// Local rather than borrowed from `core::highlight`: the shell writing an
/// attribute into the tag it is itself rewriting is shell business, and reaching
/// into a private core rendering module for eight lines would couple the layers
/// for nothing. Same reasoning as [`scripts::js_string`], the JS-literal
/// counterpart — moved to [`scripts`] because every toolkit shell needs it to
/// build the same eval'd snippets `View` does here (e.g. `scroll_to_anchor`).
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
