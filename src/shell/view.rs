//! The WebKit content view: a thin, typed wrapper around `WebView`.
//!
//! All document interaction the shell needs — loading, scrolling, anchor
//! jumps, zoom, find, recolor, scroll-position round-tripping for reload — is
//! exposed as small methods that translate to `webkit6` calls and `window.*`
//! JavaScript snippets. Content itself is rendered 100% in Rust (see
//! `core::pipeline`); JS here only drives the viewport.

use std::path::Path;

use gtk::prelude::*;
use webkit6::prelude::*;
use webkit6::{FindController, FindOptions, WebView};

use crate::core::RenderedDocument;

#[derive(Clone)]
pub struct View {
    webview: WebView,
}

impl View {
    pub fn new() -> Self {
        let webview = WebView::new();
        webview.set_vexpand(true);
        webview.set_hexpand(true);

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

        Self { webview }
    }

    pub fn widget(&self) -> &WebView {
        &self.webview
    }

    /// Load a rendered document. `base` is the source file; its URI becomes the
    /// base against which document-relative images resolve.
    pub fn load_document(&self, doc: &RenderedDocument, base: &Path) {
        let base_uri = gtk::gio::File::for_path(base).uri();
        self.webview.load_html(&doc.html, Some(base_uri.as_str()));
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

    pub fn set_zoom(&self, level: f64) {
        self.webview.set_zoom_level(level.max(0.2));
    }

    pub fn zoom_level(&self) -> f64 {
        self.webview.zoom_level()
    }

    /// Toggle the `dark` class on `<html>`, matching the pipeline's recolor CSS.
    pub fn set_dark(&self, dark: bool) {
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

    /// Query scroll offset (px) and percentage (0..=100) in one JS round-trip,
    /// delivering both to `callback`. Used by the D-Bus `GetState` method so a
    /// single reply reflects a single, consistent viewport snapshot.
    pub fn scroll_state<F: FnOnce(f64, u32) + 'static>(&self, callback: F) {
        let script = "(() => { const d = document.documentElement, b = document.body; \
             const max = (b.scrollHeight || d.scrollHeight) - window.innerHeight; \
             const p = max > 0 ? Math.round((window.scrollY / max) * 100) : 0; \
             return { y: window.scrollY, p: Math.min(100, Math.max(0, p)) }; })()";
        self.webview.evaluate_javascript(
            script,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |res| match res {
                Ok(v) => {
                    let y = v.object_get_property("y").map_or(0.0, |n| n.to_double());
                    let p = v.object_get_property("p").map_or(0.0, |n| n.to_double());
                    callback(y, p.clamp(0.0, 100.0) as u32);
                }
                Err(_) => callback(0.0, 0),
            },
        );
    }

    /// Restore a scroll offset (px) after a reload once layout is available.
    pub fn restore_scroll(&self, y: f64) {
        self.run_js(&format!("window.scrollTo(0, {y});"));
    }
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
