//! The controller's own tests: the reader's flows driven through the
//! [`fake`](super::fake) toolkit, with no display, no web engine and no main
//! loop — the half of the reader's behaviour that used to be provable only by
//! the e2e suite.
//!
//! Each test builds a session over a fresh temp vault (its own document, its
//! own config and data dirs) and reads as one scenario: act on the controller
//! the way a shell would, then assert on what the fake recorded.
//!
//! [`Reader`] is the main loop the fake does not have: every action it offers
//! drives the controller and then delivers whatever page replies that produced
//! ([`FakeViewport::run_evals`]), so a test can assert immediately afterwards.
//! Timers and worker landings stay explicit — `run_timers`, `land_work` — since
//! *when* those happen is the thing several of these tests are about.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::fake::{FakeChrome, FakeHost, FakeToolkit, FakeViewport, TimerEvent, ViewCall};
use super::scripts::{
    DIAGRAM_ZOOM_MAX, DIAGRAM_ZOOM_MIN, js_string, message, nearest_source_element_js,
};
use super::session::{Controller, Dirs, KeyOutcome};
use super::toolkit::{Chrome, Prompt};
use crate::core::Action;
use crate::core::config::{
    DIAGRAM_FIT_CLASS, DIAGRAM_ZOOM_CLASS, DIAGRAM_ZOOM_VAR, Options, SelectionClipboard,
};
use crate::core::editor::EditorCommand;
use crate::core::keymap::{Key, KeyPress, Keymap};
use crate::core::source::Source;

/// Three headings and one document-relative link — enough for the TOC, the
/// section machinery and the hint overlay to have something to talk about.
const DOC: &str = "\
# Alpha

Prose with a [link](other.md).

## Beta

More prose.

## Gamma

Tail.
";

/// A document that names the vault, so its initial render is deferred until
/// the background scan lands (see `Session::initial_render_deferred`).
const VAULT_DOC: &str = "# Alpha\n\nA reference to [[Note]].\n";

/// The id of the link-hint overlay: the one piece of the hint scripts that is
/// a contract rather than a spelling — the build, filter and clear scripts all
/// address the same element by it.
const HINTS_OVERLAY_ID: &str = "__jmnj_hints";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A uniquely-named temp directory, removed when the test drops it.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("jumanji-ctl-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A controller under test, with the three fake handles it is driving and the
/// temp tree it was built over.
struct Reader {
    controller: Controller<FakeToolkit>,
    view: FakeViewport,
    chrome: FakeChrome,
    host: FakeHost,
    file: PathBuf,
    data_dir: PathBuf,
    /// Kept alive (and cleaned up) for the lifetime of the test.
    _root: TempDir,
}

impl Drop for Reader {
    /// An armed timer owns a controller handle, so a session left holding one
    /// is a cycle: it would never be dropped, and the inotify watcher it owns
    /// never released. Firing whatever is still queued ends the test cleanly.
    fn drop(&mut self) {
        self.host.run_timers();
        self.view.run_evals();
    }
}

impl Reader {
    /// A session over a temp vault holding `markdown` as `doc.md`, taken as far
    /// as [`Controller::new`] takes it — the load has been issued but the
    /// engine has not reported it finished.
    fn open(markdown: &str) -> Self {
        Self::open_with(markdown, |_| {})
    }

    /// [`Reader::open`], with `plant` run against the vault directory first so
    /// a test can put sibling documents where a link can reach them.
    fn open_with(markdown: &str, plant: impl FnOnce(&Path)) -> Self {
        Self::open_full(markdown, Options::default(), plant)
    }

    /// [`Reader::open_with`] over non-default options.
    fn open_full(markdown: &str, options: Options, plant: impl FnOnce(&Path)) -> Self {
        let root = TempDir::new();
        let vault = root.0.join("vault");
        let data_dir = root.0.join("data");
        let config_dir = root.0.join("config");
        // An explicit Obsidian marker pins the vault root at this directory,
        // whatever the ancestors of the system temp dir happen to contain.
        std::fs::create_dir_all(vault.join(".obsidian")).expect("create vault");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let file = vault.join("doc.md");
        std::fs::write(&file, markdown).expect("write document");
        plant(&vault);

        let view = FakeViewport::new();
        let chrome = FakeChrome::new();
        let host = FakeHost::new();
        let controller = Controller::<FakeToolkit>::new_in(
            view.clone(),
            chrome.clone(),
            host.clone(),
            Source::File(file.clone()),
            options,
            Keymap::default(),
            None,
            Dirs {
                config: Some(config_dir),
                data: Some(data_dir.clone()),
            },
        );
        let reader = Reader {
            controller,
            view,
            chrome,
            host,
            file,
            data_dir,
            _root: root,
        };
        reader.settle();
        reader
    }

    /// A session whose first load has finished — the state every key press and
    /// automation call assumes — with the setup's recorded calls forgotten.
    fn loaded(markdown: &str) -> Self {
        let r = Self::open(markdown);
        r.finish_load();
        r
    }

    /// [`Reader::loaded`] with a sibling `other.md` planted beside the document.
    fn loaded_with_sibling(markdown: &str) -> Self {
        let r = Self::open_with(markdown, |vault| {
            std::fs::write(vault.join("other.md"), "# Other\n\nElsewhere.\n")
                .expect("write sibling");
        });
        r.finish_load();
        r
    }

    // -- the main loop -----------------------------------------------------

    /// Deliver every page reply the last action asked for.
    fn settle(&self) {
        self.view.run_evals();
    }

    /// Fire every armed one-shot timer (the render failsafe, the wheel-zoom
    /// coalescing window).
    fn run_timers(&self) {
        self.host.run_timers();
        self.settle();
    }

    /// Land the finished vault scan on the main loop.
    fn land_work(&self) {
        self.host.land_work();
        self.settle();
    }

    /// Land the vault scan as a walk that panicked.
    fn land_work_panicked(&self) {
        self.host.land_work_panicked();
        self.settle();
    }

    /// Report the load the engine was given as finished, then start recording
    /// from a clean slate.
    fn finish_load(&self) {
        self.controller.on_load_finished();
        self.settle();
        self.view.clear();
    }

    // -- acting like a shell ------------------------------------------------

    fn press(&self, c: char) -> KeyOutcome {
        let outcome = self.controller.on_key(Some(KeyPress::char(c)));
        self.settle();
        outcome
    }

    fn press_ctrl(&self, c: char) -> KeyOutcome {
        let outcome = self
            .controller
            .on_key(Some(KeyPress::new(Key::Char(c), true, false)));
        self.settle();
        outcome
    }

    fn press_key(&self, key: Key) -> KeyOutcome {
        let outcome = self
            .controller
            .on_key(Some(KeyPress::new(key, false, false)));
        self.settle();
        outcome
    }

    fn wheel_zoom(&self, dy: f64, text: bool) {
        self.controller.on_wheel_zoom(dy, text);
        self.settle();
    }

    fn message(&self, name: &str, payload: &str) {
        self.controller.on_message(name, payload);
        self.settle();
    }

    fn navigate(&self, uri: &str) {
        self.controller.on_navigate(uri);
        self.settle();
    }

    fn execute(&self, action: Action, count: u32) {
        self.controller.execute(action, count);
        self.settle();
    }

    fn execute_str(&self, action: &str, count: u32) -> Result<(), String> {
        let result = self.controller.execute_str(action, count);
        self.settle();
        result
    }

    fn goto_source_line(&self, line: u32) {
        self.controller.goto_source_line(line);
        self.settle();
    }

    /// Type `query` into the open input bar and press `Enter`, as the entry's
    /// `activate` signal does.
    fn submit_input(&self, query: &str) {
        self.chrome.set_input_query(query);
        self.controller.on_input_submitted();
        self.settle();
    }

    /// The whole `/` gesture: open the bar, type, submit.
    fn search(&self, query: &str) {
        self.press('/');
        self.submit_input(query);
    }

    /// The automation state snapshot, as JSON.
    fn state(&self) -> String {
        let out: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        {
            let out = out.clone();
            self.controller
                .state(move |json| *out.borrow_mut() = Some(json));
        }
        self.settle();
        let json = out.borrow().clone();
        json.expect("the state snapshot was delivered")
    }

    /// Post the overlay's label→href list, as the in-page hint script does.
    fn post_hints(&self, links: &[(&str, String)]) {
        let payload = links
            .iter()
            .map(|(label, href)| format!("{label}\t{href}"))
            .collect::<Vec<_>>()
            .join("\n");
        self.message(message::HINTS, &payload);
    }

    fn vault(&self) -> &Path {
        self.file.parent().expect("vault dir")
    }

    fn sibling(&self) -> PathBuf {
        self.vault().join("other.md")
    }
}

// ---------------------------------------------------------------------------
// Reading the recording
// ---------------------------------------------------------------------------

/// The `file://` URI an engine would hand back for `path`.
fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// The scroll step one `j` press covers, taken from the same defaults the
/// session under test was built with.
fn scroll_step() -> i64 {
    Options::default().scroll_step_px as i64
}

/// The first script evaluated that contains `needle`, with the whole recording
/// in the panic message when there is none.
fn eval_containing(view: &FakeViewport, needle: &str) -> String {
    view.evals()
        .into_iter()
        .find(|js| js.contains(needle))
        .unwrap_or_else(|| panic!("no eval containing {needle:?}; got {:?}", view.evals()))
}

/// The number that follows `key` in `js` (`top: -120` → `-120.0`).
fn number_after(js: &str, key: &str) -> Option<f64> {
    let (_, rest) = js.split_once(key)?;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '.')
        .collect();
    digits.parse().ok()
}

/// The `(dx, dy)` the page was told to scroll by — the delta is the contract,
/// not the call that carries it.
fn scrolled_by(view: &FakeViewport) -> (i64, i64) {
    let js = eval_containing(view, "scrollBy");
    let read =
        |key| number_after(&js, key).unwrap_or_else(|| panic!("no {key:?} in {js:?}")) as i64;
    (read("left:"), read("top:"))
}

/// Every absolute offset the page was told to scroll to, in order.
fn scrolled_to_offsets(view: &FakeViewport) -> Vec<f64> {
    view.evals()
        .iter()
        .filter_map(|js| number_after(js, "window.scrollTo(0,"))
        .collect()
}

/// The pixel value the page was told to give the `--font-size` custom property.
fn font_size_px(view: &FakeViewport) -> f64 {
    let js = eval_containing(view, "--font-size");
    let (_, after) = js.split_once("--font-size").expect("the property");
    let value = after
        .split_once(", '")
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no value set for --font-size in {js:?}"));
    let end = value
        .find("px")
        .unwrap_or_else(|| panic!("no px value in {js:?}"));
    value[..end]
        .parse()
        .unwrap_or_else(|_| panic!("unparseable font size in {js:?}"))
}

/// Just the find-related calls, in order — a search's whole observable story.
fn find_calls(view: &FakeViewport) -> Vec<ViewCall> {
    view.calls()
        .into_iter()
        .filter(|c| {
            matches!(
                c,
                ViewCall::Find(_)
                    | ViewCall::FindNext
                    | ViewCall::FindPrevious
                    | ViewCall::FindClear
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Key dispatch
// ---------------------------------------------------------------------------

#[test]
fn j_scrolls_by_one_step() {
    let r = Reader::loaded(DOC);
    assert_eq!(r.press('j'), KeyOutcome::Consumed);
    assert_eq!(scrolled_by(&r.view), (0, scroll_step()));
}

#[test]
fn a_count_prefix_multiplies_the_scroll_step() {
    let r = Reader::loaded(DOC);
    assert_eq!(r.press('3'), KeyOutcome::Consumed);
    r.press('j');
    assert_eq!(scrolled_by(&r.view), (0, 3 * scroll_step()));
}

#[test]
fn escape_is_consumed_and_clears_a_pending_count() {
    let r = Reader::loaded(DOC);
    r.press('3');
    assert_eq!(r.chrome.status_right().pending, "3");
    assert_eq!(r.press_key(Key::Escape), KeyOutcome::Consumed);
    assert_eq!(r.chrome.status_right().pending, "");
}

#[test]
fn an_unbound_key_passes_through_to_the_document() {
    let r = Reader::loaded(DOC);
    assert_eq!(r.press('z'), KeyOutcome::PassThrough);
    assert_eq!(r.view.evals(), Vec::<String>::new());
}

#[test]
fn q_asks_the_host_to_close_the_window() {
    let r = Reader::loaded(DOC);
    r.press('q');
    assert!(r.host.quit_called());
}

// ---------------------------------------------------------------------------
// Mode machine / TOC
// ---------------------------------------------------------------------------

#[test]
fn tab_shows_the_table_of_contents() {
    let r = Reader::loaded(DOC);
    r.press_key(Key::Tab);
    assert!(r.chrome.toc_shown());
    assert_eq!(r.chrome.message(), "Index");
}

#[test]
fn j_and_k_move_the_toc_selection() {
    let r = Reader::loaded(DOC);
    r.press_key(Key::Tab);
    r.press('j');
    r.press('j');
    r.press('k');
    assert_eq!(r.chrome.toc_index(), 1);
}

#[test]
fn enter_jumps_to_the_selected_heading_and_leaves_the_toc() {
    let r = Reader::loaded(DOC);
    r.press_key(Key::Tab);
    r.press('j');
    let (anchor, index) = r.chrome.toc_selected().expect("a selected heading");
    assert_eq!(index, 1);

    r.press_key(Key::Enter);
    assert!(!r.chrome.toc_shown());
    // The contract is the anchor id the page is asked for, as the page will
    // see it — not the shape of the lookup around it.
    let id = js_string(anchor.trim_start_matches('#'));
    assert!(r.view.evaled(&id), "{:?} not asked for", id);
}

#[test]
fn escape_leaves_the_toc() {
    let r = Reader::loaded(DOC);
    r.press_key(Key::Tab);
    r.press_key(Key::Escape);
    assert!(!r.chrome.toc_shown());
    // Back in Normal mode: `j` scrolls the document again.
    r.press('j');
    assert_eq!(scrolled_by(&r.view), (0, scroll_step()));
}

#[test]
fn tab_reports_a_document_with_no_headings() {
    let r = Reader::loaded("Just prose, no headings at all.\n");
    r.press_key(Key::Tab);
    assert!(!r.chrome.toc_shown());
    assert_eq!(r.chrome.message(), "no headings");
    // Still Normal: an unbound key is not swallowed by a TOC that never opened.
    assert_eq!(r.press('z'), KeyOutcome::PassThrough);
}

// ---------------------------------------------------------------------------
// Document graph (DESIGN D14)
// ---------------------------------------------------------------------------

/// The id of the graph overlay; every graph script addresses it (or the
/// `window.__jmnj_graph` handle of the same name).
const GRAPH_OVERLAY_ID: &str = "__jmnj_graph";

/// A reader over `DOC` (which links `other.md`) with the graph open: `t`, then
/// the walk landed.
fn with_graph() -> Reader {
    let r = Reader::loaded_with_sibling(DOC);
    r.press('t');
    r.land_work();
    r
}

fn mode(r: &Reader) -> String {
    let v: serde_json::Value = serde_json::from_str(&r.state()).expect("state is JSON");
    v["mode"].as_str().unwrap_or_default().to_string()
}

#[test]
fn t_draws_the_link_tree_once_the_walk_lands() {
    let r = Reader::loaded_with_sibling(DOC);
    r.press('t');
    assert_eq!(r.chrome.message(), "reading links…");
    assert!(
        !r.view.evaled(GRAPH_OVERLAY_ID),
        "nothing is drawn before the walk lands"
    );

    r.land_work();
    assert!(r.view.evaled(GRAPH_OVERLAY_ID));
    // The linked sibling is a node, titled by its H1, path relative to the vault.
    assert!(r.view.evaled(r#"data-title="Other" data-path="other.md""#));
    assert_eq!(mode(&r), "graph");
}

#[test]
fn l_selects_the_child_and_enter_opens_it() {
    let r = with_graph();
    r.press('l');
    assert!(r.view.evaled("select(1)"));
    r.view.clear();

    r.press_key(Key::Enter);
    let loads = r.view.loads();
    assert_eq!(loads.len(), 1);
    assert_eq!(
        std::fs::canonicalize(&loads[0].1).unwrap(),
        std::fs::canonicalize(r.sibling()).unwrap()
    );
    assert_eq!(mode(&r), "normal");
    assert_eq!(
        r.chrome.trail(),
        ["doc.md", "other.md"],
        "the jump is on the jumplist"
    );
}

#[test]
fn a_double_click_opens_the_node_it_names() {
    let r = with_graph();
    r.view.clear();
    // Posts name items by key: doc.md (node 0) → other.md (node 1).
    r.message(message::GRAPH_OPEN, "0.1");
    assert_eq!(r.view.loads().len(), 1);
}

#[test]
fn a_click_on_a_node_out_of_range_is_ignored() {
    let r = with_graph();
    r.view.clear();
    r.message(message::GRAPH_SELECT, "0.7");
    r.message(message::GRAPH_OPEN, "0.7");
    r.message(message::GRAPH_OPEN, "not a key");
    assert!(r.view.calls().is_empty());
}

#[test]
fn escape_before_the_walk_lands_drops_it() {
    let r = Reader::loaded_with_sibling(DOC);
    r.press('t');
    r.press_key(Key::Escape);
    r.land_work();
    assert!(!r.view.evaled(GRAPH_OVERLAY_ID));
    assert_eq!(mode(&r), "normal");
}

#[test]
fn only_the_newest_walk_is_drawn() {
    // A walk started on doc.md is still out when the reader follows the link
    // and asks again on other.md; the first must not land as the second.
    let r = Reader::loaded_with_sibling(DOC);
    r.press('t');
    r.navigate(&file_uri(&r.sibling()));
    r.finish_load();
    r.press('t');
    r.land_work();
    assert!(
        r.view
            .evaled(r#"class="jg-node route current spine" data-i="1""#),
        "other.md is where the reader is"
    );
    assert!(!r.view.evaled(r#"class="jg-node root route current spine""#));
}

#[test]
fn a_toc_opened_while_the_walk_runs_wins() {
    let r = Reader::loaded_with_sibling(DOC);
    r.press('t');
    r.press_key(Key::Tab);
    r.land_work();
    assert!(!r.view.evaled(GRAPH_OVERLAY_ID));
    assert_eq!(mode(&r), "toc");
}

#[test]
fn t_before_the_first_load_finishes_does_nothing() {
    let r = Reader::open(DOC);
    r.press('t');
    r.land_work();
    assert!(!r.view.evaled(GRAPH_OVERLAY_ID));
}

#[test]
fn t_closes_the_graph_again() {
    let r = with_graph();
    r.view.clear();
    r.press('t');
    assert!(r.view.evaled("close()"));
    assert_eq!(mode(&r), "normal");
}

#[test]
fn an_unbound_key_does_not_reach_the_page_behind_the_graph() {
    let r = with_graph();
    assert_eq!(r.press('z'), KeyOutcome::Consumed);
}

/// The graph fields of the state snapshot: view, selected node, item count.
fn graph_state(r: &Reader) -> (String, String, u64) {
    let v: serde_json::Value = serde_json::from_str(&r.state()).expect("state is JSON");
    (
        v["graph_view"].as_str().unwrap_or_default().to_string(),
        v["graph_selected"].as_str().unwrap_or_default().to_string(),
        v["graph_items"].as_u64().unwrap_or_default(),
    )
}

#[test]
fn the_state_reports_the_open_graph() {
    let r = Reader::loaded_with_sibling(DOC);
    assert_eq!(graph_state(&r), (String::new(), String::new(), 0));
    r.press('t');
    r.land_work();
    let (view, selected, items) = graph_state(&r);
    assert_eq!(view, "links");
    assert_eq!(r.chrome.message(), "Graph: links");
    assert!(selected.ends_with("doc.md"), "the current node: {selected}");
    assert_eq!(items, 2, "doc.md and its one link");
    r.press('t');
    assert_eq!(graph_state(&r).2, 0);
}

#[test]
fn v_switches_the_view_and_keeps_the_selection() {
    let r = with_graph();
    r.press('l');
    r.view.clear();
    r.press('v');
    assert!(r.view.evaled("update("), "a re-layout keeps the camera");
    let (view, selected, _) = graph_state(&r);
    assert_eq!(view, "tree");
    assert!(selected.ends_with("other.md"), "{selected}");
    assert_eq!(r.chrome.message(), "Graph: tree", "the view is named");
    r.press('v');
    assert_eq!(r.chrome.message(), "Graph: links");
    assert_eq!(graph_state(&r).0, "links");
    assert!(graph_state(&r).1.ends_with("other.md"));
}

#[test]
fn set_graph_view_re_lays_out_an_open_graph() {
    let r = with_graph();
    r.view.clear();
    r.press(':');
    r.submit_input("set graph-view tree");
    assert!(r.view.evaled("update("));
    assert!(r.view.evaled(r#"data-view="tree""#));
    assert_eq!(graph_state(&r).0, "tree");
    assert_eq!(r.chrome.message(), "Graph: tree");
    // It is the session's view now: the next graph opens in it too.
    r.press('t');
    r.press('t');
    r.land_work();
    assert_eq!(graph_state(&r).0, "tree");
}

/// A route `doc.md → other.md` where doc.md also links x.md before other.md
/// and y.md after it — other.md's siblings — and x.md links z.md. The graph
/// is open on other.md. Nodes: doc 0, x 1, other 2, y 3, z 4.
fn with_siblings() -> Reader {
    let r = Reader::open_with(
        "# Doc\n\n[x](x.md) [other](other.md) [y](y.md)\n",
        |vault| {
            std::fs::write(vault.join("other.md"), "# Other\n").expect("write other");
            std::fs::write(vault.join("x.md"), "# X\n\n[z](z.md)\n").expect("write x");
            std::fs::write(vault.join("y.md"), "# Y\n").expect("write y");
            std::fs::write(vault.join("z.md"), "# Z\n").expect("write z");
        },
    );
    r.finish_load();
    r.navigate(&file_uri(&r.sibling()));
    r.finish_load();
    r.press('t');
    r.land_work();
    r
}

#[test]
fn a_route_steps_siblings_are_on_screen_from_the_start() {
    let r = with_siblings();
    let (_, selected, items) = graph_state(&r);
    assert!(selected.ends_with("other.md"));
    assert_eq!(items, 4, "doc, other, and other's siblings x and y");
    assert!(r.view.evaled(">+1"), "x.md starts folded over z.md");
}

#[test]
fn space_folds_and_unfolds_the_selected_node_and_h_never_folds() {
    let r = with_siblings();
    r.press('h');
    let (_, selected, items) = graph_state(&r);
    assert!(selected.ends_with("doc.md"), "h moved to the parent");
    assert_eq!(items, 4, "and folded nothing");
    r.view.clear();
    r.press_key(Key::Space);
    assert!(r.view.evaled("update("));
    assert_eq!(
        graph_state(&r).2,
        2,
        "the siblings fold away, the route stays"
    );
    assert!(graph_state(&r).1.ends_with("doc.md"));
    r.press_key(Key::Space);
    assert_eq!(graph_state(&r).2, 4);
    r.press('h');
    assert_eq!(graph_state(&r).2, 4, "h on the root: still nothing folds");
}

#[test]
fn l_unfolds_a_folded_node_and_enters_it() {
    let r = with_siblings();
    // other.md's column: x.md above it.
    r.press('k');
    assert!(graph_state(&r).1.ends_with("x.md"));
    r.view.clear();
    r.press('l');
    assert!(r.view.evaled("update("));
    let (_, selected, items) = graph_state(&r);
    assert!(selected.ends_with("z.md"), "{selected}");
    assert_eq!(items, 5);
}

#[test]
fn a_click_on_a_handle_toggles_that_node() {
    let r = with_siblings();
    r.message(message::GRAPH_FOLD, "0.1");
    let (_, selected, items) = graph_state(&r);
    assert!(selected.ends_with("x.md"), "the clicked node is selected");
    assert_eq!(items, 5, "and unfolded");
    r.message(message::GRAPH_FOLD, "0.1");
    assert_eq!(graph_state(&r).2, 4, "and folded again");
    r.message(message::GRAPH_FOLD, "0.9");
    assert_eq!(graph_state(&r).2, 4, "an unknown key is ignored");
    // A node without links has no handle; its "click" does nothing.
    r.message(message::GRAPH_FOLD, "0.3");
    assert_eq!(graph_state(&r).2, 4);
}

#[test]
fn enter_opens_a_sibling() {
    let r = with_siblings();
    r.press('k');
    r.view.clear();
    r.press_key(Key::Enter);
    let loads = r.view.loads();
    assert_eq!(loads.len(), 1);
    assert!(loads[0].1.ends_with("x.md"));
    assert_eq!(mode(&r), "normal");
}

#[test]
fn a_click_on_an_item_a_re_layout_removed_is_dropped() {
    let r = with_siblings();
    r.message(message::GRAPH_FOLD, "0.1");
    // z.md (node 4, under x) is on screen; the page is told so.
    r.message(message::GRAPH_SELECT, "0.1.4");
    assert!(graph_state(&r).1.ends_with("z.md"));
    r.message(message::GRAPH_FOLD, "0.1");
    r.press('j');
    assert!(graph_state(&r).1.ends_with("other.md"));
    // A click on z.md posted before the fold landed arrives after it: it
    // names nothing now, and must select neither whatever took its index nor
    // a stand-in for it.
    r.message(message::GRAPH_SELECT, "0.1.4");
    assert!(
        graph_state(&r).1.ends_with("other.md"),
        "the selection stays"
    );
    assert_eq!(mode(&r), "graph");
}

#[test]
fn the_toc_closes_the_graph_it_would_cover() {
    let r = with_graph();
    r.view.clear();
    r.execute_str("toggle toc", 1).unwrap();
    assert_eq!(mode(&r), "toc");
    assert!(r.view.evaled("close()"), "the overlay is gone");
    assert_eq!(graph_state(&r), (String::new(), String::new(), 0));
}

#[test]
fn toc_select_outside_the_toc_leaves_the_graph_alone() {
    let r = with_graph();
    r.execute_str("toc select", 1).unwrap();
    assert_eq!(mode(&r), "graph");
    assert_eq!(graph_state(&r).0, "links");
}

#[test]
fn ctrl_wheel_zooms_the_graph_and_not_the_page() {
    let r = with_graph();
    r.view.clear();
    r.wheel_zoom(-1.0, false);
    assert!(r.view.evaled("zoom("));
    assert!(r.view.zoom_levels().is_empty());
    r.press('+');
    assert!(r.view.zoom_levels().is_empty());
}

#[test]
fn ctrl_wheel_zooms_the_graph_about_the_pages_own_pointer() {
    // The shell's pointer is in toolkit px, which WebKit need not lay the page
    // out in (a 2:1 screen scale has been seen); converting it gave a zoom
    // that "shifts the pan". The graph zooms about the pointer the page itself
    // saw, so the controller passes no coordinates at all.
    let r = with_graph();
    r.controller.on_pointer_moved(612.0, 344.0);
    r.view.clear();
    r.wheel_zoom(-1.0, false);
    assert!(r.view.evaled(", 'pointer');"), "about the page's pointer");
    assert!(!r.view.evaled("612") && !r.view.evaled("306"));
    for key in ['-', '+'] {
        r.view.clear();
        r.press(key);
        assert!(
            r.view.evaled(", 'selection');"),
            "the keys zoom about the selection"
        );
    }
}

// ---------------------------------------------------------------------------
// Link hints
// ---------------------------------------------------------------------------

#[test]
fn f_prompts_for_a_link_and_draws_the_overlay() {
    let r = Reader::loaded(DOC);
    r.press('f');
    assert_eq!(r.chrome.message(), "follow link:");
    assert!(r.view.evaled(HINTS_OVERLAY_ID));
}

#[test]
fn a_hint_key_opens_a_markdown_link_in_the_window() {
    let r = Reader::loaded_with_sibling(DOC);
    r.press('f');
    r.post_hints(&[
        ("a", "https://example.com/".to_string()),
        ("b", file_uri(&r.sibling())),
    ]);
    r.press('b');
    let loads = r.view.loads();
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0].1, r.sibling());
}

#[test]
fn a_hint_key_hands_a_web_link_to_the_system() {
    let r = Reader::loaded(DOC);
    r.press('f');
    r.post_hints(&[("a", "https://example.com/".to_string())]);
    r.press('a');
    assert_eq!(r.host.opened(), vec!["https://example.com/".to_string()]);
    assert!(r.view.loads().is_empty());
}

#[test]
fn a_link_the_system_will_not_take_is_reported() {
    let r = Reader::loaded(DOC);
    r.host.fail_next_external("no handler for https");
    r.press('f');
    r.post_hints(&[("a", "https://example.com/".to_string())]);
    r.press('a');
    assert_eq!(
        r.chrome.message(),
        "cannot open https://example.com/: no handler for https"
    );
    assert!(r.host.opened().is_empty());
}

#[test]
fn show_mode_reports_the_target_instead_of_following_it() {
    let r = Reader::loaded(DOC);
    r.press('F');
    r.post_hints(&[("a", "https://example.com/".to_string())]);
    r.press('a');
    assert_eq!(r.chrome.message(), "→ https://example.com/");
    assert!(r.host.opened().is_empty());
    assert!(r.view.loads().is_empty());
}

#[test]
fn a_dead_end_hint_key_is_ignored() {
    let r = Reader::loaded(DOC);
    r.press('f');
    r.post_hints(&[("a", "https://example.com/".to_string())]);
    assert_eq!(r.press('z'), KeyOutcome::Consumed);
    assert_eq!(r.chrome.message(), "follow link: ");
    assert!(r.host.opened().is_empty());
}

#[test]
fn escape_clears_the_hint_overlay() {
    let r = Reader::loaded(DOC);
    r.press('f');
    r.post_hints(&[("a", "https://example.com/".to_string())]);
    r.view.clear();

    r.press_key(Key::Escape);
    // Recording restarted above, so the only script naming the overlay now is
    // the one that tears it down.
    assert!(r.view.evaled(HINTS_OVERLAY_ID));
    // And the overlay no longer eats keys.
    assert_eq!(r.press('z'), KeyOutcome::PassThrough);
}

// ---------------------------------------------------------------------------
// Jumplist
// ---------------------------------------------------------------------------

#[test]
fn ctrl_o_returns_to_the_position_the_jump_left() {
    let r = Reader::loaded(DOC);
    r.view.set_scroll_y(500.0);
    r.press('G');
    r.view.set_scroll_y(900.0);
    r.view.clear();

    r.press_ctrl('o');
    assert_eq!(scrolled_to_offsets(&r.view), vec![500.0]);
}

#[test]
fn ctrl_i_walks_the_jumplist_forward_again() {
    let r = Reader::loaded(DOC);
    r.view.set_scroll_y(500.0);
    r.press('G');
    r.view.set_scroll_y(900.0);
    r.press_ctrl('o');
    r.view.clear();

    r.press_ctrl('i');
    assert_eq!(scrolled_to_offsets(&r.view), vec![900.0]);
}

#[test]
fn the_breadcrumb_records_the_route_into_a_second_document() {
    let r = Reader::loaded_with_sibling(DOC);
    assert_eq!(r.chrome.trail(), vec!["doc.md".to_string()]);
    r.navigate(&file_uri(&r.sibling()));
    assert_eq!(
        r.chrome.trail(),
        vec!["doc.md".to_string(), "other.md".to_string()]
    );
}

// ---------------------------------------------------------------------------
// Quickmarks
// ---------------------------------------------------------------------------

#[test]
fn a_quickmark_restores_the_offset_it_was_set_at() {
    let r = Reader::loaded(DOC);
    r.view.set_scroll_y(250.0);
    r.press('m');
    r.press('a');
    assert_eq!(r.chrome.message(), "mark a set");

    r.view.set_scroll_y(800.0);
    r.view.clear();
    r.press('\'');
    r.press('a');
    assert_eq!(scrolled_to_offsets(&r.view), vec![250.0]);
}

#[test]
fn an_unset_quickmark_reports_rather_than_jumping() {
    let r = Reader::loaded(DOC);
    r.press('\'');
    r.press('z');
    assert_eq!(r.chrome.message(), "no mark z");
    assert!(scrolled_to_offsets(&r.view).is_empty());
}

// ---------------------------------------------------------------------------
// Input bar, completion, search
// ---------------------------------------------------------------------------

#[test]
fn colon_opens_the_command_line() {
    let r = Reader::loaded(DOC);
    r.press(':');
    assert_eq!(r.chrome.prompt(), Some(Prompt::Command));
}

#[test]
fn tab_completes_and_then_cycles_the_command_line() {
    let r = Reader::loaded(DOC);
    r.press(':');
    r.chrome.set_input_query("set s");
    r.press_key(Key::Tab);
    let first = r.chrome.input_query();
    r.press_key(Key::Tab);
    let second = r.chrome.input_query();
    assert!(first.starts_with("set s"), "{first:?}");
    assert!(second.starts_with("set s"), "{second:?}");
    assert_ne!(first, second);
}

#[test]
fn enter_runs_the_typed_command() {
    let r = Reader::loaded(DOC);
    r.press(':');
    r.submit_input("zoom in");
    assert_eq!(r.chrome.prompt(), None);
    let levels = r.view.zoom_levels();
    assert_eq!(levels.len(), 1);
    assert!(levels[0] > 1.0, "{levels:?}");
}

#[test]
fn slash_opens_a_search_and_enter_runs_it() {
    let r = Reader::loaded(DOC);
    r.press('/');
    assert_eq!(r.chrome.prompt(), Some(Prompt::Search));
    r.submit_input("beta");
    assert_eq!(r.chrome.prompt(), None);
    assert_eq!(
        find_calls(&r.view),
        vec![ViewCall::Find("beta".to_string())]
    );
}

#[test]
fn n_and_shift_n_step_through_the_matches() {
    let r = Reader::loaded(DOC);
    r.search("beta");
    r.press('n');
    r.press('N');
    assert_eq!(
        find_calls(&r.view),
        vec![
            ViewCall::Find("beta".to_string()),
            ViewCall::FindNext,
            ViewCall::FindPrevious,
        ]
    );
}

#[test]
fn escape_drops_the_active_search() {
    let r = Reader::loaded(DOC);
    r.search("beta");
    r.view.clear();
    r.press_key(Key::Escape);
    assert_eq!(find_calls(&r.view), vec![ViewCall::FindClear]);
}

// ---------------------------------------------------------------------------
// Zoom
// ---------------------------------------------------------------------------

#[test]
fn plus_and_minus_drive_the_zoom_level() {
    let r = Reader::loaded(DOC);
    r.press('+');
    r.press('-');
    let levels = r.view.zoom_levels();
    assert_eq!(levels.len(), 2);
    assert!((levels[0] - 1.1).abs() < 1e-9, "{levels:?}");
    assert!((levels[1] - 1.0).abs() < 1e-9, "{levels:?}");
}

#[test]
fn equals_resets_both_zoom_axes() {
    let r = Reader::loaded(DOC);
    r.press('+');
    r.view.clear();
    r.press('=');
    assert_eq!(r.view.zoom_levels(), vec![1.0]);
    let base = Options::default().font_size_px as f64;
    assert!((font_size_px(&r.view) - base).abs() < 1e-9);
}

#[test]
fn text_zoom_sets_the_font_size_in_pixels() {
    let r = Reader::loaded(DOC);
    let options = Options::default();
    r.execute(Action::TextZoomIn, 1);
    let expected = options.font_size_px as f64 * (1.0 + options.text_zoom_step);
    assert!((font_size_px(&r.view) - expected).abs() < 1e-9);
}

/// The `<html>` class list of the most recently loaded document.
fn loaded_classes(view: &FakeViewport) -> String {
    let (html, _) = view.loads().pop().expect("a loaded document");
    let (_, rest) = html.split_once("class=\"").expect("a class attribute");
    let (list, _) = rest.split_once('"').expect("a closed class attribute");
    list.to_string()
}

/// A script that can change a block's height must capture the reading anchor
/// before it and restore it after (DESIGN D5a.0). Cheap unit-level guard for
/// the invariant the `*_holds_the_reading_position` e2e tests prove end to end;
/// this one fails in milliseconds if a toggle is written unanchored.
fn assert_anchored(what: &str, script: &str) {
    assert!(
        script.contains("__jmnj_anchor"),
        "{what}: not anchored — a height change must capture the reading \
         position first (DESIGN D5a.0): {script}"
    );
    assert!(
        script.contains("scrollBy"),
        "{what}: captures an anchor but never restores it: {script}"
    );
    let capture = script.find("__jmnj_anchor").expect("capture present");
    let restore = script.rfind("scrollBy").expect("restore present");
    assert!(
        capture < restore,
        "{what}: the anchor must be captured before the change, not after: {script}"
    );
}

#[test]
fn s_flips_the_wide_block_class_and_leaves_the_document_alone() {
    // Opened but not yet finished, so the load is still on the recording: the
    // breakout ships on, so the document went out carrying the master class and
    // the first press must turn it off.
    let r = Reader::open(DOC);
    assert_eq!(
        loaded_classes(&r.view),
        "jmnj-wide jmnj-wide-diagrams jmnj-wide-fences jmnj-wide-tables"
    );
    r.finish_load();
    assert!(r.state().contains("\"wide\":true"));

    r.press('s');
    let script = eval_containing(&r.view, "jmnj-wide");
    assert!(
        script.contains("document.documentElement.classList.toggle('jmnj-wide', false);"),
        "the toggle should flip the master class: {script}"
    );
    assert_anchored("s / ToggleWide", &script);
    // The point of the class model: eligibility is already in the document, so
    // the toggle costs no re-render. It still moves content — see
    // `assert_anchored` above and DESIGN D5a.0.
    assert!(
        r.view.loads().is_empty(),
        "the toggle re-rendered: {:?}",
        r.view.loads()
    );
    assert!(r.state().contains("\"wide\":false"));

    r.view.clear();
    r.press('s');
    let back = eval_containing(&r.view, "jmnj-wide");
    assert!(
        back.contains("document.documentElement.classList.toggle('jmnj-wide', true);"),
        "the toggle should flip the master class back: {back}"
    );
    assert_anchored("s / ToggleWide (back on)", &back);
    assert!(r.state().contains("\"wide\":true"));
}

// ---------------------------------------------------------------------------
// Diagram fit and per-diagram zoom (DESIGN D5a.2)
// ---------------------------------------------------------------------------

/// The per-diagram scale the page was told to apply, and the diagram it was
/// aimed at: the whole contract of one Ctrl+wheel tick over a diagram.
fn diagram_zoom_call(view: &FakeViewport) -> (usize, f64) {
    let js = eval_containing(view, DIAGRAM_ZOOM_VAR);
    let index = number_after(&js, ".mermaid')[").expect("a diagram index") as usize;
    let factor = number_after(&js, "base * ").expect("a scale factor");
    (index, factor)
}

#[test]
fn a_flips_the_diagram_fit_class_and_leaves_the_document_alone() {
    // Fit ships off (intrinsic is D5a's decided behaviour), so the document
    // went out without the class and the first press must turn it on.
    let r = Reader::open(DOC);
    assert!(!loaded_classes(&r.view).contains(DIAGRAM_FIT_CLASS));
    r.finish_load();
    assert!(r.state().contains("\"diagram_fit\":false"));

    r.press('a');
    let script = eval_containing(&r.view, DIAGRAM_FIT_CLASS);
    assert!(
        script.contains("document.documentElement.classList.toggle('jmnj-diagram-fit', true);"),
        "the toggle should flip the fit class: {script}"
    );
    assert_anchored("a / ToggleDiagramFit", &script);
    // Same contract `s` has: the rule is already in the stylesheet, so the
    // toggle costs no re-render.
    assert!(
        r.view.loads().is_empty(),
        "the toggle re-rendered: {:?}",
        r.view.loads()
    );
    assert!(r.state().contains("\"diagram_fit\":true"));

    r.view.clear();
    r.press('a');
    let back = eval_containing(&r.view, DIAGRAM_FIT_CLASS);
    assert!(
        back.contains("document.documentElement.classList.toggle('jmnj-diagram-fit', false);"),
        "the toggle should flip the fit class back: {back}"
    );
    assert_anchored("a / ToggleDiagramFit (back off)", &back);
    assert!(r.state().contains("\"diagram_fit\":false"));
}

#[test]
fn a_re_render_keeps_whatever_the_diagram_fit_toggle_last_set() {
    // The mirror into the render options, the same one `s` needs: without it a
    // live reload would quietly drop fit mode.
    let r = Reader::loaded(DOC);
    r.press('a');
    r.view.clear();
    r.press('r');
    r.land_work();
    assert!(
        loaded_classes(&r.view).contains(DIAGRAM_FIT_CLASS),
        "the re-rendered document must still carry the fit class: {:?}",
        loaded_classes(&r.view)
    );
    assert!(r.state().contains("\"diagram_fit\":true"));
}

#[test]
fn ctrl_wheel_over_a_diagram_scales_that_diagram_and_not_the_page() {
    let r = Reader::loaded(DOC);
    // The page reports the pointer entering the second diagram's box.
    r.message(message::DIAGRAM_HOVER, "1");
    assert!(r.state().contains("\"diagram_hover\":true"));

    r.wheel_zoom(-1.0, false);
    let (index, factor) = diagram_zoom_call(&r.view);
    assert_eq!(index, 1, "the tick must land on the hovered diagram");
    let step = 1.0 + Options::default().zoom_step;
    assert!((factor - step).abs() < 1e-9, "scroll up zooms in: {factor}");
    assert!(
        r.view.zoom_levels().is_empty(),
        "the page must not zoom under the pointer: {:?}",
        r.view.zoom_levels()
    );

    // Out is the reciprocal of in, so the two exactly undo each other.
    r.view.clear();
    r.wheel_zoom(1.0, false);
    let (_, out) = diagram_zoom_call(&r.view);
    assert!(
        (out - 1.0 / step).abs() < 1e-9,
        "scroll down zooms out: {out}"
    );

    // And the script clamps, so no burst can run away.
    let js = eval_containing(&r.view, DIAGRAM_ZOOM_VAR);
    assert!(js.contains(&DIAGRAM_ZOOM_MIN.to_string()));
    assert!(js.contains(&DIAGRAM_ZOOM_MAX.to_string()));
}

#[test]
fn ctrl_wheel_away_from_a_diagram_still_zooms_the_page() {
    let r = Reader::loaded(DOC);
    // Entered a diagram, then left it: the page posts the empty payload, which
    // is how "over no diagram" is spelled.
    r.message(message::DIAGRAM_HOVER, "0");
    r.message(message::DIAGRAM_HOVER, "");
    assert!(r.state().contains("\"diagram_hover\":false"));
    r.view.clear();

    r.wheel_zoom(-1.0, false);
    let levels = r.view.zoom_levels();
    assert_eq!(levels.len(), 1, "{levels:?}");
    assert!((levels[0] - 1.1).abs() < 1e-9, "{levels:?}");
    assert!(
        !r.view.evaled(DIAGRAM_ZOOM_VAR),
        "no diagram was under the pointer: {:?}",
        r.view.evals()
    );
}

#[test]
fn ctrl_shift_wheel_over_a_diagram_is_still_text_zoom() {
    // The hover flag routes the *geometric* axis only: Ctrl+Shift is the text
    // axis, which has nothing to do with diagram size.
    let r = Reader::loaded(DOC);
    r.message(message::DIAGRAM_HOVER, "0");
    r.view.clear();

    r.wheel_zoom(-1.0, true);
    let options = Options::default();
    let expected = options.font_size_px as f64 * (1.0 + options.text_zoom_step);
    assert!((font_size_px(&r.view) - expected).abs() < 1e-9);
    assert!(!r.view.evaled(DIAGRAM_ZOOM_VAR));
}

#[test]
fn zoom_reset_clears_the_per_diagram_scales_too() {
    // D5a makes `=` the reset for zoom as a whole; a diagram left at 4× after
    // it would be a lie.
    let r = Reader::loaded(DOC);
    r.message(message::DIAGRAM_HOVER, "0");
    r.wheel_zoom(-1.0, false);
    r.view.clear();

    r.press('=');
    let js = eval_containing(&r.view, "removeProperty");
    assert!(js.contains(DIAGRAM_ZOOM_VAR), "{js}");
    assert!(js.contains(DIAGRAM_ZOOM_CLASS), "{js}");
    // Under the same anchor as the two zoom axes, not a second one.
    assert!(js.contains("--font-size"), "{js}");
    assert_eq!(r.view.zoom_levels(), vec![1.0]);
}

#[test]
fn every_document_installs_the_pointer_over_diagram_listener() {
    // The flag has to come from the page, because GTK sees the scroll first
    // (D4) — so the listener belongs in the permanent document-start set, not
    // in something a flow has to remember to install.
    let installed = super::scripts::document_start()
        .iter()
        .any(|js| js.contains(message::DIAGRAM_HOVER) && js.contains(".mermaid"));
    assert!(
        installed,
        "no pointer-over-diagram listener at document start"
    );
}

#[test]
fn every_document_holds_its_place_across_a_resize() {
    // A resize has no "before" the controller sees, so the anchor has to be
    // kept by the page itself — at document start, on every document.
    let installed = super::scripts::document_start()
        .iter()
        .any(|js| js.contains("'resize'") && js.contains("caretRangeFromPoint"));
    assert!(installed, "no resize anchor at document start");
}

#[test]
fn a_re_render_keeps_whatever_the_wide_toggle_last_set() {
    // The mirror into the render options: without it, the next re-render (a
    // live reload, `r`, a `:set`) would quietly put the breakout back on.
    let r = Reader::loaded(DOC);
    r.press('s');
    r.view.clear();
    r.press('r');
    r.land_work();
    assert_eq!(
        loaded_classes(&r.view),
        "jmnj-wide-diagrams jmnj-wide-fences jmnj-wide-tables",
        "the re-rendered document must not carry the master class again"
    );
    assert!(r.state().contains("\"wide\":false"));
}

#[test]
fn setting_wide_blocks_re_renders_with_the_new_eligibility() {
    let r = Reader::loaded(DOC);
    r.view.clear();
    r.press(':');
    r.submit_input("set wide-blocks all");
    r.land_work();
    assert_eq!(
        loaded_classes(&r.view),
        "jmnj-wide jmnj-wide-diagrams jmnj-wide-fences jmnj-wide-tables \
         jmnj-wide-code jmnj-wide-math"
    );
}

#[test]
fn a_burst_of_wheel_zoom_ticks_coalesces_into_two_applies() {
    let r = Reader::loaded(DOC);
    for _ in 0..3 {
        r.wheel_zoom(-1.0, false);
    }
    // The leading tick applied at once; the other two wait for the window.
    let levels = r.view.zoom_levels();
    assert_eq!(levels.len(), 1);
    assert!((levels[0] - 1.1).abs() < 1e-9, "{levels:?}");

    r.run_timers();
    let levels = r.view.zoom_levels();
    assert_eq!(levels.len(), 2);
    assert!((levels[1] - 1.3).abs() < 1e-9, "{levels:?}");
}

// ---------------------------------------------------------------------------
// The deferred initial render
// ---------------------------------------------------------------------------

#[test]
fn a_plain_document_renders_at_construction() {
    let r = Reader::open(DOC);
    assert_eq!(r.view.loads().len(), 1);
    assert_eq!(r.host.armed_timers(), 0);
}

#[test]
fn a_vault_document_waits_for_the_scan_to_land() {
    let r = Reader::open(VAULT_DOC);
    assert!(r.view.loads().is_empty());
    assert_eq!(r.host.armed_timers(), 1);
    r.land_work();
    assert_eq!(r.view.loads().len(), 1);
}

#[test]
fn the_failsafe_renders_when_the_scan_has_not_landed() {
    let r = Reader::open(VAULT_DOC);
    r.run_timers();
    assert_eq!(r.view.loads().len(), 1);
}

#[test]
fn a_panicked_scan_still_renders_the_document_once() {
    let r = Reader::open(VAULT_DOC);
    r.land_work_panicked();
    r.run_timers();
    assert_eq!(r.view.loads().len(), 1);
}

#[test]
fn the_failsafe_does_not_render_again_after_the_scan_has_landed() {
    let r = Reader::open(VAULT_DOC);
    r.land_work();
    // The failsafe fires into a session that has already rendered.
    r.run_timers();
    assert_eq!(r.view.loads().len(), 1);
}

#[test]
fn a_scan_landing_after_the_failsafe_re_renders_against_the_real_index() {
    let r = Reader::open(VAULT_DOC);
    r.run_timers();
    assert_eq!(r.view.loads().len(), 1);
    // The failsafe rendered against the empty index the session started with,
    // so this document's `[[…]]` resolved against nothing. A scan that lands
    // afterwards with a different index must re-render — the second render the
    // deferral exists to avoid, paid only when the scan was too slow.
    r.land_work();
    assert_eq!(r.view.loads().len(), 2);
}

#[test]
fn opening_a_second_document_re_points_the_live_reload_watcher() {
    let r = Reader::loaded_with_sibling(DOC);
    assert_eq!(r.host.timer_events(), vec![TimerEvent::Started(0)]);

    r.navigate(&file_uri(&r.sibling()));
    // The watcher for the new document starts before the old one is dropped,
    // and exactly one survives: what is watched followed what is read.
    assert_eq!(
        r.host.timer_events(),
        vec![
            TimerEvent::Started(0),
            TimerEvent::Started(1),
            TimerEvent::Cancelled(0),
        ]
    );
    assert_eq!(r.host.live_intervals(), 1);
}

// ---------------------------------------------------------------------------
// The automation surface
// ---------------------------------------------------------------------------

#[test]
fn execute_str_applies_the_count() {
    let r = Reader::loaded(DOC);
    r.execute_str("scroll down", 2).expect("known action");
    assert_eq!(scrolled_by(&r.view), (0, 2 * scroll_step()));
}

#[test]
fn execute_str_rejects_an_unknown_action() {
    let r = Reader::loaded(DOC);
    assert!(r.execute_str("fly up", 1).is_err());
    assert!(r.view.evals().is_empty());
}

#[test]
fn state_reports_the_document_the_mode_and_the_load_flag() {
    let r = Reader::loaded(DOC);
    let v: serde_json::Value = serde_json::from_str(&r.state()).expect("state is JSON");
    assert_eq!(v["loaded"], true);
    assert_eq!(v["mode"], "normal");
    assert_eq!(v["file"], r.file.to_string_lossy().as_ref());
}

#[test]
fn goto_source_line_does_nothing_before_the_load_finishes() {
    let r = Reader::open(DOC);
    r.view.clear();
    r.goto_source_line(3);
    assert!(r.view.evals().is_empty());
}

#[test]
fn goto_source_line_looks_up_the_nearest_source_element() {
    let r = Reader::loaded(DOC);
    r.goto_source_line(3);
    // The contract is the line looked for, through the shared source-position
    // search — not the wording of the query it expands to.
    assert!(r.view.evaled(&nearest_source_element_js("3")));
}

// ---------------------------------------------------------------------------
// Messages posted from inside the document
// ---------------------------------------------------------------------------

#[test]
fn a_selection_message_reaches_the_configured_clipboard() {
    let r = Reader::loaded(DOC);
    r.message(message::SELECTION, "picked text");
    assert_eq!(
        r.host.copied(),
        vec![("picked text".to_string(), SelectionClipboard::Primary)]
    );
}

#[test]
fn an_empty_selection_message_copies_nothing() {
    let r = Reader::loaded(DOC);
    r.message(message::SELECTION, "");
    assert!(r.host.copied().is_empty());
}

#[test]
fn a_scroll_message_paints_the_status_without_asking_the_page() {
    let r = Reader::loaded(DOC);
    r.message(message::SCROLL, "42 1234.5");
    assert_eq!(r.chrome.status_right().percent, 42);
    // No round trip: the payload already carried everything the bar needed.
    assert!(r.view.calls().is_empty());
}

#[test]
fn an_editor_sync_message_spawns_the_editor_at_the_clicked_line() {
    let r = Reader::open_full(DOC, editor_options(), |_| {});
    r.finish_load();
    r.message(message::EDITOR_SYNC, "12");
    assert_eq!(
        r.host.spawned(),
        vec![vec![
            "my-editor".to_string(),
            "+12".to_string(),
            r.file.to_string_lossy().into_owned(),
        ]]
    );
    assert_eq!(r.chrome.message(), "editor: my-editor at line 12");
}

#[test]
fn an_editor_that_will_not_spawn_is_reported() {
    let r = Reader::open_full(DOC, editor_options(), |_| {});
    r.finish_load();
    r.host.fail_next_spawn("no such program");
    r.message(message::EDITOR_SYNC, "12");
    assert_eq!(r.chrome.message(), "editor-command failed: no such program");
    assert!(r.host.spawned().is_empty());
}

#[test]
fn an_unrecognised_message_is_ignored() {
    let r = Reader::loaded(DOC);
    r.message("nonsense", "payload");
    assert!(r.view.calls().is_empty());
    assert!(r.host.copied().is_empty());
}

/// Options naming an editor that exists only in the fake's recording.
fn editor_options() -> Options {
    Options {
        editor_command: EditorCommand::parse("my-editor +%l %f").expect("template parses"),
        ..Options::default()
    }
}

// ---------------------------------------------------------------------------
// Close
// ---------------------------------------------------------------------------

#[test]
fn closing_writes_the_reading_position_to_history() {
    let r = Reader::open(DOC);
    r.view.set_scroll_y(400.0);
    r.finish_load();
    r.controller.on_close();

    let toml = std::fs::read_to_string(r.data_dir.join("history.toml")).expect("history written");
    assert!(toml.contains(r.file.to_string_lossy().as_ref()), "{toml}");
    assert!(toml.contains("scroll_y = 400"), "{toml}");
}
