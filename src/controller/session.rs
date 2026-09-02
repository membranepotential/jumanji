//! The reader session: every piece of the running window that is not a widget.
//!
//! [`Controller`] is the whole state machine — document, vault, zoom, modes,
//! jumplist, marks, hints, completion, history — plus the flows that drive it
//! (render and load, action execution, link routing, the `:` command line). It
//! is generic over a [`Toolkit`], so the only thing it knows about GTK, WebKit
//! or macOS is the three traits in [`toolkit`](crate::controller::toolkit).
//!
//! A shell owns exactly two jobs: build the widgets, and adapt the toolkit's
//! events into the handful of `on_*` methods below (plus the automation
//! surface `execute`/`state`/`goto_source_line`, which D-Bus uses today and
//! anything could use tomorrow). Nothing else about the reader's behaviour
//! lives outside this file.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::controller::page::{InitialPosition, Page, ViewportState, ZoomAnchor};
use crate::controller::scripts::message;
use crate::controller::stdin::StdinReader;
use crate::controller::toolkit::{Chrome, Host, Prompt, Toolkit, Viewport};
use crate::controller::watch::{FileEvent, Watch};
use crate::core::command::{self, Command, Completions};
use crate::core::config::{self, Options, SetEffect};
use crate::core::editor::EditorCommand;
use crate::core::history::{FileState, History};
use crate::core::jumplist::{Jumplist, Location};
use crate::core::keymap::{Key, KeyPress, Keymap, MatchResult, Matcher};
use crate::core::marks::{Marks, Position};
use crate::core::obsidian;
use crate::core::pipeline::{self, Options as RenderOptions};
use crate::core::source::Source;
use crate::core::vault::{self, Vault, VaultIndex};
use crate::core::{Action, Direction, Heading, Mode};

/// Which link-hint action is pending (mirrors `f` vs `F`).
#[derive(Debug, Clone, Copy)]
enum HintKind {
    /// `f` — follow the chosen link (route it through [`Controller::on_navigate`]).
    Follow,
    /// `F` — only report the chosen link's target in the statusbar.
    Show,
}

/// One labelled link in the hint overlay.
#[derive(Debug, Clone)]
struct HintLink {
    label: String,
    href: String,
}

/// Interaction state that sits *outside* the keymap modes: the link-hint
/// overlay intercepts keys directly (not via a `Mode`), the way the input bar
/// does. Everything else is `None`.
enum Input {
    None,
    /// The hint overlay is active. `links` is filled asynchronously when the
    /// overlay JS posts its label→href map back.
    Hint {
        kind: HintKind,
        typed: String,
        links: Vec<HintLink>,
    },
}

/// An in-progress tab-completion cycle for the `:` command line.
struct Completion {
    candidates: Vec<String>,
    index: usize,
}

/// Whether a key the shell handed to [`Controller::on_key`] was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    /// The controller acted on it (or deliberately swallowed it); the shell
    /// must stop the event.
    Consumed,
    /// Nothing here wanted it; the shell lets it reach the document.
    PassThrough,
}

/// Mutable session state, shared across every callback the controller hands a
/// toolkit. Reached only through [`Controller`].
struct Session<T: Toolkit> {
    /// The document's base path: the real file, or — for a stdin stream — a
    /// sentinel under the current directory (`<cwd>/stdin.md`) so document-
    /// relative images and `.md` links resolve against the CWD, which is what a
    /// pipe user expects. Never read/written for stdin (content comes from
    /// [`stdin_buffer`](Self::stdin_buffer)).
    file: PathBuf,
    /// The content buffer when reading stdin (`Some` ⇒ this is a stdin
    /// document). The reader thread appends bytes; renders snapshot it. `None`
    /// for a file document.
    stdin_buffer: Option<Arc<Mutex<Vec<u8>>>>,
    /// Live options, mutated by `:set`; the source of truth the derived render
    /// options and step fields are re-synced from.
    options: Options,
    render_opts: RenderOptions,
    /// The bindings this session dispatches keys through (`mode × count ×
    /// key-seq → Action`). Fixed for the process: config is read once at
    /// launch.
    keymap: Keymap,
    /// The vault root (DESIGN D11), resolved once from the document jumanji was
    /// launched with and pinned for the process. Pinned rather than recomputed
    /// so that following a wikilink into a subfolder cannot narrow the vault
    /// under the reader — you opened a collection, not a directory.
    vault_root: PathBuf,
    /// Per-document link resolution (DESIGN D11): the index of
    /// [`vault_root`](Self::vault_root), bound to this document. Rescanned in
    /// the background on every document load and on `r`, never by the
    /// watch-driven live reload — editing a note cannot rename another one, so
    /// a re-render reuses the index it was loaded with.
    vault: Vault,
    /// Whether a background scan is in flight. Keeps a burst of document loads
    /// from piling up redundant scans of the same pinned root, and keeps two
    /// snapshots from landing out of order.
    vault_scanning: bool,
    /// Whether the rendered document contains any `[[…]]` at all. A landed
    /// rescan re-renders only if it does — which is what keeps the pipeline off
    /// the hot path for the ordinary markdown file that names nothing.
    doc_uses_vault: bool,
    /// Set at construction when the launch document may reference the vault
    /// (`vault::may_reference_vault`), and cleared by the *first render,
    /// whoever triggers it* — normally the scan landing (the fast path) or
    /// [`INITIAL_RENDER_FAILSAFE`] (the pathological one), but equally a
    /// user's `r`, `:open`, or link follow arriving inside the deferral
    /// window. Consumed at the top of [`Controller::do_render_and_load`]
    /// rather than at each trigger site, so no render path can forget to
    /// cancel the deferral and leave a stale flag that fires a redundant
    /// render later. While true, the window is up but nothing has been
    /// rendered yet: [`Controller::new`] skipped its usual closing
    /// `do_render_and_load` so the *first* render can use the freshly landed
    /// index instead of the empty one it started with, which is what saves a
    /// second full render on every vault document at startup. `false` for a
    /// stdin source (no vault) and for any file whose initial
    /// `may_reference_vault` check came back negative.
    initial_render_deferred: bool,
    /// Reverse editor-sync template (DESIGN D7): spawned on Ctrl+click with
    /// `%l`/`%f` substituted. Config-only (copied from options at construction).
    editor_command: EditorCommand,
    /// Where the load now in flight opens: a remembered offset, a link fragment
    /// (`other.md#section`, a resolved `[[Note#H]]`, DESIGN D11), or a
    /// `--forward <line>` (DESIGN D7). Armed by whoever initiates the load —
    /// which is where the precedence between the three is resolved, once — and
    /// carried *into* [`Page::load_document`], which lands it before the first
    /// painted frame rather than after the load finishes.
    ///
    /// It stays armed until that load finishes rather than being consumed at
    /// initiation, because until then it is the only record of where the reader
    /// asked to be: a re-render that lands mid-flight (a vault rescan) must
    /// carry the same position rather than capture the pre-restore live scroll.
    pending_position: InitialPosition,
    /// XDG config base (`…/.config`); themes live under `<it>/jumanji/themes`.
    config_dir: Option<PathBuf>,
    /// Data dir (`…/.local/share/jumanji`); holds `history.toml`.
    data_dir: Option<PathBuf>,
    scroll_step: i64,
    /// Geometric zoom step (added to the engine `zoom_level` per step).
    zoom_step: f64,
    /// Text-zoom step: fraction of the base font size added per step.
    text_zoom_step: f64,
    /// Base body font size in px (text-zoom 100% reference; from config).
    font_base_px: f64,
    /// Current geometric zoom factor (1.0 = 100%). The session owns the intended
    /// level; the viewport is driven to match. Kept here (rather than read back
    /// from the engine) because anchored zoom sets the native level in an async
    /// callback, so the engine's own value briefly lags the intent.
    ///
    /// **Session-scoped, not per-document** (D5a): seeded once from `history` when
    /// the window is built — the only cold start there is — and thereafter owned
    /// by the session, so it carries unchanged across every document switch. The
    /// per-file value in `history` is the *default on open*, consulted only at
    /// that cold start.
    zoom: f64,
    /// Current text-zoom factor (1.0 = 100%). Session-scoped exactly like
    /// [`Session::zoom`].
    text_zoom: f64,
    /// Last pointer position in **viewport** logical px, as the shell reports it
    /// (see [`Controller::on_pointer_moved`]) — already translated out of window
    /// coordinates, so anchoring a wheel zoom at the cursor needs nothing but the
    /// zoom divisor.
    pointer: (f64, f64),
    /// Ctrl+wheel ticks accumulated but not yet applied (+ = zoom in). Coalesced
    /// so a rapid burst becomes one anchored reflow, not one per tick.
    pending_zoom_steps: i32,
    /// Whether a coalesced wheel-zoom flush is already queued on the main loop.
    zoom_flush_scheduled: bool,
    matcher: Matcher,
    /// Mirror of the matcher's mode (the matcher does not expose a getter);
    /// kept in lockstep by every `set_mode` call so `GetState` can report it.
    mode: Mode,
    view: T::Viewport,
    /// Status line, input bar, and the content↔TOC stack.
    chrome: T::Chrome,
    /// Timers, worker threads, and the operating system.
    host: T::Host,
    toc: Vec<Heading>,
    section: usize,
    dark: bool,
    /// Whether the initial load has finished. Key/automation actions are no-ops
    /// before this; the D-Bus `loaded` flag lets clients (tests, editor
    /// integrations) wait for a driveable window.
    loaded: bool,
    /// Last observed scroll offset, refreshed on every status update. Read
    /// synchronously on window-close to flush history without an async query.
    last_scroll: f64,
    /// Link-hint / other out-of-band interaction state.
    input: Input,
    /// Pending `:`-completion cycle, if any.
    completion: Option<Completion>,
    /// Jumplist for `Ctrl-o` / `Ctrl-i` (per document; reset on `:open`).
    jumplist: Jumplist,
    /// Quickmark registers `m<x>` / `'<x>` (per document; reset on `:open`).
    marks: Marks,
    /// Per-file window-state, loaded at startup and flushed on close/switch.
    history: History,
    _watch: Option<Watch<T::Host>>,
    _theme_watch: Option<Watch<T::Host>>,
    /// The stdin reader thread + poll source, for a stdin document. Dropping it
    /// stops the streaming updates.
    _stdin: Option<StdinReader<T::Host>>,
}

impl<T: Toolkit> Session<T> {
    /// Whether this is a stdin (streaming) document rather than a file.
    fn is_stdin(&self) -> bool {
        self.stdin_buffer.is_some()
    }
}

/// The reader session behind a shared handle: cloning one is cloning the
/// handle, which is what every callback the controller installs captures.
pub struct Controller<T: Toolkit>(Rc<RefCell<Session<T>>>);

impl<T: Toolkit> Clone for Controller<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// The document base path for `source`: the real file, or a sentinel under the
/// current directory for stdin so relative images/links resolve against the CWD.
fn base_path(source: &Source) -> PathBuf {
    match source {
        Source::File(path) => path.clone(),
        Source::Stdin => std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("stdin.md"),
    }
}

/// How long the deferred initial render (see
/// [`initial_render_deferred`](Session::initial_render_deferred)) waits on the
/// vault scan before giving up and rendering anyway. The window itself is
/// already on screen by the time this is armed — this only bounds how long the
/// *content* waits, so a pathological vault (a slow mount, a huge tree) cannot
/// leave the reader looking blank indefinitely.
const INITIAL_RENDER_FAILSAFE: std::time::Duration = std::time::Duration::from_millis(250);

/// Trailing-window for Ctrl+wheel zoom coalescing. The first tick of a burst
/// applies immediately (leading edge, so a single tick feels instant); ticks
/// arriving within this window after it are batched into one further anchored
/// reflow. Long enough that a physical burst collapses, short enough to feel
/// immediate.
const WHEEL_ZOOM_COALESCE: std::time::Duration = std::time::Duration::from_millis(40);

impl<T: Toolkit + 'static> Controller<T> {
    /// Build the session around widgets the shell has already created, and take
    /// the reader all the way to its first render: saved window-state, the
    /// opening position, the background vault scan, live reload (or the stdin
    /// stream), the theme watcher, and the initial render — deferred when the
    /// document may reference the vault.
    ///
    /// `forward` is an optional `--forward <line>` to open at (file sources
    /// only; rejected for stdin long before we get here).
    pub fn new(
        view: T::Viewport,
        chrome: T::Chrome,
        host: T::Host,
        source: Source,
        options: Options,
        keymap: Keymap,
        forward: Option<u32>,
    ) -> Self {
        let is_stdin = source.is_stdin();
        let file = base_path(&source);
        // Cheap, best-effort check of whether this launch is worth deferring the
        // initial render for (see `initial_render_deferred`). Re-read by
        // `do_render_and_load` when the render actually happens — a second read
        // of one small file at startup is nothing next to the render it lets us
        // skip. Stdin never defers: it has no vault, and its content isn't even
        // available yet (the reader thread starts below).
        let defer_initial_render = !is_stdin
            && std::fs::read_to_string(&file)
                .map(|md| vault::may_reference_vault(&md))
                .unwrap_or(false);

        chrome.set_trail(vec![source.display_name()]);

        let config_dir = config::xdg_config_dir();
        let data_dir = xdg_data_dir();
        let history = load_history(&data_dir);

        // Resolved once, from the document we were launched with, and pinned for
        // the process (DESIGN D11).
        let vault_root = vault::root_for(&file);

        let this = Self(Rc::new(RefCell::new(Session {
            file: file.clone(),
            stdin_buffer: None,
            options: options.clone(),
            render_opts: RenderOptions {
                page_width_px: options.page_width_px,
                show_frontmatter: options.show_frontmatter,
                font_body: options.font_body.clone(),
                font_mono: options.font_mono.clone(),
                font_size_px: options.font_size_px,
                // Populated per-render from `<config>/jumanji/themes/*.css`.
                extra_css: Vec::new(),
                // External fence renderers are config-only (no runtime `:set`), so
                // copy them once here; live reload re-runs the pipeline (and thus
                // the renderers) for free.
                renderers: options.renderers.clone(),
            },
            keymap,
            // Empty until the first background scan lands (kicked off below). A
            // launch never blocks on the walk: the window and the web engine's
            // processes come up while it runs, and for a vault big enough for the
            // difference to be visible that is the whole point.
            vault: Vault::new(VaultIndex::build(vault_root.clone(), Vec::new()), &file),
            vault_root,
            vault_scanning: false,
            doc_uses_vault: false,
            initial_render_deferred: defer_initial_render,
            editor_command: options.editor_command.clone(),
            // Resolved just below, once the saved window-state has been read.
            pending_position: InitialPosition::Top,
            config_dir,
            data_dir,
            scroll_step: options.scroll_step_px as i64,
            zoom_step: options.zoom_step,
            text_zoom_step: options.text_zoom_step,
            font_base_px: options.font_size_px as f64,
            zoom: 1.0,
            text_zoom: 1.0,
            pointer: (0.0, 0.0),
            pending_zoom_steps: 0,
            zoom_flush_scheduled: false,
            matcher: Matcher::new(Mode::Normal),
            mode: Mode::Normal,
            view,
            chrome,
            host,
            toc: Vec::new(),
            section: 0,
            dark: options.default_recolor,
            loaded: false,
            last_scroll: 0.0,
            input: Input::None,
            completion: None,
            jumplist: Jumplist::new(),
            marks: Marks::new(),
            history,
            _watch: None,
            _theme_watch: None,
            _stdin: None,
        })));

        // Restore any saved window-state for this file so the first painted frame is
        // already at the right place — zoom natively, scroll and text zoom through
        // the initial load itself. Read the value out before taking the mutable
        // borrow (avoid a reentrant borrow). Skipped for stdin: a stream has no
        // stable identity to key history on.
        //
        // This is the *only* place the saved zoom is read: the window's cold start,
        // where there is no live session zoom to inherit. Every later document switch
        // carries the session's zoom instead (D5a, `load_document`).
        let saved = if is_stdin {
            None
        } else {
            this.0.borrow().history.get(&file)
        };
        {
            let mut s = this.0.borrow_mut();
            if let Some(st) = &saved {
                s.text_zoom = st.text_zoom;
                s.zoom = st.zoom;
                s.view.set_zoom(st.zoom);
            }
            // The launch's opening position, precedence resolved once: an editor
            // that said `--forward <line>` pointed at that line deliberately, so it
            // outranks wherever this file was last left off.
            s.pending_position = match (forward, &saved) {
                (Some(line), _) => InitialPosition::SourceLine(line),
                (None, Some(st)) => InitialPosition::Offset(st.scroll_y),
                (None, None) => InitialPosition::Top,
            };
        }

        // First, so the walk overlaps everything below it — the watchers, the
        // window going up, and the engine spawning its processes. On any vault
        // the scan lands well before the initial load finishes; on a pathological
        // one the reader is usable meanwhile.
        this.rescan_vault();
        // A stdin document streams from a reader thread; a file document watches the
        // filesystem for live reload. The two are mutually exclusive.
        if is_stdin {
            this.start_stdin();
        } else {
            this.start_watch();
        }
        this.start_theme_watch();

        // Initial render + load — deferred when the document may reference the
        // vault, so it can render once against the freshly landed index instead
        // of twice (once against the empty one, again when the scan lands). See
        // `initial_render_deferred`.
        if defer_initial_render {
            this.arm_initial_render_failsafe();
        } else {
            this.do_render_and_load();
        }
        this
    }

    /// Arm the failsafe for a deferred initial render: if the scan hasn't landed
    /// (and rendered) within [`INITIAL_RENDER_FAILSAFE`], render anyway against
    /// whatever index is in hand — normally still the empty one the session was
    /// constructed with, which degrades to today's behaviour (a second render
    /// when the scan does land, see [`Controller::rescan_vault`]).
    fn arm_initial_render_failsafe(&self) {
        let host = self.0.borrow().host.clone();
        let this = self.clone();
        host.defer(INITIAL_RENDER_FAILSAFE, move || {
            let still_deferred = {
                let mut s = this.0.borrow_mut();
                std::mem::take(&mut s.initial_render_deferred)
            };
            if still_deferred {
                this.do_render_and_load();
            }
        });
    }

    /// One message posted from inside the document (see
    /// [`scripts::message`](crate::controller::scripts::message)), demultiplexed
    /// by name. An unrecognised name is ignored.
    pub fn on_message(&self, name: &str, payload: &str) {
        match name {
            message::SELECTION => {
                if payload.is_empty() {
                    return;
                }
                let (host, target) = {
                    let s = self.0.borrow();
                    (s.host.clone(), s.options.selection_clipboard)
                };
                host.copy_selection(payload, target);
            }
            message::SCROLL => self.on_native_scroll(payload),
            message::HINTS => self.on_hints_posted(payload),
            message::EDITOR_SYNC => {
                if payload.is_empty() {
                    return;
                }
                self.on_editor_sync(payload);
            }
            _ => {}
        }
    }

    /// Handle a native-scroll ping (`"<percent> <scrollY>"`, see
    /// `install_scroll_notify`) with no JS round trip: unlike `refresh_status`,
    /// which re-queries the whole viewport (multiple querySelectors,
    /// getBoundingClientRects, getComputedStyle — forced layout) for every single
    /// percent step, the payload already has everything needed to paint the
    /// statusbar. A malformed payload (there should never be one) is ignored
    /// silently rather than risking a stale statusbar over a parse panic.
    fn on_native_scroll(&self, payload: &str) {
        let Some((percent, y)) = payload.split_once(' ') else {
            return;
        };
        let (Ok(percent), Ok(y)) = (percent.parse::<u32>(), y.parse::<f64>()) else {
            return;
        };
        let mut s = self.0.borrow_mut();
        s.last_scroll = y;
        // Same fields `refresh_status` re-fits/paints, just built synchronously
        // from the session instead of round-tripping into the page for them.
        s.chrome.refit_trail();
        let pending = s.matcher.pending_indicator();
        let zoom = zoom_indicator(s.zoom, s.text_zoom);
        s.chrome.set_status_right(percent, &pending, &zoom);
    }

    /// Read the file, render it, and load the HTML. When `preserve_scroll`, capture
    /// the current scroll offset first and open the re-rendered document there —
    /// so a live reload of the file being read stays put from its first frame
    /// instead of flashing back to the top.
    fn render_and_load(&self, preserve_scroll: bool) {
        if preserve_scroll {
            let this = self.clone();
            let view = self.0.borrow().view.clone();
            view.scroll_position(move |y| {
                this.0.borrow_mut().pending_position = InitialPosition::Offset(y);
                this.do_render_and_load();
            });
        } else {
            self.do_render_and_load();
        }
    }

    /// Rebuild the vault index on a worker thread and swap it in when it lands
    /// (DESIGN D11).
    ///
    /// The walk is the one piece of per-load work whose cost is set by the *tree*
    /// rather than by the document, so it is the one piece that must not run on the
    /// main loop: a vault behind a slow mount, or a `.git/`-rooted tree, would
    /// otherwise stall every `:open` for as long as the filesystem took to answer.
    ///
    /// Landing is deliberately quiet. The index almost always comes back identical
    /// to the one already in hand — nothing was created or renamed in the second
    /// since the last scan — and an identical index cannot change a single
    /// resolution, so the common case re-renders nothing at all.
    fn rescan_vault(&self) {
        {
            let mut s = self.0.borrow_mut();
            if s.vault_scanning {
                return;
            }
            s.vault_scanning = true;
        }
        let (root, host) = {
            let s = self.0.borrow();
            (s.vault_root.clone(), s.host.clone())
        };
        // Weak: a scan outliving its window must not keep the session alive.
        let weak = Rc::downgrade(&self.0);
        host.spawn_blocking(
            move || VaultIndex::build(root.clone(), vault::scan(&root)),
            move |scanned| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let this = Self(inner);
                let mut s = this.0.borrow_mut();
                s.vault_scanning = false;
                // A deferred initial render (see `initial_render_deferred`) is
                // waiting on exactly this landing — nothing has been rendered at
                // all yet, so it must go out even when the scan found nothing new
                // (an empty vault scans identical to the empty index the session
                // started with) or the walk itself panicked. Consumed here, before
                // either early return below, so neither can skip it.
                let deferred = std::mem::take(&mut s.initial_render_deferred);
                // `None` is a panic in the walk. Keeping the previous index costs
                // some links their targets; taking down the reader would cost the
                // document.
                let Some(index) = scanned else {
                    if deferred {
                        drop(s);
                        this.do_render_and_load();
                    }
                    return;
                };
                if *s.vault.index() == index {
                    if deferred {
                        drop(s);
                        this.do_render_and_load();
                    }
                    return;
                }
                s.vault.set_index(index);
                if deferred {
                    drop(s);
                    this.do_render_and_load();
                    return;
                }
                if !s.doc_uses_vault {
                    return;
                }
                // A position already armed (a history offset, a jumplist hop, a
                // link fragment) is the place the reader asked for, and the load
                // that would land it has not finished yet. Re-reading the live
                // scroll here would capture the pre-restore 0.0 and overwrite it;
                // keep the armed one.
                let preserve_scroll = matches!(s.pending_position, InitialPosition::Top);
                drop(s);
                this.render_and_load(preserve_scroll);
            },
        );
    }

    fn do_render_and_load(&self) {
        let mut s = self.0.borrow_mut();
        // Every render cancels a pending deferred initial render, whatever
        // triggered it: a user's `r`/`:open`/link follow inside the deferral
        // window renders the document just as well as the scan landing would
        // have, and a flag left standing would make the scan's landing (or the
        // failsafe) fire a redundant render over the document being read.
        s.initial_render_deferred = false;
        // User CSS themes are reloaded on every render so edits hot-swap in.
        s.render_opts.extra_css = load_themes(&s.config_dir);
        let path = s.file.clone();
        // Content comes from the stdin buffer for a stream, else from the file. A
        // chunk boundary may split a multibyte char; `from_utf8_lossy` renders it as
        // a replacement char that self-corrects on the next chunk.
        let md = match &s.stdin_buffer {
            Some(buf) => Ok(String::from_utf8_lossy(&buf.lock().unwrap()).into_owned()),
            None => std::fs::read_to_string(&path)
                .map_err(|err| format!("cannot read {}: {err}", path.display())),
        };
        match md {
            Ok(md) => {
                s.doc_uses_vault = vault::may_reference_vault(&md);
                let doc = pipeline::render(&md, &s.render_opts, &s.vault);
                s.toc = doc.toc.clone();
                s.section = 0;
                // Re-assert the recolor on the outgoing document, and hand the same
                // state to `load_document` so it pre-applies the `dark` class on
                // `<html>` and the new document paints dark from its first frame.
                let dark = s.dark;
                s.view.set_dark(dark);
                // Text zoom rides into the HTML the same way, so the first frames
                // are already at the right size; `None` keeps the stylesheet's base.
                let font_size_px = (s.text_zoom != 1.0).then(|| s.font_base_px * s.text_zoom);
                let at = s.pending_position.clone();
                s.view.load_document(&doc, &path, &at, dark, font_size_px);
            }
            Err(msg) => {
                // No load will finish, so nothing would ever consume the armed
                // position — disarm it rather than let it leak into a later load of
                // some other document.
                s.pending_position = InitialPosition::Top;
                s.chrome.set_message(&msg);
            }
        }
    }

    /// On load completion, settle the reading position, re-assert recolor, and
    /// mark the reader driveable.
    ///
    /// No longer where the position is *established* — that rides into the load
    /// itself ([`Page::load_document`]), because anything issued here reaches a
    /// document the engine has already parsed, laid out and composited, and needs
    /// a further IPC hop to take effect. This is only the late-layout correction,
    /// and the ordering below matters: the settle goes out **first**, ahead of the
    /// recolor eval, rather than queueing behind other work on the same channel.
    pub fn on_load_finished(&self) {
        {
            let mut s = self.0.borrow_mut();
            // The final authority on where the document sits: subresources are
            // in by now, so an offset that clamped against a shorter,
            // still-loading document lands properly here. Idempotent, and a
            // no-op for a document that opened at the top.
            s.view.settle_initial_position();
            // Idempotent, and it covers the one case the pre-applied `dark`
            // class cannot: a recolor toggled *while* this load was in flight,
            // which the HTML was built too early to know about. It also
            // re-asserts the native background colour.
            let dark = s.dark;
            s.view.set_dark(dark);
            // Text zoom needs nothing here — `load_document` writes the
            // `--font-size` into the HTML, so re-applying it would only reflow
            // the settled page again. Geometric zoom needs nothing either: the
            // native zoom level is a view property that survives a document
            // reload (verified by the live-reload e2e).
            s.pending_position = InitialPosition::Top;
            s.loaded = true;
        }
        self.refresh_status();
    }

    /// One key press, already reduced to the core [`KeyPress`] abstraction.
    /// `None` is a press with no textual meaning at all — a bare modifier, an
    /// arrow or function key — which can match no binding but is still swallowed
    /// by the link-hint overlay, exactly like every other key while it is up.
    ///
    /// Four steps, in this order: the hint overlay takes everything; `Esc` is
    /// the universal abort; the input bar types (with `Tab` completing a `:`
    /// command line); everything else goes through the matcher.
    pub fn on_key(&self, key: Option<KeyPress>) -> KeyOutcome {
        // 1) Link-hint interaction intercepts every key, matcher-free.
        let in_hint = matches!(self.0.borrow().input, Input::Hint { .. });
        if in_hint {
            match key {
                Some(kp) if kp.key == Key::Escape => self.cancel_hints(),
                Some(kp) => self.on_hint_key(kp),
                None => {}
            }
            return KeyOutcome::Consumed;
        }

        // 2) Universal abort (zathura: Esc always returns to normal).
        if key.is_some_and(|kp| kp.key == Key::Escape) {
            self.execute(Action::Abort, 1);
            return KeyOutcome::Consumed;
        }

        // 3) While the input bar is open, let the entry type; Tab completes a
        //    `:` command line, and any edit invalidates a pending cycle.
        let input_visible = self.0.borrow().chrome.prompt().is_some();
        if input_visible {
            if key.is_some_and(|kp| kp.key == Key::Tab) {
                let is_cmd = self.0.borrow().chrome.prompt() == Some(Prompt::Command);
                if is_cmd {
                    self.do_completion();
                }
                return KeyOutcome::Consumed;
            }
            self.0.borrow_mut().completion = None;
            return KeyOutcome::PassThrough;
        }

        // 4) Normal / TOC dispatch through the matcher.
        let Some(kp) = key else {
            return KeyOutcome::PassThrough;
        };
        let result = {
            let s = &mut *self.0.borrow_mut();
            s.matcher.feed(kp, &s.keymap)
        };
        let outcome = match result {
            MatchResult::Matched { action, count } => {
                self.run_action(action, count.unwrap_or(1));
                KeyOutcome::Consumed
            }
            MatchResult::Pending => KeyOutcome::Consumed,
            MatchResult::NoMatch => KeyOutcome::PassThrough,
        };
        self.refresh_status();
        outcome
    }

    /// One Ctrl+wheel tick (`text` ⇒ Ctrl+Shift, the text-zoom axis). Negative
    /// `dy` is a scroll up, which zooms in.
    ///
    /// Text zoom applies immediately and top-anchored; geometric zoom is
    /// coalesced and cursor-anchored, because each step is a full reflow and a
    /// physical burst should become one apply rather than one per tick.
    pub fn on_wheel_zoom(&self, dy: f64, text: bool) {
        if text {
            let action = if dy < 0.0 {
                Action::TextZoomIn
            } else {
                Action::TextZoomOut
            };
            self.execute(action, 1);
        } else {
            self.accumulate_wheel_zoom(dy);
        }
    }

    /// Accumulate one Ctrl+wheel tick. Leading-edge coalescing: the first tick of a
    /// burst applies right away and opens a trailing window; subsequent ticks in
    /// that window only accumulate, and the timer flushes the remainder when it
    /// fires. No tick is ever lost — every tick adds a step, and `flush_wheel_zoom`
    /// drains all accumulated steps.
    fn accumulate_wheel_zoom(&self, dy: f64) {
        let leading = {
            let mut s = self.0.borrow_mut();
            s.pending_zoom_steps += if dy < 0.0 { 1 } else { -1 };
            if s.zoom_flush_scheduled {
                false // a window is already open; the timer will flush this tick.
            } else {
                s.zoom_flush_scheduled = true;
                true
            }
        };
        if leading {
            // Apply the first tick immediately, then open the trailing window.
            self.flush_wheel_zoom();
            let host = self.0.borrow().host.clone();
            let this = self.clone();
            host.defer(WHEEL_ZOOM_COALESCE, move || {
                this.0.borrow_mut().zoom_flush_scheduled = false;
                this.flush_wheel_zoom();
            });
        }
    }

    /// Apply all accumulated Ctrl+wheel ticks as one cursor-anchored zoom change.
    /// A no-op when nothing is pending (the trailing flush after an empty window).
    fn flush_wheel_zoom(&self) {
        let applied = {
            let mut s = self.0.borrow_mut();
            let steps = std::mem::take(&mut s.pending_zoom_steps);
            if steps == 0 {
                None
            } else {
                let level = (s.zoom + s.zoom_step * steps as f64).max(0.2);
                // Capture the anchor from the *current* (pre-change) zoom: the page
                // is still laid out at `s.zoom`, and `cursor_anchor` divides by that
                // to convert to CSS px. Must run before `s.zoom` is updated.
                let anchor = s.cursor_anchor();
                s.zoom = level;
                s.view.zoom_to(level, anchor);
                Some(())
            }
        };
        if applied.is_some() {
            self.refresh_status();
        }
    }

    /// The pointer moved to (`x`, `y`) — **viewport** logical px, so the shell
    /// translates out of its window coordinates first. Remembered so a Ctrl+wheel
    /// zoom can anchor at the cursor.
    pub fn on_pointer_moved(&self, x: f64, y: f64) {
        self.0.borrow_mut().pointer = (x, y);
    }

    /// `Enter` in the input bar: run a search or a `:` command depending on the
    /// active prompt kind.
    pub fn on_input_submitted(&self) {
        let prompt = self.0.borrow().chrome.prompt();
        let query = self.0.borrow().chrome.input_query();
        match prompt {
            Some(Prompt::Search) => {
                {
                    let s = self.0.borrow();
                    s.chrome.close_input();
                    s.view.focus();
                }
                self.0.borrow_mut().completion = None;
                if query.is_empty() {
                    self.0.borrow().view.find_clear();
                    self.refresh_status();
                } else {
                    // A search is a jump: record the pre-search position first.
                    self.jump_to(move |s| s.view.find(&query));
                }
            }
            Some(Prompt::Command) => {
                {
                    let s = self.0.borrow();
                    s.chrome.close_input();
                    s.view.focus();
                }
                self.0.borrow_mut().completion = None;
                self.run_command(&query);
                self.refresh_status();
            }
            None => {}
        }
    }

    /// The window is closing: flush per-file window-state to `history.toml`
    /// synchronously, so `q` reliably persists position even though scroll
    /// queries are async.
    pub fn on_close(&self) {
        let mut s = self.0.borrow_mut();
        // A stdin stream has no file identity to persist window-state against
        // (zathura does not remember stdin documents either), so skip history.
        if !s.is_stdin() {
            s.record_current_state();
            if let Some(dir) = s.data_dir.clone() {
                let _ = write_history(&dir, &s.history);
            }
        }
    }

    fn start_watch(&self) {
        let path = self.0.borrow().file.clone();
        self.restart_watch(&path);
    }

    /// Start streaming from standard input: install the reader (its buffer becomes
    /// the render source) and re-render on each debounced batch, preserving the
    /// reading position exactly like live reload. EOF just stops the updates.
    fn start_stdin(&self) {
        let this = self.clone();
        let host = self.0.borrow().host.clone();
        let reader = StdinReader::start(&host, move || this.render_and_load(true));
        let mut s = self.0.borrow_mut();
        s.stdin_buffer = Some(reader.buffer());
        s._stdin = Some(reader);
    }

    /// (Re)point the document watcher at `path`, replacing any existing one.
    fn restart_watch(&self, path: &Path) {
        let this = self.clone();
        let host = self.0.borrow().host.clone();
        let watch = Watch::start(&host, path, move |event| match event {
            FileEvent::Changed => this.render_and_load(true),
            FileEvent::Removed => {
                this.0
                    .borrow()
                    .chrome
                    .set_message("file removed — showing last render");
            }
        });
        let mut s = self.0.borrow_mut();
        match watch {
            Ok(w) => s._watch = Some(w),
            Err(err) => s
                .chrome
                .set_message(&format!("live reload disabled: {err}")),
        }
    }

    /// Watch `<config>/jumanji/themes` (if it exists) so user-CSS edits hot-swap.
    fn start_theme_watch(&self) {
        let Some(dir) = self
            .0
            .borrow()
            .config_dir
            .as_ref()
            .map(|c| c.join("jumanji").join("themes"))
        else {
            return;
        };
        if !dir.exists() {
            return; // No themes dir yet: empty, no error, no watcher.
        }
        let this = self.clone();
        let host = self.0.borrow().host.clone();
        if let Ok(w) = Watch::start_dir(&host, &dir, move |_| this.render_and_load(true)) {
            self.0.borrow_mut()._theme_watch = Some(w);
        }
    }

    /// The reader state as the compact JSON object the automation surface
    /// reports, delivered to `callback` once the viewport snapshot lands.
    pub fn state(&self, callback: impl FnOnce(String) + 'static) {
        #[rustfmt::skip]
        let (view, file, trail, dark, zoom, text_zoom, section, toc_len, loaded, mode,
             vault_files) = {
            let s = self.0.borrow();
            // Report `stdin` for a stream, not its CWD sentinel path: it is
            // honest, and it keeps the D-Bus forward-search (which matches on
            // this field, DESIGN D7) from ever treating a stream as a file.
            let file = if s.is_stdin() {
                "stdin".to_string()
            } else {
                s.file.to_string_lossy().into_owned()
            };
            (
                s.view.clone(),
                file,
                s.trail_string(),
                s.dark,
                s.zoom,
                s.text_zoom,
                s.section,
                s.toc.len(),
                s.loaded,
                s.mode_str().to_string(),
                s.vault.index().file_count(),
            )
        };
        view.scroll_state(move |vs| {
            callback(state_json(
                &file,
                &trail,
                &vs,
                dark,
                zoom,
                text_zoom,
                section,
                toc_len,
                loaded,
                &mode,
                vault_files,
            ));
        });
    }

    /// Run the action named by `action` (the automation spelling, see
    /// [`config::parse_action`]) `count` times; `Err` for an unknown name.
    pub fn execute_str(&self, action: &str, count: u32) -> Result<(), String> {
        let parsed = config::parse_action(action)?;
        self.execute(parsed, count.max(1));
        Ok(())
    }

    /// Forward editor sync (DESIGN D7): scroll to the element nearest at-or-before
    /// source `line`, recording the departure position on the jumplist first (like
    /// every other jump). A no-op until the document has loaded.
    pub fn goto_source_line(&self, line: u32) {
        if !self.0.borrow().loaded {
            return;
        }
        self.jump_to(move |s| s.view.goto_source_line(line));
    }

    /// Reverse editor sync (DESIGN D7): a Ctrl+click posted `line` (as a string) for
    /// the clicked element. Substitute it and the current file into `editor-command`
    /// and spawn the editor detached — never blocking the UI. Any failure (bad line,
    /// no program, spawn error) is a statusbar notice, never a crash.
    fn on_editor_sync(&self, line: &str) {
        // A stdin stream has no file to point an editor at, so `%f` is meaningless.
        if self.0.borrow().is_stdin() {
            self.0
                .borrow()
                .chrome
                .set_message("editor sync unavailable for a stdin document (no file)");
            return;
        }
        let Ok(line) = line.trim().parse::<u32>() else {
            return;
        };
        let (command, file, chrome, host) = {
            let s = self.0.borrow();
            (
                s.editor_command.clone(),
                s.file.clone(),
                s.chrome.clone(),
                s.host.clone(),
            )
        };
        // Substitute `%l`/`%f`, then expand a leading `$VAR` per token (so the
        // default `$EDITOR` resolves from the environment at spawn time).
        let argv: Vec<String> = command
            .to_argv(line, &file)
            .into_iter()
            .map(|tok| expand_env_token(&tok))
            .collect();

        match argv.split_first() {
            Some((program, _)) if !program.is_empty() => match host.spawn_detached(&argv) {
                Ok(()) => chrome.set_message(&format!("editor: {program} at line {line}")),
                Err(e) => chrome.set_message(&format!("editor-command failed: {e}")),
            },
            _ => {
                chrome.set_message("editor-command has no program (set $EDITOR or editor-command)")
            }
        }
    }

    /// Execute one [`Action`], `count` times where meaningful, and repaint the
    /// status line — the whole of what a key press or an automation call does.
    pub fn execute(&self, action: Action, count: u32) {
        self.run_action(action, count);
        self.refresh_status();
    }

    /// Execute one [`Action`], `count` times where meaningful. The status
    /// refresh is the caller's ([`Controller::execute`] pairs the two); the key
    /// dispatcher refreshes once for the whole press, matched or not.
    fn run_action(&self, action: Action, count: u32) {
        let count_i = count.max(1) as i64;
        let mut s = self.0.borrow_mut();
        match action {
            Action::Scroll(dir) => {
                let step = s.scroll_step * count_i;
                let (dx, dy) = match dir {
                    Direction::Down => (0, step),
                    Direction::Up => (0, -step),
                    Direction::Right => (step, 0),
                    Direction::Left => (-step, 0),
                };
                s.view.scroll_by(dx, dy);
            }
            Action::HalfPage(dir) => {
                let down = matches!(dir, Direction::Down);
                s.view.scroll_half_page(down, count);
            }
            // gg / G / <N>G are real jumps → record the departure position first.
            Action::GotoTop => {
                drop(s);
                self.jump_to(|s| {
                    s.section = 0;
                    s.view.scroll_to_top();
                });
            }
            Action::GotoBottom => {
                drop(s);
                self.jump_to(|s| {
                    s.section = s.toc.len().saturating_sub(1);
                    s.view.scroll_to_bottom();
                });
            }
            Action::GotoSection(n) => {
                drop(s);
                self.jump_to(move |s| {
                    if !s.toc.is_empty() {
                        let idx = ((n as usize).saturating_sub(1)).min(s.toc.len() - 1);
                        s.section = idx;
                        let anchor = s.toc[idx].anchor.clone();
                        s.view.scroll_to_anchor(&anchor);
                    }
                });
            }
            // Section next/prev are *not* jumps (zathura parity).
            Action::SectionNext => {
                if !s.toc.is_empty() {
                    s.section = (s.section + 1).min(s.toc.len() - 1);
                    let anchor = s.toc[s.section].anchor.clone();
                    s.view.scroll_to_anchor(&anchor);
                }
            }
            Action::SectionPrevious => {
                if !s.toc.is_empty() {
                    s.section = s.section.saturating_sub(1);
                    let anchor = s.toc[s.section].anchor.clone();
                    s.view.scroll_to_anchor(&anchor);
                }
            }
            // Keyboard / automation zoom is immediate (counts already batch) and
            // top-anchored — the reflow keeps the top of the viewport fixed.
            Action::ZoomIn => {
                s.zoom = (s.zoom + s.zoom_step * count as f64).max(0.2);
                let level = s.zoom;
                s.view.zoom_to(level, ZoomAnchor::Top);
            }
            Action::ZoomOut => {
                s.zoom = (s.zoom - s.zoom_step * count as f64).max(0.2);
                let level = s.zoom;
                s.view.zoom_to(level, ZoomAnchor::Top);
            }
            Action::TextZoomIn => {
                s.text_zoom = clamp_text_zoom(
                    s.text_zoom + s.text_zoom_step * count as f64,
                    s.font_base_px,
                );
                s.view.set_text_zoom_px(s.font_base_px * s.text_zoom);
            }
            Action::TextZoomOut => {
                s.text_zoom = clamp_text_zoom(
                    s.text_zoom - s.text_zoom_step * count as f64,
                    s.font_base_px,
                );
                s.view.set_text_zoom_px(s.font_base_px * s.text_zoom);
            }
            Action::ZoomReset => {
                s.zoom = 1.0;
                s.text_zoom = 1.0;
                let base = s.font_base_px;
                // Reset both axes under a single top anchor (avoids two anchors
                // fighting over the combined reflow).
                s.view.reset_zoom(base);
            }
            Action::SearchStart => s.chrome.open_input(Prompt::Search),
            Action::SearchNext => s.view.find_next(),
            Action::SearchPrevious => s.view.find_previous(),
            Action::Recolor => {
                s.dark = !s.dark;
                let dark = s.dark;
                s.view.set_dark(dark);
                s.chrome.set_dark(dark);
            }
            Action::Reload => {
                drop(s);
                // An explicit reload is exactly when the index may have changed
                // underfoot (a note created or renamed since the last load). The
                // render below does not wait for it: if the rescan does turn up
                // something new, landing it re-renders again.
                self.rescan_vault();
                self.render_and_load(true);
            }
            Action::ToggleFrontmatter => {
                let show = !s.options.show_frontmatter;
                s.options.show_frontmatter = show;
                s.render_opts.show_frontmatter = show;
                drop(s);
                self.render_and_load(true);
            }
            Action::ToggleToc => {
                drop(s);
                self.toggle_toc();
            }
            Action::CommandLine => s.chrome.open_input(Prompt::Command),
            Action::FollowLink => {
                drop(s);
                self.start_hints(HintKind::Follow);
            }
            Action::ShowLinkTarget => {
                drop(s);
                self.start_hints(HintKind::Show);
            }
            Action::QuickmarkSet(c) => {
                let view = s.view.clone();
                let zoom = s.zoom;
                drop(s);
                let this = self.clone();
                view.scroll_position(move |y| {
                    let mut s = this.0.borrow_mut();
                    s.marks.set(c, Position { scroll_y: y, zoom });
                    s.chrome.set_message(&format!("mark {c} set"));
                });
            }
            Action::QuickmarkJump(c) => {
                let pos = s.marks.get(c);
                drop(s);
                match pos {
                    Some(p) => {
                        let this = self.clone();
                        let view = self.0.borrow().view.clone();
                        view.scroll_position(move |cur| {
                            {
                                let mut s = this.0.borrow_mut();
                                let loc = s.current_location(cur);
                                s.jumplist.push(loc);
                                // `set_zoom` is a native call that reflows the page
                                // at once, while the scroll is an async eval — so
                                // when the two differ a frame can land at the new
                                // zoom and the old offset. Both are already issued
                                // in one turn from this eval's completion callback
                                // (the sequencing `Page::zoom_to` uses), and the
                                // remaining gap is closed for the common case by
                                // simply not touching zoom when the mark was set at
                                // the level already in effect.
                                if (p.zoom - s.zoom).abs() > f64::EPSILON {
                                    s.zoom = p.zoom;
                                    s.view.set_zoom(p.zoom);
                                }
                                s.view.restore_scroll(p.scroll_y);
                            }
                            this.refresh_status();
                        });
                    }
                    None => self.0.borrow().chrome.set_message(&format!("no mark {c}")),
                }
            }
            Action::JumpBackward => {
                drop(s);
                let this = self.clone();
                let view = self.0.borrow().view.clone();
                view.scroll_position(move |cur| {
                    let current = this.0.borrow().current_location(cur);
                    let target = this.0.borrow_mut().jumplist.back(current);
                    // Stepping back shortens the breadcrumb even when the hop stays
                    // inside the current document (no reload to repaint it).
                    if target.is_some_and(|loc| this.navigate_to_location(loc)) {
                        this.0.borrow().show_trail();
                    }
                    this.refresh_status();
                });
            }
            Action::JumpForward => {
                let target = s.jumplist.forward();
                drop(s);
                if target.is_some_and(|loc| self.navigate_to_location(loc)) {
                    self.0.borrow().show_trail();
                }
            }
            Action::TocNext => s.chrome.toc_move(count_i as i32),
            Action::TocPrevious => s.chrome.toc_move(-(count_i as i32)),
            Action::TocExpand => s.chrome.toc_expand(),
            Action::TocCollapse => s.chrome.toc_collapse(),
            Action::TocSelect => {
                drop(s);
                self.toc_select();
            }
            Action::Abort => {
                // Read the mode *before* resetting it: leaving the TOC page is
                // decided by the mode the abort interrupted, not the one it is
                // about to install.
                let in_toc = s.mode == Mode::Toc;
                s.matcher.set_mode(Mode::Normal);
                s.mode = Mode::Normal;
                if in_toc {
                    s.leave_toc();
                }
                if matches!(s.input, Input::Hint { .. }) {
                    s.view.clear_hints();
                    s.input = Input::None;
                }
                if s.chrome.prompt().is_some() {
                    s.chrome.close_input();
                    s.view.focus();
                }
                // Zathura's universal abort: Esc also drops any active search
                // (highlights + `n`/`N` state) and clears any transient statusbar
                // notice, returning the chrome to its resting state.
                s.view.find_clear();
                s.show_trail();
                s.completion = None;
            }
            Action::Quit => {
                // Quitting synchronously runs the shell's close path, whose handler
                // re-borrows the session to flush history — so release our borrow
                // first.
                let host = s.host.clone();
                drop(s);
                host.quit();
            }
        }
    }

    /// Refresh the right-hand status (scroll %, pending key echo, zoom) and cache
    /// the live scroll offset for the synchronous close-time history flush.
    fn refresh_status(&self) {
        let this = self.clone();
        let (view, chrome, pending, zoom) = {
            let s = self.0.borrow();
            // The bar may have been resized since the breadcrumb was last laid out;
            // re-fitting here is idempotent and costs a string compare.
            s.chrome.refit_trail();
            (
                s.view.clone(),
                s.chrome.clone(),
                s.matcher.pending_indicator(),
                zoom_indicator(s.zoom, s.text_zoom),
            )
        };
        view.scroll_state(move |vs| {
            this.0.borrow_mut().last_scroll = vs.scroll_y;
            chrome.set_status_right(vs.scroll_percent, &pending, &zoom);
        });
    }

    // -----------------------------------------------------------------------
    // TOC mode
    // -----------------------------------------------------------------------

    /// The chrome reported an activated TOC row (a double-click, or `Enter` while
    /// the list has focus): the same jump path the `TocSelect` action takes.
    pub fn on_toc_activated(&self) {
        self.toc_select();
    }

    /// Toggle between the content page and the TOC page (zathura `Tab`).
    fn toggle_toc(&self) {
        {
            let mut s = self.0.borrow_mut();
            if s.mode == Mode::Toc {
                s.leave_toc();
            } else {
                if s.toc.is_empty() {
                    s.chrome.set_message("no headings");
                    return;
                }
                let (toc, section, dark) = (s.toc.clone(), s.section, s.dark);
                s.mode = Mode::Toc;
                s.matcher.set_mode(Mode::Toc);
                s.chrome.show_toc(&toc, section, dark);
                s.chrome.set_message("Index");
            }
        }
        self.refresh_status();
    }

    /// Jump to the selected TOC entry's anchor and return to Normal mode (records a
    /// jumplist entry, zathura index-select behaviour).
    fn toc_select(&self) {
        let target = {
            let mut s = self.0.borrow_mut();
            match s.chrome.toc_selected() {
                Some(sel) => {
                    s.leave_toc();
                    Some(sel)
                }
                None => None,
            }
        };
        if let Some((anchor, heading_index)) = target {
            self.jump_to(move |s| {
                s.section = heading_index;
                s.view.scroll_to_anchor(&anchor);
            });
        }
    }

    // -----------------------------------------------------------------------
    // Link hints (`f` / `F`)
    // -----------------------------------------------------------------------

    /// Enter the link-hint interaction: draw the overlay and start collecting keys.
    fn start_hints(&self, kind: HintKind) {
        let mut s = self.0.borrow_mut();
        if s.mode != Mode::Normal {
            return;
        }
        s.input = Input::Hint {
            kind,
            typed: String::new(),
            links: Vec::new(),
        };
        s.chrome.set_message(hint_prompt(kind));
        s.view.request_hints();
    }

    /// The overlay JS posted its label→href list (tab-separated, one per line).
    fn on_hints_posted(&self, msg: &str) {
        let links = parse_hints(msg);
        {
            let mut s = self.0.borrow_mut();
            match &mut s.input {
                Input::Hint { links: l, .. } => *l = links,
                _ => return,
            }
        }
        if self.0.borrow().input_links_empty() {
            // Nothing to hint at: report and drop back to normal.
            {
                let s = self.0.borrow();
                s.view.clear_hints();
                s.chrome.set_message("no links in view");
            }
            self.0.borrow_mut().input = Input::None;
            return;
        }
        match self.hint_resolve() {
            Some(action) => self.hint_act(action),
            None => self.update_hint_status(),
        }
    }

    /// Handle one keypress while the hint overlay is active.
    fn on_hint_key(&self, kp: KeyPress) {
        match kp.key {
            Key::Backspace => {
                {
                    let mut s = self.0.borrow_mut();
                    if let Input::Hint { typed, .. } = &mut s.input {
                        typed.pop();
                    }
                }
                // Re-filter; a shorter prefix never triggers an exact match.
                let _ = self.hint_resolve();
                self.update_hint_status();
            }
            Key::Char(c) => {
                let accepted = {
                    let mut s = self.0.borrow_mut();
                    match &mut s.input {
                        Input::Hint { typed, links, .. } => {
                            if links.is_empty() {
                                // Links not posted yet: buffer optimistically.
                                typed.push(c);
                                true
                            } else {
                                let tentative = format!("{typed}{c}");
                                if links.iter().any(|l| l.label.starts_with(&tentative)) {
                                    *typed = tentative;
                                    true
                                } else {
                                    false // dead end: ignore the keystroke.
                                }
                            }
                        }
                        _ => false,
                    }
                };
                if accepted {
                    match self.hint_resolve() {
                        Some(action) => self.hint_act(action),
                        None => self.update_hint_status(),
                    }
                }
            }
            _ => {}
        }
    }

    /// If the typed prefix exactly matches a label, consume the overlay and return
    /// the resolved action; otherwise narrow the visible hints and return `None`.
    fn hint_resolve(&self) -> Option<HintAction> {
        let mut s = self.0.borrow_mut();
        let Input::Hint { kind, typed, links } = &s.input else {
            return None;
        };
        let kind = *kind;
        let typed = typed.clone();
        if let Some(link) = links.iter().find(|l| l.label == typed) {
            let href = link.href.clone();
            s.input = Input::None;
            s.view.clear_hints();
            s.show_trail();
            return Some(match kind {
                HintKind::Follow => HintAction::Follow(href),
                HintKind::Show => HintAction::Show(href),
            });
        }
        s.view.filter_hints(&typed);
        None
    }

    fn hint_act(&self, action: HintAction) {
        match action {
            HintAction::Follow(href) => self.on_navigate(&href),
            HintAction::Show(href) => self.0.borrow().chrome.set_message(&format!("→ {href}")),
        }
    }

    fn update_hint_status(&self) {
        let s = self.0.borrow();
        if let Input::Hint { kind, typed, .. } = &s.input {
            let prompt = hint_prompt(*kind);
            s.chrome.set_message(&format!("{prompt} {typed}"));
        }
    }

    fn cancel_hints(&self) {
        let mut s = self.0.borrow_mut();
        s.view.clear_hints();
        s.input = Input::None;
        s.show_trail();
    }

    // -----------------------------------------------------------------------
    // Link routing and `:open`
    // -----------------------------------------------------------------------

    /// Route a resolved target URI — a link click the engine was told to ignore,
    /// or a followed hint: same-document fragment → scroll; local
    /// `.md`/`.markdown` → open in-window; anything else → hand to the system
    /// (the reader itself never navigates).
    pub fn on_navigate(&self, uri: &str) {
        let current = self.0.borrow().file.clone();
        // The engine hands the fragment back percent-encoded, so a block id arrives
        // as `%5E37066d` — and `getElementById` needs the literal `^` (DESIGN D11).
        let (base, frag) = match uri.split_once('#') {
            Some((b, f)) => (b.to_string(), Some(obsidian::percent_decode(f))),
            None => (uri.to_string(), None),
        };

        // Same-document fragment: scroll to the anchor (recording a jump). We drive
        // the scroll ourselves rather than letting the engine navigate.
        if let Some(anchor) = frag.clone()
            && same_document(&current, &base)
        {
            self.jump_to(move |s| s.view.scroll_to_anchor(&anchor));
            return;
        }

        // Local markdown file → open it in this window.
        if let Some(path) = file_uri_to_path(&base) {
            let is_md = path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
                .unwrap_or(false);
            if is_md {
                // Cross-document fragments used to be dropped here; `open_file`
                // carries this one over the load instead (D11 — which fixes plain
                // markdown fragment links too, not just wikilinks).
                self.open_file(path, frag);
                return;
            }
        }

        // Everything else (http/https, other local files) → the system default.
        let (host, chrome) = {
            let s = self.0.borrow();
            (s.host.clone(), s.chrome.clone())
        };
        match host.open_external(uri) {
            Ok(()) => chrome.set_message(&format!("opened externally: {uri}")),
            Err(e) => chrome.set_message(&format!("cannot open {uri}: {e}")),
        }
    }

    /// Open `path` in this window in response to a link follow or `:open`. Records
    /// the current position on the jumplist first — so `Ctrl-o` / `Backspace`
    /// returns here — unless we're leaving a non-returnable stdin stream. The new
    /// file resumes at its saved scroll position (or the top, if unseen), or at
    /// `anchor` when the link carried a fragment.
    fn open_file(&self, path: PathBuf, anchor: Option<String>) {
        if !path.exists() {
            self.0
                .borrow()
                .chrome
                .set_message(&format!("no such file: {}", path.display()));
            return;
        }
        // Query the departure offset live rather than reading the cached
        // `last_scroll`, matching what [`Action::JumpBackward`] already does. The
        // cache is only refreshed by `refresh_status`, and the in-page scroll
        // listener pings it only when the *rounded percent* changes — so a small
        // wheel scroll followed straight away by a link click would otherwise record
        // a stale departure and land `Ctrl-o` a little off.
        let this = self.clone();
        let view = self.0.borrow().view.clone();
        view.scroll_position(move |cur| {
            {
                let mut s = this.0.borrow_mut();
                // Record the departure so the jumplist can return here. A stdin
                // stream has no reopenable identity, so leaving one records nothing.
                if !s.is_stdin() {
                    let loc = s.current_location(cur);
                    s.jumplist.push(loc);
                }
            }
            // Precedence, resolved here rather than left to two fields racing at
            // load-finished time: a link that named a fragment named a place in the
            // target document, and that beats wherever the reader last left it.
            let at = match anchor {
                Some(anchor) => InitialPosition::Anchor(anchor),
                None => InitialPosition::Offset(
                    this.0
                        .borrow()
                        .history
                        .get(&path)
                        .map(|st| st.scroll_y)
                        .unwrap_or(0.0),
                ),
            };
            this.load_document(path, at);
        });
    }

    /// Load `path` into this window, opening it at `at`: persist the *outgoing*
    /// file's position, reset per-document state, re-point the watcher, and render.
    /// The jumplist is deliberately **not** reset — it spans documents, so `Ctrl-o`
    /// can walk back into the previous file. Callers own all jumplist bookkeeping.
    ///
    /// Zoom is deliberately **not** touched either: it is a live session setting
    /// that carries across navigation (D5a), so following a link out of a document
    /// you are reading at 130% keeps you at 130% even when the target has never been
    /// opened. The per-file zoom in `history` is the *default on open* and is read
    /// only at the window's cold start, where there is no session zoom to inherit.
    fn load_document(&self, path: PathBuf, at: InitialPosition) {
        {
            let mut s = self.0.borrow_mut();
            // Opening a file ends any stdin stream and starts a normal file document
            // (with live reload, history, editor sync). Persist the *previous* file's
            // position first, but not a stream's (it has no history identity).
            if s.is_stdin() {
                s.stdin_buffer = None;
                s._stdin = None;
            } else {
                s.record_current_state();
            }
            if s.mode == Mode::Toc {
                s.leave_toc();
            }
            s.file = path.clone();
            // The root is pinned, so a document switch changes only which note the
            // index is being consulted *from* — the index itself carries over and
            // the render below resolves immediately. A rescan is kicked off after
            // this borrow (the new document may have arrived alongside notes the
            // current index never saw); watch-driven live reload deliberately does
            // not rescan — editing a note cannot rename another one (D11).
            s.vault.rebind(&path);
            // The breadcrumb ends at the new document; the callers' jumplist
            // bookkeeping (below) has already recorded how we got here.
            s.show_trail();
            // Per-document navigation state resets on a document switch; the
            // jumplist persists (it spans documents — see the fn doc).
            s.marks = Marks::new();
            s.section = 0;
            s.loaded = false;
            // Zoom carries over untouched (see the fn doc). Geometric zoom needs no
            // call at all — the native zoom level is a view property that survives a
            // document load — and `s.text_zoom` is left as-is so
            // `do_render_and_load` below inlines the session's font size into the
            // HTML and the new document's first frame is already at the right size
            // (D12). The caller decides the opening position.
            s.pending_position = at;
        }
        self.restart_watch(&path);
        self.rescan_vault();
        self.do_render_and_load();
        self.refresh_status();
    }

    // -----------------------------------------------------------------------
    // `:` command line
    // -----------------------------------------------------------------------

    fn run_command(&self, query: &str) {
        match command::parse(query) {
            Ok(Command::Open(raw)) => {
                let current = self.0.borrow().file.clone();
                let resolved = resolve_open_path(&current, &raw);
                self.open_file(resolved, None);
            }
            Ok(Command::Set(key, value)) => self.apply_set(&key, &value),
            Ok(Command::Exec(action)) => self.run_action(action, 1),
            Ok(Command::Quit) => {
                // Release the borrow before quitting (the handler re-borrows).
                let host = self.0.borrow().host.clone();
                host.quit();
            }
            Err(e) => self.0.borrow().chrome.set_message(&e),
        }
    }

    /// Apply a `:set key value` and honour the resulting [`SetEffect`].
    fn apply_set(&self, key: &str, value: &str) {
        let effect = self.0.borrow_mut().options.set(key, value);
        match effect {
            Err(e) => self.0.borrow().chrome.set_message(&e),
            Ok(SetEffect::Rerender) => {
                {
                    let mut s = self.0.borrow_mut();
                    let o = s.options.clone();
                    s.render_opts.page_width_px = o.page_width_px;
                    s.render_opts.show_frontmatter = o.show_frontmatter;
                    s.render_opts.font_body = o.font_body.clone();
                    s.render_opts.font_mono = o.font_mono.clone();
                    s.render_opts.font_size_px = o.font_size_px;
                    s.font_base_px = o.font_size_px as f64;
                }
                self.render_and_load(true);
            }
            Ok(SetEffect::Recolor) => {
                let mut s = self.0.borrow_mut();
                s.dark = s.options.default_recolor;
                let dark = s.dark;
                s.view.set_dark(dark);
                s.chrome.set_dark(dark);
            }
            Ok(SetEffect::None) => {
                let mut s = self.0.borrow_mut();
                s.scroll_step = s.options.scroll_step_px as i64;
                s.zoom_step = s.options.zoom_step;
                s.text_zoom_step = s.options.text_zoom_step;
            }
        }
    }

    /// Tab-complete the `:` command line, cycling on repeated presses.
    fn do_completion(&self) {
        let mut s = self.0.borrow_mut();
        if s.chrome.prompt() != Some(Prompt::Command) {
            return;
        }
        let current = s.chrome.input_query();
        // Taken before the completion borrow below: the echo is laid out to fit the
        // bar, so it pages through *all* candidates instead of listing a prefix.
        let cols = s.chrome.status_columns();

        // Cycle when the shown text is the current candidate and there are more.
        if let Some(comp) = s.completion.as_mut() {
            let showing_current = comp
                .candidates
                .get(comp.index)
                .map(|c| c == &current)
                .unwrap_or(false);
            if showing_current && comp.candidates.len() > 1 {
                comp.index = (comp.index + 1) % comp.candidates.len();
                let next = comp.candidates[comp.index].clone();
                let line = command::completion_line(&comp.candidates, comp.index, cols);
                s.chrome.set_input_query(&next);
                s.chrome.set_message(&line);
                return;
            }
        }

        // Fresh completion from the current input.
        let file = s.file.clone();
        let candidates = compute_completion(&file, &current);
        if candidates.is_empty() {
            s.completion = None;
            return;
        }
        let first = candidates[0].clone();
        let line = command::completion_line(&candidates, 0, cols);
        s.chrome.set_input_query(&first);
        s.chrome.set_message(&line);
        s.completion = Some(Completion {
            candidates,
            index: 0,
        });
    }

    // -----------------------------------------------------------------------
    // Jumplist
    // -----------------------------------------------------------------------

    /// Restore a jumplist [`Location`]: scroll in place when it names the current
    /// document, otherwise open its file at the recorded offset. A `None` document
    /// is the stdin stream we've since replaced — unreturnable.
    ///
    /// Returns whether we actually landed there; a refused hop leaves a statusbar
    /// notice the caller must not paint over.
    fn navigate_to_location(&self, loc: Location) -> bool {
        let same_doc = {
            let s = self.0.borrow();
            match (&loc.doc, s.is_stdin()) {
                (Some(p), false) => *p == s.file,
                (None, true) => true,
                _ => false,
            }
        };
        if same_doc {
            self.0.borrow().view.restore_scroll(loc.scroll_y);
            return true;
        }
        match loc.doc {
            Some(path) if path.exists() => {
                self.load_document(path, InitialPosition::Offset(loc.scroll_y));
                true
            }
            Some(path) => {
                self.0
                    .borrow()
                    .chrome
                    .set_message(&format!("no such file: {}", path.display()));
                false
            }
            None => {
                self.0
                    .borrow()
                    .chrome
                    .set_message("cannot return to piped input");
                false
            }
        }
    }

    /// Record the current (async-queried) scroll position on the jumplist, then run
    /// `after` — the actual jump — in the query's callback.
    fn jump_to(&self, after: impl FnOnce(&mut Session<T>) + 'static) {
        let this = self.clone();
        let view = self.0.borrow().view.clone();
        view.scroll_position(move |cur| {
            {
                let mut s = this.0.borrow_mut();
                let loc = s.current_location(cur);
                s.jumplist.push(loc);
                after(&mut s);
            }
            this.refresh_status();
        });
    }
}

/// What a completed hint resolves to.
enum HintAction {
    Follow(String),
    Show(String),
}

impl<T: Toolkit> Session<T> {
    fn input_links_empty(&self) -> bool {
        match &self.input {
            Input::Hint { links, .. } => links.is_empty(),
            _ => true,
        }
    }

    /// The reported mode: `hint` (overlay active) > `command`/`search` (input bar) >
    /// the keymap mode (`toc`/`normal`).
    fn mode_str(&self) -> &'static str {
        if matches!(self.input, Input::Hint { .. }) {
            return "hint";
        }
        match self.chrome.prompt() {
            Some(Prompt::Command) => return "command",
            Some(Prompt::Search) => return "search",
            None => {}
        }
        match self.mode {
            Mode::Toc => "toc",
            Mode::Normal => "normal",
        }
    }

    /// Return to the content page and Normal mode.
    fn leave_toc(&mut self) {
        self.mode = Mode::Normal;
        self.matcher.set_mode(Mode::Normal);
        self.chrome.hide_toc();
        self.show_trail();
        self.view.focus();
    }

    /// Repaint the statusbar's left field with the jumplist breadcrumb: the route
    /// to the current document (`index.md > topic.md > note.md`). Called after a
    /// document switch, a jumplist hop, and whenever a transient message clears.
    fn show_trail(&self) {
        self.chrome.set_trail(self.trail_segments());
    }

    /// The breadcrumb segments: the jumplist's route to the live document, as
    /// display names, oldest first.
    fn trail_segments(&self) -> Vec<String> {
        let current = (!self.is_stdin()).then_some(self.file.as_path());
        self.jumplist
            .trail(current)
            .into_iter()
            .map(doc_label)
            .collect()
    }

    /// The untruncated breadcrumb, for the state snapshot: what the statusbar
    /// shows when the window is wide enough for all of it.
    fn trail_string(&self) -> String {
        self.trail_segments().join(" > ")
    }

    /// The current reading position as a jumplist [`Location`]: the live document
    /// (a file, or `None` for the stdin stream) at scroll offset `scroll_y`.
    fn current_location(&self, scroll_y: f64) -> Location {
        Location {
            doc: if self.is_stdin() {
                None
            } else {
                Some(self.file.clone())
            },
            scroll_y,
        }
    }

    /// Fold the current position into the in-memory history (not yet written).
    fn record_current_state(&mut self) {
        let state = FileState {
            scroll_y: self.last_scroll,
            zoom: self.zoom,
            text_zoom: self.text_zoom,
        };
        let file = self.file.clone();
        self.history.record(&file, state);
    }

    /// The current cursor as a [`ZoomAnchor`] in the viewport's CSS-px
    /// coordinates. [`pointer`](Self::pointer) is already in viewport logical px,
    /// so this only divides by the zoom level: the CSS viewport is `deviceWidth /
    /// (scale × zoom)` and a logical px is `deviceWidth / scale`, so the display
    /// scale factor cancels and only the zoom divisor remains. **Must be evaluated
    /// at the zoom level the page is currently laid out at** (the pre-change
    /// zoom): the divisor is that level, so calling it after updating
    /// [`zoom`](Self::zoom) would convert with the wrong scale and misplace the
    /// anchor — the error grows with distance from the origin (the
    /// cursor-near-bottom bug).
    fn cursor_anchor(&self) -> ZoomAnchor {
        let (x, y) = self.pointer;
        let zoom = self.zoom.max(0.2);
        ZoomAnchor::Point {
            x: x / zoom,
            y: y / zoom,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn hint_prompt(kind: HintKind) -> &'static str {
    match kind {
        HintKind::Follow => "follow link:",
        HintKind::Show => "show target:",
    }
}

/// Parse the overlay's `label\thref` lines into typed [`HintLink`]s.
fn parse_hints(msg: &str) -> Vec<HintLink> {
    msg.lines()
        .filter_map(|line| {
            line.split_once('\t').map(|(label, href)| HintLink {
                label: label.to_string(),
                href: href.to_string(),
            })
        })
        .collect()
}

/// Convert a `file://` URI to a filesystem path; `None` for other schemes.
///
/// Decoded here rather than through a toolkit URI type (gio's `File::for_uri`,
/// `NSURL`) because this layer has no toolkit: a `file://` URI is a percent-
/// encoded path and nothing else, and [`obsidian::percent_decode`] is the same
/// decoder the link fragments already go through.
fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file://host/p` carries an authority before the path; `file:///p` an
    // empty one. Drop it either way, as gio's `File::for_uri(..).path()` did —
    // a file URI names a local path, and the host (normally `localhost` or
    // nothing) adds no information the reader could act on.
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(slash) => &rest[slash..],
        None => return None,
    };
    Some(PathBuf::from(obsidian::percent_decode(path)))
}

/// Whether `base` (a fragment-less URI) names the document already open.
///
/// Compared as **paths**, never as URI strings: an engine and
/// `core::obsidian::percent_encode` disagree on which characters to escape
/// (`'`, `(`, `)`, `+` among them), so a byte comparison misses a same-document
/// `[[#Heading]]` in any file whose path contains one — reloading the document
/// and pushing a spurious jumplist entry instead of just scrolling.
fn same_document(current: &Path, base: &str) -> bool {
    if base.is_empty() {
        return true;
    }
    match file_uri_to_path(base) {
        Some(path) => canonical(&path) == canonical(current),
        None => false,
    }
}

/// `path` canonicalized, or unchanged when it cannot be (it may not exist).
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Full completion candidate lines (without the leading `:`).
fn compute_completion(current_file: &Path, input: &str) -> Vec<String> {
    match command::complete(input) {
        Completions::Candidates(v) => v,
        Completions::Path { prefix } => {
            let dir = current_file.parent().unwrap_or(Path::new("."));
            complete_path(dir, &prefix)
                .into_iter()
                .map(|p| format!("open {p}"))
                .collect()
        }
    }
}

/// Filesystem completion for a `:open` path prefix, relative to `current_dir`.
/// Candidates keep the typed directory prefix; directories get a trailing `/`.
fn complete_path(current_dir: &Path, prefix: &str) -> Vec<String> {
    let (typed_dir, partial) = match prefix.rfind('/') {
        Some(i) => (&prefix[..=i], &prefix[i + 1..]),
        None => ("", prefix),
    };
    let expanded = expand_tilde(typed_dir);
    let listing_dir = if expanded.as_os_str().is_empty() {
        current_dir.to_path_buf()
    } else if expanded.is_absolute() {
        expanded
    } else {
        current_dir.join(expanded)
    };

    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&listing_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(partial) {
                continue;
            }
            // Skip dotfiles unless the user has started typing one.
            if name.starts_with('.') && !partial.starts_with('.') {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let mut cand = format!("{typed_dir}{name}");
            if is_dir {
                cand.push('/');
            }
            out.push(cand);
        }
    }
    out.sort();
    out
}

/// Resolve a `:open` argument: expand `~`, and take relatives against the
/// current file's directory.
fn resolve_open_path(current_file: &Path, raw: &str) -> PathBuf {
    let p = expand_tilde(raw.trim());
    if p.is_absolute() {
        p
    } else {
        current_file.parent().map(|d| d.join(&p)).unwrap_or(p)
    }
}

/// Expand a leading `~` / `~/` against `$HOME`.
fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    if s == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    PathBuf::from(s)
}

/// A document's statusbar name: its basename, or `stdin` for the stream.
fn doc_label(doc: Option<&Path>) -> String {
    match doc {
        Some(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.to_string_lossy().into_owned()),
        None => "stdin".to_string(),
    }
}

/// `$XDG_DATA_HOME/jumanji` (or `$HOME/.local/share/jumanji`).
fn xdg_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir).join("jumanji"));
    }
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("jumanji")
    })
}

fn load_history(dir: &Option<PathBuf>) -> History {
    let Some(dir) = dir else {
        return History::default();
    };
    match std::fs::read_to_string(dir.join("history.toml")) {
        Ok(text) => History::load(&text),
        Err(_) => History::default(),
    }
}

fn write_history(dir: &Path, history: &History) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("history.toml"), history.to_toml())
}

/// Load `<config>/jumanji/themes/*.css`, sorted by filename. Missing dir → empty.
fn load_themes(config_dir: &Option<PathBuf>) -> Vec<String> {
    let Some(cd) = config_dir else {
        return Vec::new();
    };
    let dir = cd.join("jumanji").join("themes");
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .map(|e| e.eq_ignore_ascii_case("css"))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    files.sort();
    files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

/// Expand a whole-token environment reference (`$EDITOR` → its value); any other
/// token is returned unchanged. An unset variable yields an empty string, which
/// the caller treats as "no program".
fn expand_env_token(token: &str) -> String {
    match token.strip_prefix('$') {
        Some(name) => std::env::var(name).unwrap_or_default(),
        None => token.to_string(),
    }
}

/// Serialize the reader state as the compact JSON object [`Controller::state`]
/// returns. The viewport widths (`viewport_width`, `doc_scroll_width`,
/// `diagram_width`, `math_width`) let e2e tests assert the reflow invariants and
/// that MathML laid out with nonzero geometry; `fn_color` lets e2e assert the
/// dark-mode syntax-highlight scoping fix; `first_frame_scroll_y` and
/// `reveal_scroll_y` let e2e assert the no-flash property (the *first* painted
/// frame's offset and the offset the body was unhidden at, not just the final
/// one); the rest are unchanged.
#[allow(clippy::too_many_arguments)]
fn state_json(
    file: &str,
    trail: &str,
    vs: &ViewportState,
    dark: bool,
    zoom: f64,
    text_zoom: f64,
    section: usize,
    toc_len: usize,
    loaded: bool,
    mode: &str,
    vault_files: usize,
) -> String {
    format!(
        "{{\"file\":{file},\"trail\":{trail},\
         \"scroll_y\":{scroll_y},\"scroll_percent\":{scroll_percent},\
         \"content_width\":{content_width},\"viewport_width\":{viewport_width},\
         \"doc_scroll_width\":{doc_scroll_width},\"diagram_width\":{diagram_width},\
         \"math_width\":{math_width},\"msup_shift_ratio\":{msup_shift_ratio},\
         \"fence_width\":{fence_width},\"frontmatter_width\":{frontmatter_width},\
         \"first_frame_scroll_y\":{first_frame_scroll_y},\
         \"reveal_scroll_y\":{reveal_scroll_y},\
         \"reveal_failsafe\":{reveal_failsafe},\"restoring\":{restoring},\
         \"fn_color\":{fn_color},\
         \"dark\":{dark},\"zoom\":{zoom},\"text_zoom\":{text_zoom},\"mode\":{mode},\
         \"section\":{section},\"toc_len\":{toc_len},\"loaded\":{loaded},\
         \"vault_files\":{vault_files}}}",
        file = json_string(file),
        trail = json_string(trail),
        scroll_y = vs.scroll_y,
        scroll_percent = vs.scroll_percent,
        content_width = vs.content_width,
        viewport_width = vs.viewport_width,
        doc_scroll_width = vs.doc_scroll_width,
        diagram_width = vs.diagram_width,
        math_width = vs.math_width,
        msup_shift_ratio = vs.msup_shift_ratio,
        fence_width = vs.fence_width,
        frontmatter_width = vs.frontmatter_width,
        first_frame_scroll_y = vs.first_frame_scroll_y,
        reveal_scroll_y = vs.reveal_scroll_y,
        reveal_failsafe = vs.revealed_by_failsafe,
        restoring = vs.restoring,
        fn_color = json_string(&vs.fn_color),
        mode = json_string(mode),
    )
}

/// Encode `s` as a JSON string literal (double-quoted, minimally escaped).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Clamp the text-zoom factor to a sane range: no smaller than 8 px, no larger
/// than 3× the base font size.
fn clamp_text_zoom(factor: f64, base_px: f64) -> f64 {
    let min = if base_px > 0.0 { 8.0 / base_px } else { 0.5 };
    factor.clamp(min, 3.0)
}

/// The right-hand zoom indicator: `{geometric}%/{text}%T` when either axis
/// differs from 100%, empty when both are exactly 100%.
fn zoom_indicator(geometric: f64, text: f64) -> String {
    let g = (geometric * 100.0).round() as i64;
    let t = (text * 100.0).round() as i64;
    if g == 100 && t == 100 {
        String::new()
    } else {
        format!("{g}%/{t}%T")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_decode_to_paths() {
        assert_eq!(
            file_uri_to_path("file:///home/u/notes/a.md"),
            Some(PathBuf::from("/home/u/notes/a.md"))
        );
        // Percent-encoding is undone: a space, and the `^` a block id carries.
        assert_eq!(
            file_uri_to_path("file:///home/u/my%20notes/b%5Ec.md"),
            Some(PathBuf::from("/home/u/my notes/b^c.md"))
        );
        // A stray `%` that starts no valid escape is left alone. (gio, which
        // this replaced, rejected the whole URI instead and such a link fell
        // through to the system handler; opening it in-window is the better
        // reading of a link to a file that really is called `100%.md`.)
        assert_eq!(
            file_uri_to_path("file:///tmp/100%.md"),
            Some(PathBuf::from("/tmp/100%.md"))
        );
    }

    #[test]
    fn a_file_uri_authority_is_dropped() {
        // Both spellings name the same local file, as they did under gio.
        assert_eq!(
            file_uri_to_path("file://localhost/tmp/a.md"),
            Some(PathBuf::from("/tmp/a.md"))
        );
        assert_eq!(
            file_uri_to_path("file://otherhost/tmp/a.md"),
            Some(PathBuf::from("/tmp/a.md"))
        );
        // An authority with no path names nothing.
        assert_eq!(file_uri_to_path("file://localhost"), None);
    }

    #[test]
    fn non_file_uris_have_no_path() {
        assert_eq!(file_uri_to_path("https://example.com/a.md"), None);
        assert_eq!(file_uri_to_path("mailto:a@b.c"), None);
        assert_eq!(file_uri_to_path("/home/u/a.md"), None);
    }

    #[test]
    fn zoom_indicator_is_empty_at_one_hundred_percent() {
        assert_eq!(zoom_indicator(1.0, 1.0), "");
        assert_eq!(zoom_indicator(1.3, 1.0), "130%/100%T");
    }
}
