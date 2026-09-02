//! The contract between the controller and a toolkit shell.
//!
//! Three small traits, one per concern, bundled by [`Toolkit`] so the
//! controller is generic over a single type:
//!
//! - [`Viewport`] — a webview, reduced to the primitives the controller
//!   composes everything else from (load, eval, zoom, background, find, focus).
//!   The scrolling, hint, zoom-anchoring and state-snapshot behaviour is *not*
//!   here: it is JS the controller owns and runs through `eval`, so it is the
//!   same on every platform by construction.
//! - [`Chrome`] — the status line, the input bar, and the table-of-contents
//!   page. Widgets on GTK; in-page overlays on a toolkit without widgets.
//! - [`Host`] — the main loop and the operating system: timers, a worker
//!   thread whose result lands back on the main loop, the system URI handler,
//!   the selection clipboard, detached process spawning, and quitting.
//!
//! Every callback the controller hands a toolkit is `'static` but **not**
//! `Send`: the controller lives in an `Rc<RefCell<…>>` on the main thread and
//! its callbacks capture that handle. A toolkit whose native API demands
//! `Send` (wry's `evaluate_script_with_callback`, tao's `EventLoopProxy`)
//! parks the callback in a main-thread slot and sends only a key across.

use std::path::Path;
use std::time::Duration;

use crate::core::Heading;
use crate::core::config::SelectionClipboard;

/// A webview: the primitives the controller drives a document through.
pub trait Viewport {
    /// Replace the document with `html`. `base` is the *source file* the
    /// document was rendered from; document-relative images resolve against
    /// its directory. How that base is expressed to the engine (a `file://`
    /// base URI, a custom-protocol origin) is the toolkit's business.
    fn load_html(&self, html: &str, base: &Path);

    /// Run `js` in the document's main frame, discarding the result.
    fn eval(&self, js: &str);

    /// Run `js` and deliver its completion value, **serialized as JSON**, to
    /// `callback` on the main loop — or `None` when the eval failed or the
    /// value has no JSON form. A number comes back as `"12.5"`, an object as
    /// `{"y":12.5,…}`; the controller parses. JSON rather than a native value
    /// type because that is the one representation every engine binding can
    /// produce (JavaScriptCore `to_json`, wry's callback string).
    fn eval_json(&self, js: &str, callback: impl FnOnce(Option<String>) + 'static);

    /// Engine-level page zoom (WebKitGTK `zoom-level`, WKWebView `pageZoom`):
    /// scales the whole page, diagrams included. A property of the view that
    /// survives a document load.
    fn set_zoom_level(&self, level: f64);

    /// The native colour painted behind the document, so regions the page has
    /// not painted yet never flash a mismatched theme.
    fn set_background_dark(&self, dark: bool);

    /// Find-in-page: highlight every match of `text`, select the first.
    /// Case-insensitive, wrapping — the vim/zathura default.
    fn find(&self, text: &str);
    fn find_next(&self);
    fn find_previous(&self);
    /// Drop the current search: highlights and `n`/`N` state.
    fn find_clear(&self);

    /// Give the document keyboard focus (after the input bar closes, after
    /// leaving the TOC page).
    fn focus(&self);
}

/// Which kind of input the bar is collecting. The prompt character and how
/// `Enter` is interpreted both follow from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// `/` incremental find.
    Search,
    /// `:` command line.
    Command,
}

impl Prompt {
    /// The character the input bar shows in front of the query.
    pub fn prefix(self) -> &'static str {
        match self {
            Prompt::Search => "/",
            Prompt::Command => ":",
        }
    }
}

/// The reader's chrome: status line, input bar, table-of-contents page.
///
/// The bar has two fields. The **left** shows either the jumplist breadcrumb
/// (`set_trail`, re-fitted to the width on demand) or a transient message
/// that holds until the trail is set again. The **right** shows the scroll
/// percent plus optional pending-key and zoom indicators.
pub trait Chrome {
    /// Show the breadcrumb — the route to the current document, oldest first,
    /// as display names — fitted to the bar. Replaces any transient message.
    fn set_trail(&self, segments: Vec<String>);
    /// Re-fit the breadcrumb to the current width. Idempotent and cheap, so
    /// the status refresh calls it unconditionally; a transient message wins.
    fn refit_trail(&self);
    /// How many monospace columns the left field currently spans — the budget
    /// anything laid out to fit it (breadcrumb, completion echo) works with.
    /// `usize::MAX` when the width is not yet known.
    fn status_columns(&self) -> usize;
    /// Right-hand status: pending count/key indicator, zoom indicator (empty
    /// strings when there is nothing to show), scroll percent.
    fn set_status_right(&self, percent: u32, pending: &str, zoom: &str);
    /// A transient notice on the left (`no links in view`, errors, `Index`).
    fn set_message(&self, msg: &str);

    /// Open the input bar for `prompt`, seeded with its prefix, focused.
    fn open_input(&self, prompt: Prompt);
    /// Hide and clear the input bar.
    fn close_input(&self);
    /// The active prompt, or `None` while the input bar is hidden.
    fn prompt(&self) -> Option<Prompt>;
    /// The input text with the prompt prefix removed.
    fn input_query(&self) -> String;
    /// Replace the input text (prefix preserved), cursor at the end.
    fn set_input_query(&self, query: &str);

    /// Build the TOC page from `headings` (every node expanded), select the
    /// entry for `section`, and show the page in place of the document.
    fn show_toc(&self, headings: &[Heading], section: usize, dark: bool);
    /// Return to the document page.
    fn hide_toc(&self);
    /// Move the TOC selection by `delta` visible rows, clamped. Positive is down.
    fn toc_move(&self, delta: i32);
    /// Expand the selected TOC node's children (no-op on a leaf).
    fn toc_expand(&self);
    /// Collapse the selected TOC node; on a leaf or an already-collapsed node,
    /// move the selection to its parent instead (zathura `h`).
    fn toc_collapse(&self);
    /// The selected entry's anchor and index into the heading list.
    fn toc_selected(&self) -> Option<(String, usize)>;

    /// Recolor the chrome to match the document's dark mode.
    fn set_dark(&self, dark: bool);
}

/// The main loop and the operating system, as the controller needs them.
pub trait Host {
    /// A running repeating timer; dropping it cancels the timer.
    type Timer;

    /// Run `f` once on the main loop after `delay`.
    fn defer(&self, delay: Duration, f: impl FnOnce() + 'static);

    /// Run `f` on the main loop every `period` until the timer is dropped.
    fn interval(&self, period: Duration, f: impl FnMut() + 'static) -> Self::Timer;

    /// Run `work` off the main thread and deliver its result to `done` on the
    /// main loop. `done` receives `None` if `work` panicked: the controller
    /// must be able to carry on (a vault walk that blew up costs some links
    /// their targets, not the reader its document).
    fn spawn_blocking<R: Send + 'static>(
        &self,
        work: impl FnOnce() -> R + Send + 'static,
        done: impl FnOnce(Option<R>) + 'static,
    );

    /// Hand `uri` to the system's default handler (a browser, an image viewer).
    /// The reader itself never navigates.
    fn open_external(&self, uri: &str) -> Result<(), String>;

    /// Copy `text` to the selection target the user configured.
    fn copy_selection(&self, text: &str, target: SelectionClipboard);

    /// Spawn `argv` detached — never waited on, never blocking the UI. Used
    /// for the reverse editor sync (DESIGN D7). `argv[0]` is the program.
    fn spawn_detached(&self, argv: &[String]) -> Result<(), String>;

    /// Close the window and end the main loop. The shell must route this
    /// through the same path a window-manager close takes, so the controller's
    /// close hook (which flushes history) runs either way.
    fn quit(&self);
}

/// A toolkit: the three implementations bundled so the controller is generic
/// over one type.
///
/// All three are `Clone + 'static` because the controller treats them as cheap
/// handles: it hands them to `'static` callbacks (an eval completion, a timer,
/// a worker landing) constantly, and a toolkit type that could not be copied
/// into one would force every such site through the shared `RefCell` instead.
/// GTK objects are refcounted handles already; a fake wraps its recording in an
/// `Rc`. `Viewport: Clone + 'static` is also exactly what
/// [`Page`](crate::controller::page::Page)'s blanket impl asks for, so a
/// toolkit's viewport gets the shared behaviour by construction.
pub trait Toolkit {
    type Viewport: Viewport + Clone + 'static;
    type Chrome: Chrome + Clone + 'static;
    type Host: Host + Clone + 'static;
}
