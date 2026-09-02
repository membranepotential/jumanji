//! The fake toolkit the controller's own tests run on.
//!
//! [`FakeToolkit`] implements the three traits in
//! [`toolkit`](crate::controller::toolkit) over plain recorded state: no
//! display, no web engine, no main loop. Every part is an `Rc`-backed handle
//! (the associated types are `Clone + 'static`, and the controller keeps its
//! own clones), so a test inspects exactly the state the controller drives.
//!
//! Nothing here runs by itself. Every asynchronous edge of a real toolkit —
//! an `eval_json` completion, a timer, a worker landing — is *queued*, and the
//! test's harness pumps it where the story says it happens:
//!
//! - **`eval_json` replies are queued and delivered by
//!   [`FakeViewport::run_evals`]**, never inside the call that issued them.
//!   That is what a real engine does (the reply crosses an IPC hop), and it
//!   matters: the controller issues evals while holding a `RefCell` borrow of
//!   its own session (`flush_wheel_zoom` around `Page::zoom_to`, for one), so
//!   a fake that called back synchronously would panic where WebKit would not.
//!   The reply itself comes from a one-field page model (the scroll offset)
//!   rather than from a scripted queue: the controller asks for `window.scrollY`
//!   and for the state snapshot on nearly every action, so a positional queue
//!   would make every test count evals instead of describing behaviour.
//! - **Timers and worker landings are queued too** ([`FakeHost::run_timers`],
//!   [`FakeHost::land_work`]). Repeating timers are *held* (so cancel-on-drop
//!   is real, and [`FakeHost::timer_events`] can witness a watcher being
//!   replaced) but never ticked: they are the live-reload and stdin polls,
//!   whose input is the filesystem rather than the test.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use crate::controller::scripts::FIRST_FRAME_GLOBAL;
use crate::controller::toolkit::{Chrome, Host, Prompt, Toolkit, Viewport};
use crate::core::Heading;
use crate::core::config::SelectionClipboard;

/// The toolkit under test: the three fakes bundled.
pub struct FakeToolkit;

impl Toolkit for FakeToolkit {
    type Viewport = FakeViewport;
    type Chrome = FakeChrome;
    type Host = FakeHost;
}

// ---------------------------------------------------------------------------
// Viewport
// ---------------------------------------------------------------------------

/// One recorded call on the viewport. The JS-composing behaviour ([`Page`] and
/// everything built on it) collapses into [`Eval`](ViewCall::Eval) /
/// [`EvalJson`](ViewCall::EvalJson) carrying the script text — which is what
/// the controller's contract with a page actually is.
///
/// [`Page`]: crate::controller::page::Page
#[derive(Debug, Clone, PartialEq)]
pub enum ViewCall {
    LoadHtml { html: String, base: PathBuf },
    Eval(String),
    EvalJson(String),
    SetZoomLevel(f64),
    SetBackgroundDark(bool),
    Find(String),
    FindNext,
    FindPrevious,
    FindClear,
    Focus,
}

/// A recording viewport over a one-field page model.
#[derive(Clone, Default)]
pub struct FakeViewport(Rc<RefCell<ViewState>>);

#[derive(Default)]
struct ViewState {
    calls: Vec<ViewCall>,
    scroll_y: f64,
    /// `eval_json` scripts whose reply has not been delivered yet, oldest
    /// first — the fake's stand-in for the engine's completion queue.
    pending: VecDeque<PendingEval>,
}

/// One issued `eval_json`: the script, and the completion waiting on it.
struct PendingEval {
    js: String,
    callback: Box<dyn FnOnce(Option<String>)>,
}

/// How many queued replies [`FakeViewport::run_evals`] will deliver before it
/// decides the controller is chasing its own tail. Generous: the longest real
/// chain is a link follow (scroll query → load → status refresh).
const EVAL_SETTLE_LIMIT: usize = 64;

impl FakeViewport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything recorded since the last [`clear`](Self::clear).
    pub fn calls(&self) -> Vec<ViewCall> {
        self.0.borrow().calls.clone()
    }

    /// Forget the recorded calls (the page state stays) — the setup/act
    /// boundary a test acts from.
    pub fn clear(&self) {
        self.0.borrow_mut().calls.clear();
    }

    /// Move the fake page: the offset `window.scrollY` and the state snapshot
    /// report from here on. The controller scrolls by evaluating JS, which
    /// nothing here runs, so a test that cares about a position sets it.
    pub fn set_scroll_y(&self, y: f64) {
        self.0.borrow_mut().scroll_y = y;
    }

    /// The scripts passed to `eval`, in order.
    pub fn evals(&self) -> Vec<String> {
        self.0
            .borrow()
            .calls
            .iter()
            .filter_map(|c| match c {
                ViewCall::Eval(js) => Some(js.clone()),
                _ => None,
            })
            .collect()
    }

    /// Whether any evaluated script contains `needle`.
    pub fn evaled(&self, needle: &str) -> bool {
        self.evals().iter().any(|js| js.contains(needle))
    }

    /// The documents loaded, as `(html, base)`, in order.
    pub fn loads(&self) -> Vec<(String, PathBuf)> {
        self.0
            .borrow()
            .calls
            .iter()
            .filter_map(|c| match c {
                ViewCall::LoadHtml { html, base } => Some((html.clone(), base.clone())),
                _ => None,
            })
            .collect()
    }

    /// The native zoom levels set, in order.
    pub fn zoom_levels(&self) -> Vec<f64> {
        self.0
            .borrow()
            .calls
            .iter()
            .filter_map(|c| match c {
                ViewCall::SetZoomLevel(level) => Some(*level),
                _ => None,
            })
            .collect()
    }

    /// Deliver every queued `eval_json` reply, and every reply the resulting
    /// controller work queues in turn, until the page falls silent. The
    /// harness pumps this after each action, the way a main loop would.
    pub fn run_evals(&self) {
        for _ in 0..EVAL_SETTLE_LIMIT {
            let next = self.0.borrow_mut().pending.pop_front();
            let Some(PendingEval { js, callback }) = next else {
                return;
            };
            // Answered at delivery time, from the page as it stands now.
            let reply = self.reply(&js);
            callback(reply);
        }
        panic!("eval_json replies never settled ({EVAL_SETTLE_LIMIT} delivered)");
    }

    fn record(&self, call: ViewCall) {
        self.0.borrow_mut().calls.push(call);
    }

    /// The fake page's answer to `js`: its scroll offset, its state snapshot,
    /// or nothing at all (the zoom-anchor capture, whose value the controller
    /// ignores).
    fn reply(&self, js: &str) -> Option<String> {
        let y = self.0.borrow().scroll_y;
        if js.trim() == "window.scrollY" {
            return Some(y.to_string());
        }
        if js.contains(FIRST_FRAME_GLOBAL) {
            // Only the two fields a display-free page can mean anything by;
            // every other key of the snapshot defaults (see `page::Snapshot`).
            return Some(format!("{{\"y\":{y},\"p\":0}}"));
        }
        None
    }
}

impl Viewport for FakeViewport {
    fn load_html(&self, html: &str, base: &Path) {
        self.record(ViewCall::LoadHtml {
            html: html.to_string(),
            base: base.to_path_buf(),
        });
    }

    fn eval(&self, js: &str) {
        self.record(ViewCall::Eval(js.to_string()));
    }

    fn eval_json(&self, js: &str, callback: impl FnOnce(Option<String>) + 'static) {
        self.record(ViewCall::EvalJson(js.to_string()));
        // Queued, not called: the reply must not land inside the borrow the
        // caller is still holding (see the module docs).
        self.0.borrow_mut().pending.push_back(PendingEval {
            js: js.to_string(),
            callback: Box::new(callback),
        });
    }

    fn set_zoom_level(&self, level: f64) {
        self.record(ViewCall::SetZoomLevel(level));
    }

    fn set_background_dark(&self, dark: bool) {
        self.record(ViewCall::SetBackgroundDark(dark));
    }

    fn find(&self, text: &str) {
        self.record(ViewCall::Find(text.to_string()));
    }

    fn find_next(&self) {
        self.record(ViewCall::FindNext);
    }

    fn find_previous(&self) {
        self.record(ViewCall::FindPrevious);
    }

    fn find_clear(&self) {
        self.record(ViewCall::FindClear);
    }

    fn focus(&self) {
        self.record(ViewCall::Focus);
    }
}

// ---------------------------------------------------------------------------
// Chrome
// ---------------------------------------------------------------------------

/// The right-hand status field, as last painted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusRight {
    pub percent: u32,
    pub pending: String,
    pub zoom: String,
}

/// The chrome as plain state: bar fields, input bar, TOC list.
#[derive(Clone, Default)]
pub struct FakeChrome(Rc<RefCell<ChromeState>>);

#[derive(Default)]
struct ChromeState {
    trail: Vec<String>,
    message: String,
    right: StatusRight,
    prompt: Option<Prompt>,
    input: String,
    toc_shown: bool,
    headings: Vec<Heading>,
    selected: usize,
    dark: bool,
}

/// The width the fake bar reports, wide enough for a completion echo to hold
/// several candidates and narrow enough that paging is exercised.
const STATUS_COLUMNS: usize = 80;

impl FakeChrome {
    pub fn new() -> Self {
        Self::default()
    }

    /// The breadcrumb segments last set.
    pub fn trail(&self) -> Vec<String> {
        self.0.borrow().trail.clone()
    }

    /// The transient message, or `""` once a breadcrumb has replaced it.
    pub fn message(&self) -> String {
        self.0.borrow().message.clone()
    }

    pub fn status_right(&self) -> StatusRight {
        self.0.borrow().right.clone()
    }

    /// Whether the TOC page is the visible one.
    pub fn toc_shown(&self) -> bool {
        self.0.borrow().toc_shown
    }

    /// The selected TOC row's index into the heading list.
    pub fn toc_index(&self) -> usize {
        self.0.borrow().selected
    }
}

impl Chrome for FakeChrome {
    fn set_trail(&self, segments: Vec<String>) {
        let mut c = self.0.borrow_mut();
        c.trail = segments;
        // A breadcrumb replaces a transient message, as the real bar does.
        c.message.clear();
    }

    fn refit_trail(&self) {}

    fn status_columns(&self) -> usize {
        STATUS_COLUMNS
    }

    fn set_status_right(&self, percent: u32, pending: &str, zoom: &str) {
        self.0.borrow_mut().right = StatusRight {
            percent,
            pending: pending.to_string(),
            zoom: zoom.to_string(),
        };
    }

    fn set_message(&self, msg: &str) {
        self.0.borrow_mut().message = msg.to_string();
    }

    fn open_input(&self, prompt: Prompt) {
        let mut c = self.0.borrow_mut();
        c.prompt = Some(prompt);
        c.input.clear();
    }

    fn close_input(&self) {
        let mut c = self.0.borrow_mut();
        c.prompt = None;
        c.input.clear();
    }

    fn prompt(&self) -> Option<Prompt> {
        self.0.borrow().prompt
    }

    fn input_query(&self) -> String {
        self.0.borrow().input.clone()
    }

    fn set_input_query(&self, query: &str) {
        self.0.borrow_mut().input = query.to_string();
    }

    fn show_toc(&self, headings: &[Heading], section: usize, dark: bool) {
        let mut c = self.0.borrow_mut();
        c.headings = headings.to_vec();
        c.selected = section.min(headings.len().saturating_sub(1));
        c.dark = dark;
        c.toc_shown = true;
    }

    fn hide_toc(&self) {
        self.0.borrow_mut().toc_shown = false;
    }

    fn toc_move(&self, delta: i32) {
        let mut c = self.0.borrow_mut();
        if c.headings.is_empty() {
            return;
        }
        let last = (c.headings.len() - 1) as i32;
        c.selected = (c.selected as i32 + delta).clamp(0, last) as usize;
    }

    /// Every node is expanded in this model, so there is nothing to open.
    fn toc_expand(&self) {}

    /// With everything expanded, collapsing is only zathura's "move to the
    /// parent": the nearest earlier heading of a shallower level.
    fn toc_collapse(&self) {
        let mut c = self.0.borrow_mut();
        let Some(level) = c.headings.get(c.selected).map(|h| h.level) else {
            return;
        };
        if let Some(parent) = c.headings[..c.selected]
            .iter()
            .rposition(|h| h.level < level)
        {
            c.selected = parent;
        }
    }

    fn toc_selected(&self) -> Option<(String, usize)> {
        let c = self.0.borrow();
        c.headings
            .get(c.selected)
            .map(|h| (h.anchor.clone(), c.selected))
    }

    fn set_dark(&self, dark: bool) {
        self.0.borrow_mut().dark = dark;
    }
}

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

/// A repeating timer's lifecycle. Ids are handed out in creation order, so a
/// log of these says *which* timer was cancelled, and when — which is how a
/// test tells a watcher that was replaced from one that was never re-pointed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerEvent {
    Started(u64),
    Cancelled(u64),
}

/// The main loop and the operating system, reduced to queues a test drains.
#[derive(Clone, Default)]
pub struct FakeHost(Rc<RefCell<HostState>>);

#[derive(Default)]
struct HostState {
    /// One-shot timers, in the order they were armed.
    deferred: Vec<Box<dyn FnOnce()>>,
    /// Live repeating timers, by id. Held so that cancel-on-drop is real;
    /// never ticked (see the module docs).
    intervals: BTreeMap<u64, Box<dyn FnMut()>>,
    next_interval: u64,
    /// Every start and cancellation, in order.
    timer_log: Vec<TimerEvent>,
    /// Finished worker jobs waiting to land on the main loop. `true` delivers
    /// the result, `false` is the panicked walk the controller must survive.
    landings: VecDeque<Box<dyn FnOnce(bool)>>,
    opened: Vec<String>,
    copied: Vec<(String, SelectionClipboard)>,
    spawned: Vec<Vec<String>>,
    /// Armed failures: the next `open_external` / `spawn_detached` fails with
    /// this message instead of succeeding (no browser, no such editor).
    next_external_error: Option<String>,
    next_spawn_error: Option<String>,
    quit: bool,
}

impl FakeHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fire every armed one-shot timer, oldest first. A timer armed *by* one of
    /// them stays queued for the next call, so a test never loops by accident.
    pub fn run_timers(&self) {
        let due = std::mem::take(&mut self.0.borrow_mut().deferred);
        for f in due {
            f();
        }
    }

    /// How many one-shot timers are armed.
    pub fn armed_timers(&self) -> usize {
        self.0.borrow().deferred.len()
    }

    /// How many repeating timers are alive (one per live watcher).
    pub fn live_intervals(&self) -> usize {
        self.0.borrow().intervals.len()
    }

    /// Every repeating timer started or cancelled, in order.
    pub fn timer_events(&self) -> Vec<TimerEvent> {
        self.0.borrow().timer_log.clone()
    }

    /// Land every finished worker job with its result.
    pub fn land_work(&self) {
        self.drain_landings(true);
    }

    /// Land every finished worker job as a panicked one (`done(None)`).
    pub fn land_work_panicked(&self) {
        self.drain_landings(false);
    }

    /// The URIs handed to the system's default handler.
    pub fn opened(&self) -> Vec<String> {
        self.0.borrow().opened.clone()
    }

    /// The selections copied, with the clipboard each went to.
    pub fn copied(&self) -> Vec<(String, SelectionClipboard)> {
        self.0.borrow().copied.clone()
    }

    /// The argv of every detached spawn (the editor sync).
    pub fn spawned(&self) -> Vec<Vec<String>> {
        self.0.borrow().spawned.clone()
    }

    /// Whether the controller asked the window to close.
    pub fn quit_called(&self) -> bool {
        self.0.borrow().quit
    }

    /// Make the next hand-off to the system handler fail with `message` — the
    /// desktop with no browser registered for the scheme.
    pub fn fail_next_external(&self, message: &str) {
        self.0.borrow_mut().next_external_error = Some(message.to_string());
    }

    /// Make the next detached spawn fail with `message` — an `editor-command`
    /// naming a program that is not there.
    pub fn fail_next_spawn(&self, message: &str) {
        self.0.borrow_mut().next_spawn_error = Some(message.to_string());
    }

    fn drain_landings(&self, deliver: bool) {
        let due = std::mem::take(&mut self.0.borrow_mut().landings);
        for f in due {
            f(deliver);
        }
    }
}

/// A live repeating timer; dropping it removes it from the host, exactly as
/// dropping a `SourceGuard` removes its glib source.
pub struct FakeTimer {
    host: Rc<RefCell<HostState>>,
    id: u64,
}

impl Drop for FakeTimer {
    fn drop(&mut self) {
        let mut h = self.host.borrow_mut();
        h.intervals.remove(&self.id);
        h.timer_log.push(TimerEvent::Cancelled(self.id));
    }
}

impl Host for FakeHost {
    type Timer = FakeTimer;

    fn defer(&self, _delay: Duration, f: impl FnOnce() + 'static) {
        self.0.borrow_mut().deferred.push(Box::new(f));
    }

    fn interval(&self, _period: Duration, f: impl FnMut() + 'static) -> Self::Timer {
        let id = {
            let mut h = self.0.borrow_mut();
            let id = h.next_interval;
            h.next_interval += 1;
            h.intervals.insert(id, Box::new(f));
            h.timer_log.push(TimerEvent::Started(id));
            id
        };
        FakeTimer {
            host: self.0.clone(),
            id,
        }
    }

    fn spawn_blocking<R: Send + 'static>(
        &self,
        work: impl FnOnce() -> R + Send + 'static,
        done: impl FnOnce(Option<R>) + 'static,
    ) {
        // The work runs here and now — it is a directory walk over a temp tree,
        // and a test wants it finished; only the *landing* is deferred, which
        // is the half the controller sequences anything against.
        let result = work();
        self.0
            .borrow_mut()
            .landings
            .push_back(Box::new(move |deliver| done(deliver.then_some(result))));
    }

    fn open_external(&self, uri: &str) -> Result<(), String> {
        let mut h = self.0.borrow_mut();
        if let Some(err) = h.next_external_error.take() {
            return Err(err); // Nothing was opened, so nothing is recorded.
        }
        h.opened.push(uri.to_string());
        Ok(())
    }

    fn copy_selection(&self, text: &str, target: SelectionClipboard) {
        self.0.borrow_mut().copied.push((text.to_string(), target));
    }

    fn spawn_detached(&self, argv: &[String]) -> Result<(), String> {
        let mut h = self.0.borrow_mut();
        if let Some(err) = h.next_spawn_error.take() {
            return Err(err); // Nothing ran, so nothing is recorded.
        }
        h.spawned.push(argv.to_vec());
        Ok(())
    }

    fn quit(&self) {
        self.0.borrow_mut().quit = true;
    }
}
