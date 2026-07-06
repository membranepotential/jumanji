//! The WebKit content view: a thin, typed wrapper around `WebView`.
//!
//! All document interaction the shell needs — loading, scrolling, anchor
//! jumps, zoom, find, recolor, scroll-position round-tripping for reload — is
//! exposed as small methods that translate to `webkit6` calls and `window.*`
//! JavaScript snippets. Content itself is rendered 100% in Rust (see
//! `core::pipeline`); JS here only drives the viewport.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use gtk::gdk::RGBA;
use gtk::prelude::*;
use webkit6::prelude::*;
use webkit6::{
    FindController, FindOptions, UserContentInjectedFrames, UserContentManager, UserScript,
    UserScriptInjectionTime, WebView,
};

use crate::core::RenderedDocument;
use crate::core::config::SelectionClipboard;

/// Native WebView background painted behind the document, matched to the theme
/// so unpainted regions never flash a mismatched colour (light `#ffffff`,
/// dark `#1a1a1a` — the same values `style.css` uses for `--bg`).
const BG_LIGHT: RGBA = RGBA::WHITE;
const BG_DARK: RGBA = RGBA::new(0.101, 0.101, 0.101, 1.0);

/// The script-message handler name the selection user-script posts to.
const SELECTION_HANDLER: &str = "selection";

#[derive(Clone)]
pub struct View {
    webview: WebView,
    /// The desired recolor (dark) state, tracked so `load_document` can pre-apply
    /// the `dark` class on `<html>` and paint dark from the very first frame.
    dark: Rc<Cell<bool>>,
}

impl View {
    pub fn new(selection_clipboard: SelectionClipboard) -> Self {
        let ucm = UserContentManager::new();
        install_selection_copy(&ucm, selection_clipboard);

        let webview = WebView::builder().user_content_manager(&ucm).build();
        webview.set_vexpand(true);
        webview.set_hexpand(true);
        webview.set_background_color(&BG_LIGHT);

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
            settings.set_enable_smooth_scrolling(true);
        }

        Self {
            webview,
            dark: Rc::new(Cell::new(false)),
        }
    }

    pub fn widget(&self) -> &WebView {
        &self.webview
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
        self.run_js(&format!("window.scrollBy({dx}, {dy});"));
    }

    /// Scroll by a fraction of the viewport height (half-page navigation).
    pub fn scroll_half_page(&self, down: bool, times: u32) {
        let sign = if down { 1.0 } else { -1.0 };
        self.run_js(&format!(
            "window.scrollBy(0, {sign} * (window.innerHeight / 2) * {times});"
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

    /// Geometric zoom: webkit full-page zoom (scales everything, diagrams
    /// included, because `zoom-text-only` is off by default). The level is
    /// mirrored into the `--zoom` custom property so the stylesheet can keep
    /// the layout width constant under zoom (no reflow; see `style.css`).
    /// Like text zoom, the property is re-applied after each load.
    pub fn set_zoom(&self, level: f64) {
        let level = level.max(0.2);
        self.webview.set_zoom_level(level);
        self.run_js(&format!(
            "document.documentElement.style.setProperty('--zoom', {level});"
        ));
    }

    pub fn zoom_level(&self) -> f64 {
        self.webview.zoom_level()
    }

    /// Text zoom: set the effective body font size (px) via the `--font-size`
    /// custom property on `<html>`, reflowing prose without touching layout
    /// geometry or diagram sizing. Re-applied after each load (the inline style
    /// is lost when the document reloads).
    ///
    /// Reflow moves content, so the element at the top of the viewport is
    /// captured before the change and scrolled back into view after — the
    /// reading position stays anchored instead of jumping.
    pub fn set_text_zoom_px(&self, px: f64) {
        self.run_js(&format!(
            "(() => {{
                let anchor = null;
                if (window.scrollY > 0) {{
                    const m = document.querySelector('main') || document.body;
                    const r = m.getBoundingClientRect();
                    const cx = Math.max(1, Math.min(innerWidth - 1, r.left + r.width / 2));
                    for (const py of [8, 40, 80, 140]) {{
                        const c = document.elementFromPoint(cx, py);
                        if (c && c !== document.body && c !== document.documentElement && c !== m) {{
                            anchor = c;
                            break;
                        }}
                    }}
                }}
                document.documentElement.style.setProperty('--font-size', '{px}px');
                if (anchor) anchor.scrollIntoView({{ block: 'start' }});
            }})();"
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

    /// Query the vertical scroll percentage (0..=100), delivering it to
    /// `callback`. Returns 0 for documents shorter than the viewport.
    pub fn scroll_percent<F: FnOnce(u32) + 'static>(&self, callback: F) {
        let script = "(() => { const d = document.documentElement, b = document.body; \
             const max = (b.scrollHeight || d.scrollHeight) - window.innerHeight; \
             return max > 0 ? Math.round((window.scrollY / max) * 100) : 0; })()";
        self.webview.evaluate_javascript(
            script,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |res| {
                let p = res.ok().map(|v| v.to_double()).unwrap_or(0.0);
                callback(p.clamp(0.0, 100.0) as u32);
            },
        );
    }

    /// Query scroll offset (px), percentage (0..=100) and the content column's
    /// layout width (CSS px) in one JS round-trip, delivering all three to
    /// `callback`. Used by the D-Bus `GetState` method so a single reply
    /// reflects a single, consistent viewport snapshot. The layout width is in
    /// CSS px, so it must stay *constant* under geometric zoom (the no-reflow
    /// invariant of D5a) — tests assert on exactly that.
    pub fn scroll_state<F: FnOnce(f64, u32, f64) + 'static>(&self, callback: F) {
        let script = "(() => { const d = document.documentElement, b = document.body; \
             const max = (b.scrollHeight || d.scrollHeight) - window.innerHeight; \
             const p = max > 0 ? Math.round((window.scrollY / max) * 100) : 0; \
             const m = document.querySelector('main') || b; \
             return { y: window.scrollY, p: Math.min(100, Math.max(0, p)), \
                      w: m.offsetWidth }; })()";
        self.webview.evaluate_javascript(
            script,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |res| match res {
                Ok(v) => {
                    let y = v.object_get_property("y").map_or(0.0, |n| n.to_double());
                    let p = v.object_get_property("p").map_or(0.0, |n| n.to_double());
                    let w = v.object_get_property("w").map_or(0.0, |n| n.to_double());
                    callback(y, p.clamp(0.0, 100.0) as u32, w);
                }
                Err(_) => callback(0.0, 0, 0.0),
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
