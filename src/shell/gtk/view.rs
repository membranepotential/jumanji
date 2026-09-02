//! The WebKit content view: the GTK implementation of
//! [`Viewport`](crate::controller::toolkit::Viewport).
//!
//! Only engine primitives live here — load, eval, zoom level, background
//! colour, find, focus — plus the WebKitGTK-specific plumbing around them: the
//! `window.__jmnj_post` prelude, the user-script installation, the single
//! script-message router, and the navigation policy. Everything the reader
//! *does* with a document (scrolling, hints, anchored zoom, the document-load
//! rewrite, the state snapshot) is toolkit-agnostic and lives in
//! [`controller::page`](crate::controller::page), and what each posted message
//! *means* lives in the controller too — this file only splits a message into
//! its name and payload and passes it on.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gtk::gdk::RGBA;
use gtk::prelude::*;
use webkit6::prelude::*;
use webkit6::{
    FindController, FindOptions, NavigationPolicyDecision, NavigationType, PolicyDecisionType,
    UserContentInjectedFrames, UserContentManager, UserScript, UserScriptInjectionTime, WebView,
};

use crate::controller::scripts::{self, POST_FN};
use crate::controller::toolkit::Viewport;

/// The controller's navigation sink, installed after construction: a resolved
/// target URI for every navigation the policy below refused.
type NavigateSink = Rc<RefCell<Option<Box<dyn Fn(String)>>>>;

/// The controller's message sink: `(name, payload)` for everything the shared
/// scripts post through [`POST_HANDLER`].
type MessageSink = Rc<RefCell<Option<Box<dyn Fn(&str, &str)>>>>;

/// The last selection the *user* made, shared with the shell's
/// [`Host`](crate::controller::toolkit::Host) so both halves of the
/// copy-on-select feature agree on what PRIMARY should hold.
pub type LastSelection = Rc<RefCell<Option<String>>>;

/// Native WebView background painted behind the document, matched to the theme
/// so unpainted regions never flash a mismatched colour (light `#ffffff`,
/// dark `#1a1a1a` — the same values `style.css` uses for `--bg`).
const BG_LIGHT: RGBA = RGBA::WHITE;
const BG_DARK: RGBA = RGBA::new(0.101, 0.101, 0.101, 1.0);

/// The single script-message handler every shared script
/// ([`scripts::document_start`], [`scripts::hints_build_js`]) posts through,
/// via the `window.__jmnj_post` prelude installed in [`View::new`]. One handler
/// instead of one per message name: WebKitGTK is the toolkit that owns
/// `window.webkit.messageHandlers`, so the seam that lets a non-WebKit shell
/// share these scripts unmodified is exactly this — a single postMessage point
/// the shell splits back into name and payload and hands to the controller
/// (see [`connect_message_router`]).
const POST_HANDLER: &str = "jmnj";

#[derive(Clone)]
pub struct View {
    webview: WebView,
    /// Called with the name and payload of every message the shared scripts
    /// post (`selection`, `scroll`, `hints`, `editorsync`).
    message_cb: MessageSink,
    /// Called with a resolved target URI when the webview tries to navigate
    /// (a link click); the controller decides whether to scroll, open, or
    /// delegate.
    navigate_cb: NavigateSink,
}

impl View {
    /// `last_selection` is shared with the shell's `Host`: WebKitGTK copies a
    /// find match into PRIMARY as it selects it, and the `found-text` hook
    /// below undoes that by writing the user's last real selection back.
    pub fn new(last_selection: LastSelection) -> Self {
        let ucm = UserContentManager::new();
        let message_cb: MessageSink = Rc::new(RefCell::new(None));

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
        connect_message_router(&ucm, message_cb.clone());

        let webview = WebView::builder().user_content_manager(&ucm).build();
        // WebKitGTK copies the find match into PRIMARY as it selects it. `found-text`
        // fires after that write, so restoring PRIMARY here — to the user's last real
        // selection, or empty — reliably undoes it. This is the
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

        let navigate_cb: NavigateSink = Rc::new(RefCell::new(None));
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
            message_cb,
            navigate_cb,
        }
    }

    pub fn widget(&self) -> &WebView {
        &self.webview
    }

    /// Install the handler for messages posted from inside the document. The
    /// arguments are the message name and its payload, split by
    /// [`connect_message_router`].
    pub fn set_message_handler(&self, f: impl Fn(&str, &str) + 'static) {
        *self.message_cb.borrow_mut() = Some(Box::new(f));
    }

    /// Install the handler for attempted navigations (link clicks). The
    /// argument is the resolved absolute target URI.
    pub fn set_navigate_handler(&self, f: impl Fn(String) + 'static) {
        *self.navigate_cb.borrow_mut() = Some(Box::new(f));
    }

    fn find_controller(&self) -> Option<FindController> {
        self.webview.find_controller()
    }
}

impl Viewport for View {
    /// `base` is the source file; its URI becomes the base against which
    /// document-relative images resolve.
    fn load_html(&self, html: &str, base: &Path) {
        let base_uri = gtk::gio::File::for_path(base).uri();
        self.webview.load_html(html, Some(base_uri.as_str()));
    }

    fn eval(&self, js: &str) {
        self.webview
            .evaluate_javascript(js, None, None, None::<&gtk::gio::Cancellable>, |_res| {});
    }

    fn eval_json(&self, js: &str, callback: impl FnOnce(Option<String>) + 'static) {
        self.webview.evaluate_javascript(
            js,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |res| {
                callback(res.ok().and_then(|v| v.to_json(0)).map(|s| s.to_string()));
            },
        );
    }

    fn set_zoom_level(&self, level: f64) {
        self.webview.set_zoom_level(level);
    }

    fn set_background_dark(&self, dark: bool) {
        self.webview
            .set_background_color(if dark { &BG_DARK } else { &BG_LIGHT });
    }

    /// Search the document. WebKit highlights every match and selects the first;
    /// the `found-text` handler installed in [`View::new`] then restores PRIMARY,
    /// so the highlight stays but the match never lands on the clipboard.
    fn find(&self, text: &str) {
        // Case-insensitive, wrapping search — the vim/zathura default.
        let opts = FindOptions::CASE_INSENSITIVE | FindOptions::WRAP_AROUND;
        if let Some(fc) = self.find_controller() {
            fc.search(text, opts.bits(), u32::MAX);
        }
    }

    fn find_next(&self) {
        if let Some(fc) = self.find_controller() {
            fc.search_next();
        }
    }

    fn find_previous(&self) {
        if let Some(fc) = self.find_controller() {
            fc.search_previous();
        }
    }

    fn find_clear(&self) {
        if let Some(fc) = self.find_controller() {
            fc.search_finish();
        }
    }

    fn focus(&self) {
        self.webview.grab_focus();
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

/// Register [`POST_HANDLER`] and hand every message the shared scripts post
/// through it to the controller, name and payload apart. Splitting the two is
/// all this does: which names exist and what each one means is the
/// controller's business (`scripts::message`), so a second shell reproduces
/// this function and nothing else.
///
/// The payload is split on the *first* `:` only — the hints message posts
/// `label\thref` lines, which themselves contain newlines and tabs, so only
/// the separator the prelude inserted may be significant.
fn connect_message_router(ucm: &UserContentManager, message_cb: MessageSink) {
    ucm.connect_script_message_received(Some(POST_HANDLER), move |_, value| {
        let payload = value.to_str();
        let Some((name, msg)) = payload.split_once(':') else {
            return;
        };
        if let Some(cb) = message_cb.borrow().as_ref() {
            cb(name, msg);
        }
    });
}

/// Deny every webview-initiated navigation except the programmatic document
/// load (`load_html`/reload, which arrive as `NavigationType::Other`). A link
/// click is routed to the controller instead — the app itself never navigates
/// (DESIGN.md: offline-only, CSP-locked). See `set_navigate_handler`.
fn install_navigation_policy(webview: &WebView, sink: NavigateSink) {
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
