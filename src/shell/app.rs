//! The GTK4 application: window, capture-phase key dispatch, and the mapping
//! from [`Action`]s to `webkit6` calls. As thin as the design allows — the
//! keymap, config, command parsing, jumplist, marks, and history all live in
//! `core`; this layer is the imperative glue.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk::gdk::{Key as GdkKey, ModifierType};
use gtk::gio;
use gtk::glib;
use gtk::glib::variant::ToVariant;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, EventControllerKey, EventControllerMotion,
    EventControllerScroll, EventControllerScrollFlags, Orientation, PropagationPhase, Stack,
};
use webkit6::LoadEvent;
use webkit6::prelude::*;

use crate::core::command::{self, Command, Completions};
use crate::core::config::{self, Config, Options, SetEffect};
use crate::core::editor::EditorCommand;
use crate::core::history::{FileState, History};
use crate::core::jumplist::Jumplist;
use crate::core::keymap::{Key, KeyPress, Keymap, MatchResult, Matcher};
use crate::core::marks::{Marks, Position};
use crate::core::pipeline::{self, Options as RenderOptions};
use crate::core::source::Source;
use crate::core::{Action, Direction, Heading, Mode};

use super::bar::{Bar, Prompt};
use super::dbus;
use super::stdin::StdinReader;
use super::toc::TocView;
use super::view::{View, ViewportState, ZoomAnchor};
use super::watch::{FileEvent, Watch};

const APP_ID: &str = "org.membranepotential.jumanji";

/// Which link-hint action is pending (mirrors `f` vs `F`).
#[derive(Debug, Clone, Copy)]
enum HintKind {
    /// `f` — follow the chosen link (route it through [`open_uri`]).
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

/// Shell-local interaction state that sits *outside* the keymap modes: the
/// link-hint overlay intercepts keys directly (not via a `Mode`), the way the
/// input bar does. Everything else is `None`.
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

/// Mutable shell state shared across GTK callbacks.
struct Shell {
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
    /// The document's basename (or `stdin`), restored to the statusbar's left
    /// field after a transient message or mode label clears it.
    filename: String,
    /// Live options, mutated by `:set`; the source of truth the derived render
    /// options and step fields are re-synced from.
    options: Options,
    render_opts: RenderOptions,
    /// Reverse editor-sync template (DESIGN D7): spawned on Ctrl+click with
    /// `%l`/`%f` substituted. Config-only (copied from options at construction).
    editor_command: EditorCommand,
    /// A `--forward <line>` requested on the command line, applied once the
    /// initial load finishes (DESIGN D7 forward sync on a fresh launch).
    pending_forward: Option<u32>,
    /// XDG config base (`…/.config`); themes live under `<it>/jumanji/themes`.
    config_dir: Option<PathBuf>,
    /// Data dir (`…/.local/share/jumanji`); holds `history.toml`.
    data_dir: Option<PathBuf>,
    scroll_step: i64,
    /// Geometric zoom step (added to the webkit `zoom_level` per step).
    zoom_step: f64,
    /// Text-zoom step: fraction of the base font size added per step.
    text_zoom_step: f64,
    /// Base body font size in px (text-zoom 100% reference; from config).
    font_base_px: f64,
    /// Current geometric zoom factor (1.0 = 100%). The shell owns the intended
    /// level; the webview is driven to match. Kept in the shell (rather than read
    /// back from `zoom_level()`) because anchored zoom sets the native level in an
    /// async callback, so `zoom_level()` briefly lags the intent.
    zoom: f64,
    /// Current text-zoom factor (1.0 = 100%).
    text_zoom: f64,
    /// Last pointer position in *window* coordinates (from the motion
    /// controller), translated to the webview at wheel-zoom time to anchor at
    /// the cursor.
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
    view: View,
    toc_view: TocView,
    /// Stack holding the content (`"content"`) and TOC (`"toc"`) pages.
    stack: Stack,
    bar: Bar,
    window: ApplicationWindow,
    toc: Vec<Heading>,
    section: usize,
    dark: bool,
    /// Whether the initial [`LoadEvent::Finished`] has fired. Key/D-Bus actions
    /// are no-ops before this; the D-Bus `loaded` flag lets clients (tests,
    /// editor integrations) wait for a driveable window.
    loaded: bool,
    /// Scroll offset to restore once the next load finishes.
    pending_restore: Option<f64>,
    /// Last observed scroll offset, refreshed on every status update. Read
    /// synchronously on window-close to flush history without an async query.
    last_scroll: f64,
    /// Link-hint / other shell-local interaction state.
    input: Input,
    /// Pending `:`-completion cycle, if any.
    completion: Option<Completion>,
    /// Jumplist for `Ctrl-o` / `Ctrl-i` (per document; reset on `:open`).
    jumplist: Jumplist,
    /// Quickmark registers `m<x>` / `'<x>` (per document; reset on `:open`).
    marks: Marks,
    /// Per-file window-state, loaded at startup and flushed on close/switch.
    history: History,
    _watch: Option<Watch>,
    _theme_watch: Option<Watch>,
    /// The stdin reader thread + poll source, for a stdin document. Dropping it
    /// stops the streaming updates.
    _stdin: Option<StdinReader>,
    /// Keeps the per-instance D-Bus name owned for the process lifetime.
    _dbus: Option<gtk::gio::OwnerId>,
}

impl Shell {
    /// Whether this is a stdin (streaming) document rather than a file.
    fn is_stdin(&self) -> bool {
        self.stdin_buffer.is_some()
    }
}

/// Launch the application for `source` with the resolved `config`. `forward` is
/// an optional `--forward <line>` to jump to once the initial load finishes
/// (file sources only; rejected for stdin before we get here).
pub fn run(source: Source, config: Config, forward: Option<u32>) -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();

    let keymap = Rc::new(config.keymap);
    let options = config.options;

    app.connect_activate(move |app| {
        build_ui(
            app,
            source.clone(),
            options.clone(),
            keymap.clone(),
            forward,
        );
    });

    // We parse args ourselves (see `main`); don't let GTK interpret argv.
    app.run_with_args::<&str>(&[])
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

fn build_ui(
    app: &Application,
    source: Source,
    options: Options,
    keymap: Rc<Keymap>,
    forward: Option<u32>,
) {
    let is_stdin = source.is_stdin();
    let file = base_path(&source);
    let view = View::new(options.selection_clipboard);
    let toc_view = TocView::new();
    let bar = Bar::new();

    let stack = Stack::new();
    stack.set_vexpand(true);
    stack.set_hexpand(true);
    stack.add_named(view.widget(), Some("content"));
    stack.add_named(toc_view.widget(), Some("toc"));
    stack.set_visible_child_name("content");

    let layout = GtkBox::new(Orientation::Vertical, 0);
    layout.append(&stack);
    layout.append(bar.widget());

    let window = ApplicationWindow::builder()
        .application(app)
        .title("jumanji")
        .default_width((options.page_width_px + 80).max(640) as i32)
        .default_height(800)
        .child(&layout)
        .build();

    let filename = source.display_name();
    bar.set_filename(&filename);

    let config_dir = config::xdg_config_dir();
    let data_dir = xdg_data_dir();
    let history = load_history(&data_dir);

    let shell = Rc::new(RefCell::new(Shell {
        file: file.clone(),
        stdin_buffer: None,
        filename,
        options: options.clone(),
        render_opts: RenderOptions {
            page_width_px: options.page_width_px,
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
        editor_command: options.editor_command.clone(),
        pending_forward: forward,
        config_dir: config_dir.clone(),
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
        view: view.clone(),
        toc_view,
        stack,
        bar: bar.clone(),
        window: window.clone(),
        toc: Vec::new(),
        section: 0,
        dark: options.default_recolor,
        loaded: false,
        pending_restore: None,
        last_scroll: 0.0,
        input: Input::None,
        completion: None,
        jumplist: Jumplist::new(),
        marks: Marks::new(),
        history,
        _watch: None,
        _theme_watch: None,
        _stdin: None,
        _dbus: None,
    }));

    // Restore any saved window-state for this file so the first painted frame is
    // already at the right place. Scroll is deferred to load-finished. Read the
    // value out before taking the mutable borrow (avoid a reentrant borrow).
    // Skipped for stdin: a stream has no stable identity to key history on.
    let saved = if is_stdin {
        None
    } else {
        shell.borrow().history.get(&file)
    };
    if let Some(st) = saved {
        let mut s = shell.borrow_mut();
        s.text_zoom = st.text_zoom;
        s.zoom = st.zoom;
        s.view.set_zoom(st.zoom);
        s.pending_restore = Some(st.scroll_y);
    }

    install_view_handlers(&shell);
    connect_toc_activate(&shell);
    connect_load_finished(&shell);
    connect_keys(&shell, keymap);
    connect_scroll(&shell);
    connect_motion(&shell);
    connect_input_entry(&shell);
    connect_close(&shell);
    // A stdin document streams from a reader thread; a file document watches the
    // filesystem for live reload. The two are mutually exclusive.
    if is_stdin {
        start_stdin(&shell);
    } else {
        start_watch(&shell);
    }
    start_theme_watch(&shell);
    serve_dbus(&shell);

    window.present();
    view.widget().grab_focus();

    // Initial render + load.
    do_render_and_load(&shell);
}

/// Wire the view's shell-supplied callbacks: link-hint posts and navigation
/// routing (link clicks).
fn install_view_handlers(shell: &Rc<RefCell<Shell>>) {
    let view = shell.borrow().view.clone();
    {
        let shell = shell.clone();
        view.set_hints_handler(move |json| on_hints_posted(&shell, &json));
    }
    {
        let shell = shell.clone();
        view.set_navigate_handler(move |uri| open_uri(&shell, &uri));
    }
    {
        let shell = shell.clone();
        view.set_editor_sync_handler(move |line| on_editor_sync(&shell, &line));
    }
}

/// Read the file, render it, and load the HTML. When `preserve_scroll`, capture
/// the current scroll offset first so it can be restored after the load.
fn render_and_load(shell: &Rc<RefCell<Shell>>, preserve_scroll: bool) {
    if preserve_scroll {
        let shell = shell.clone();
        let view = shell.borrow().view.clone();
        view.scroll_position(move |y| {
            shell.borrow_mut().pending_restore = Some(y);
            do_render_and_load(&shell);
        });
    } else {
        do_render_and_load(shell);
    }
}

fn do_render_and_load(shell: &Rc<RefCell<Shell>>) {
    let mut s = shell.borrow_mut();
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
            let doc = pipeline::render(&md, &s.render_opts);
            s.toc = doc.toc.clone();
            s.section = 0;
            // Record the desired recolor state before loading so `load_document`
            // pre-applies the `dark` class and paints dark from the first frame.
            let dark = s.dark;
            s.view.set_dark(dark);
            s.view.load_document(&doc, &path);
        }
        Err(msg) => {
            s.bar.set_message(&msg);
        }
    }
}

/// On load completion, apply recolor state, restore any pending scroll offset,
/// re-apply both zoom axes, and refresh the percentage indicator.
fn connect_load_finished(shell: &Rc<RefCell<Shell>>) {
    let webview = shell.borrow().view.widget().clone();
    let shell = shell.clone();
    webview.connect_load_changed(move |_, event| {
        if event != LoadEvent::Finished {
            return;
        }
        {
            let s = shell.borrow();
            s.view.set_dark(s.dark);
            // Re-apply text zoom: the inline `--font-size` custom property is
            // lost on reload. Geometric zoom needs no re-apply — the native
            // `zoom_level` is a WebView property that survives a document reload
            // (verified by the live-reload e2e). No anchoring here — the scroll
            // offset is restored explicitly below.
            if s.text_zoom != 1.0 {
                s.view.set_text_zoom_px(s.font_base_px * s.text_zoom);
            }
            if let Some(y) = s.pending_restore {
                s.view.restore_scroll(y);
            }
        }
        let forward = {
            let mut s = shell.borrow_mut();
            s.pending_restore = None;
            s.loaded = true;
            // A `--forward <line>` overrides the restored scroll: the editor
            // explicitly pointed the reader at this line, so jump there once.
            s.pending_forward.take()
        };
        if let Some(line) = forward {
            goto_source_line(&shell, line);
        }
        refresh_status(&shell);
    });
}

fn connect_keys(shell: &Rc<RefCell<Shell>>, keymap: Rc<Keymap>) {
    let controller = EventControllerKey::new();
    controller.set_propagation_phase(PropagationPhase::Capture);
    let window = shell.borrow().window.clone();

    let shell = shell.clone();
    controller.connect_key_pressed(move |_, keyval, _keycode, mods| {
        // 1) Link-hint interaction intercepts every key, matcher-free.
        let in_hint = matches!(shell.borrow().input, Input::Hint { .. });
        if in_hint {
            if keyval == GdkKey::Escape {
                cancel_hints(&shell);
            } else if let Some(kp) = to_keypress(keyval, mods) {
                on_hint_key(&shell, kp);
            }
            return glib::Propagation::Stop;
        }

        // 2) Universal abort (zathura: Esc always returns to normal).
        if keyval == GdkKey::Escape {
            execute(&shell, Action::Abort, 1);
            refresh_status(&shell);
            return glib::Propagation::Stop;
        }

        // 3) While the input bar is open, let the entry type; Tab completes a
        //    `:` command line, and any edit invalidates a pending cycle.
        let input_visible = shell.borrow().bar.is_input_visible();
        if input_visible {
            if keyval == GdkKey::Tab || keyval == GdkKey::ISO_Left_Tab {
                let is_cmd = shell.borrow().bar.prompt() == Some(Prompt::Command);
                if is_cmd {
                    do_completion(&shell);
                }
                return glib::Propagation::Stop;
            }
            shell.borrow_mut().completion = None;
            return glib::Propagation::Proceed;
        }

        // 4) Normal / TOC dispatch through the matcher.
        let Some(kp) = to_keypress(keyval, mods) else {
            return glib::Propagation::Proceed;
        };
        let result = shell.borrow_mut().matcher.feed(kp, &keymap);
        let propagation = match result {
            MatchResult::Matched { action, count } => {
                execute(&shell, action, count.unwrap_or(1));
                glib::Propagation::Stop
            }
            MatchResult::Pending => glib::Propagation::Stop,
            MatchResult::NoMatch => glib::Propagation::Proceed,
        };
        refresh_status(&shell);
        propagation
    });

    window.add_controller(controller);
}

/// Bind Ctrl+wheel → geometric zoom and Ctrl+Shift+wheel → text zoom on the
/// window (capture phase, so we intercept before WebKit's own scroll handling).
fn connect_scroll(shell: &Rc<RefCell<Shell>>) {
    let controller = EventControllerScroll::new(EventControllerScrollFlags::BOTH_AXES);
    controller.set_propagation_phase(PropagationPhase::Capture);
    let window = shell.borrow().window.clone();

    let shell = shell.clone();
    controller.connect_scroll(move |ctrl, _dx, dy| {
        let mods = ctrl.current_event_state();
        if !mods.contains(ModifierType::CONTROL_MASK) || dy == 0.0 {
            return glib::Propagation::Proceed;
        }
        if mods.contains(ModifierType::SHIFT_MASK) {
            // Ctrl+Shift+wheel → text zoom, immediate (top-anchored).
            let action = if dy < 0.0 {
                Action::TextZoomIn
            } else {
                Action::TextZoomOut
            };
            execute(&shell, action, 1);
            refresh_status(&shell);
        } else {
            // Ctrl+wheel → geometric zoom, coalesced + cursor-anchored: each
            // step is now a full reflow, so a burst is batched into one apply.
            accumulate_wheel_zoom(&shell, dy);
        }
        glib::Propagation::Stop
    });

    window.add_controller(controller);
}

/// Track the pointer (window coordinates) so Ctrl+wheel can anchor at the
/// cursor. Capture phase on the toplevel, mirroring the key/scroll controllers —
/// a controller on the WebView itself never sees these events (DESIGN.md D5a).
fn connect_motion(shell: &Rc<RefCell<Shell>>) {
    let controller = EventControllerMotion::new();
    controller.set_propagation_phase(PropagationPhase::Capture);
    let window = shell.borrow().window.clone();
    let sh = shell.clone();
    controller.connect_motion(move |_, x, y| {
        sh.borrow_mut().pointer = (x, y);
    });
    window.add_controller(controller);
}

/// Trailing-window for Ctrl+wheel zoom coalescing. The first tick of a burst
/// applies immediately (leading edge, so a single tick feels instant); ticks
/// arriving within this window after it are batched into one further anchored
/// reflow. Long enough that a physical burst collapses, short enough to feel
/// immediate.
const WHEEL_ZOOM_COALESCE: std::time::Duration = std::time::Duration::from_millis(40);

/// Accumulate one Ctrl+wheel tick. Leading-edge coalescing: the first tick of a
/// burst applies right away and opens a trailing window; subsequent ticks in
/// that window only accumulate, and the timer flushes the remainder when it
/// fires. No tick is ever lost — every tick adds a step, and `flush_wheel_zoom`
/// drains all accumulated steps.
fn accumulate_wheel_zoom(shell: &Rc<RefCell<Shell>>, dy: f64) {
    let leading = {
        let mut s = shell.borrow_mut();
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
        flush_wheel_zoom(shell);
        let sh = shell.clone();
        glib::timeout_add_local_once(WHEEL_ZOOM_COALESCE, move || {
            sh.borrow_mut().zoom_flush_scheduled = false;
            flush_wheel_zoom(&sh);
        });
    }
}

/// Apply all accumulated Ctrl+wheel ticks as one cursor-anchored zoom change.
/// A no-op when nothing is pending (the trailing flush after an empty window).
fn flush_wheel_zoom(shell: &Rc<RefCell<Shell>>) {
    let applied = {
        let mut s = shell.borrow_mut();
        let steps = std::mem::take(&mut s.pending_zoom_steps);
        if steps == 0 {
            None
        } else {
            let level = (s.zoom + s.zoom_step * steps as f64).max(0.2);
            // Capture the anchor from the *current* (pre-change) zoom: the page
            // is still laid out at `s.zoom`, and `cursor_anchor` divides by that
            // to convert to CSS px. Must run before `s.zoom` is updated.
            let anchor = cursor_anchor(&s);
            s.zoom = level;
            s.view.zoom_to(level, anchor);
            Some(())
        }
    };
    if applied.is_some() {
        refresh_status(shell);
    }
}

/// The current cursor as a [`ZoomAnchor`] in the webview's CSS-px coordinates.
/// Translate window→webview (GTK logical px), then divide by the zoom level to
/// get CSS px for `elementFromPoint`: the CSS viewport is `deviceWidth /
/// (scale × zoom)` and GTK logical px is `deviceWidth / scale`, so the display
/// scale factor cancels and only the zoom divisor remains. **Must be evaluated
/// at the zoom level the page is currently laid out at** (the pre-change zoom):
/// the divisor is that level, so calling it after updating `Shell.zoom` would
/// convert with the wrong scale and misplace the anchor — the error grows with
/// distance from the origin (the cursor-near-bottom bug).
fn cursor_anchor(s: &Shell) -> ZoomAnchor {
    let (wx, wy) = s.pointer;
    let webview = s.view.widget();
    let src = gtk::graphene::Point::new(wx as f32, wy as f32);
    let p = s
        .window
        .compute_point(webview, &src)
        .unwrap_or_else(|| gtk::graphene::Point::new(wx as f32, wy as f32));
    let zoom = s.zoom.max(0.2);
    ZoomAnchor::Point {
        x: p.x() as f64 / zoom,
        y: p.y() as f64 / zoom,
    }
}

/// Wire the input bar's `Enter`: run a search or a `:` command depending on the
/// active prompt kind.
fn connect_input_entry(shell: &Rc<RefCell<Shell>>) {
    let entry = shell.borrow().bar.entry().clone();
    let shell = shell.clone();
    entry.connect_activate(move |_| {
        let prompt = shell.borrow().bar.prompt();
        let query = shell.borrow().bar.input_query();
        match prompt {
            Some(Prompt::Search) => {
                {
                    let s = shell.borrow();
                    s.bar.close_input();
                    s.view.widget().grab_focus();
                }
                shell.borrow_mut().completion = None;
                if query.is_empty() {
                    shell.borrow().view.find_clear();
                    refresh_status(&shell);
                } else {
                    // A search is a jump: record the pre-search position first.
                    jump_to(&shell, move |s| s.view.find(&query));
                }
            }
            Some(Prompt::Command) => {
                {
                    let s = shell.borrow();
                    s.bar.close_input();
                    s.view.widget().grab_focus();
                }
                shell.borrow_mut().completion = None;
                run_command(&shell, &query);
                refresh_status(&shell);
            }
            None => {}
        }
    });
}

/// Flush per-file window-state to `history.toml` synchronously on window close,
/// so `q` reliably persists position even though scroll queries are async.
fn connect_close(shell: &Rc<RefCell<Shell>>) {
    let window = shell.borrow().window.clone();
    let shell = shell.clone();
    window.connect_close_request(move |_| {
        let mut s = shell.borrow_mut();
        // A stdin stream has no file identity to persist window-state against
        // (zathura does not remember stdin documents either), so skip history.
        if !s.is_stdin() {
            record_current_state(&mut s);
            if let Some(dir) = s.data_dir.clone() {
                let _ = write_history(&dir, &s.history);
            }
        }
        glib::Propagation::Proceed
    });
}

fn start_watch(shell: &Rc<RefCell<Shell>>) {
    let path = shell.borrow().file.clone();
    restart_watch(shell, &path);
}

/// Start streaming from standard input: install the reader (its buffer becomes
/// the render source) and re-render on each debounced batch, preserving the
/// reading position exactly like live reload. EOF just stops the updates.
fn start_stdin(shell: &Rc<RefCell<Shell>>) {
    let handler_shell = shell.clone();
    let reader = StdinReader::start(move || render_and_load(&handler_shell, true));
    let mut s = shell.borrow_mut();
    s.stdin_buffer = Some(reader.buffer());
    s._stdin = Some(reader);
}

/// (Re)point the document watcher at `path`, replacing any existing one.
fn restart_watch(shell: &Rc<RefCell<Shell>>, path: &Path) {
    let handler_shell = shell.clone();
    let watch = Watch::start(path, move |event| match event {
        FileEvent::Changed => render_and_load(&handler_shell, true),
        FileEvent::Removed => {
            handler_shell
                .borrow()
                .bar
                .set_message("file removed — showing last render");
        }
    });
    let mut s = shell.borrow_mut();
    match watch {
        Ok(w) => s._watch = Some(w),
        Err(err) => s.bar.set_message(&format!("live reload disabled: {err}")),
    }
}

/// Watch `<config>/jumanji/themes` (if it exists) so user-CSS edits hot-swap.
fn start_theme_watch(shell: &Rc<RefCell<Shell>>) {
    let Some(dir) = shell
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
    let handler_shell = shell.clone();
    if let Ok(w) = Watch::start_dir(&dir, move |_| render_and_load(&handler_shell, true)) {
        shell.borrow_mut()._theme_watch = Some(w);
    }
}

/// Register the per-instance D-Bus automation surface (DESIGN.md D7 foundation).
fn serve_dbus(shell: &Rc<RefCell<Shell>>) {
    let get_state = {
        let shell = shell.clone();
        Rc::new(move |invocation: gtk::gio::DBusMethodInvocation| {
            let (view, file, dark, zoom, text_zoom, section, toc_len, loaded, mode) = {
                let s = shell.borrow();
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
                    s.dark,
                    s.zoom,
                    s.text_zoom,
                    s.section,
                    s.toc.len(),
                    s.loaded,
                    mode_str(&s).to_string(),
                )
            };
            view.scroll_state(move |vs| {
                let json = state_json(
                    &file, &vs, dark, zoom, text_zoom, section, toc_len, loaded, &mode,
                );
                invocation.return_value(Some(&(json,).to_variant()));
            });
        }) as dbus::GetState
    };

    let execute_action = {
        let shell = shell.clone();
        Rc::new(move |action: &str, count: u32| -> Result<(), String> {
            let parsed = config::parse_action(action)?;
            execute(&shell, parsed, count.max(1));
            refresh_status(&shell);
            Ok(())
        }) as dbus::ExecuteAction
    };

    let goto_line = {
        let shell = shell.clone();
        Rc::new(move |line: u32| goto_source_line(&shell, line)) as dbus::GotoLine
    };

    let owner = dbus::serve(dbus::Automation {
        get_state,
        execute_action,
        goto_line,
    });
    shell.borrow_mut()._dbus = owner;
}

/// Forward editor sync (DESIGN D7): scroll to the element nearest at-or-before
/// source `line`, recording the departure position on the jumplist first (like
/// every other jump). A no-op until the document has loaded.
fn goto_source_line(shell: &Rc<RefCell<Shell>>, line: u32) {
    if !shell.borrow().loaded {
        return;
    }
    jump_to(shell, move |s| s.view.goto_source_line(line));
}

/// Reverse editor sync (DESIGN D7): a Ctrl+click posted `line` (as a string) for
/// the clicked element. Substitute it and the current file into `editor-command`
/// and spawn the editor detached — never blocking the UI. Any failure (bad line,
/// no program, spawn error) is a statusbar notice, never a crash.
fn on_editor_sync(shell: &Rc<RefCell<Shell>>, line: &str) {
    // A stdin stream has no file to point an editor at, so `%f` is meaningless.
    if shell.borrow().is_stdin() {
        shell
            .borrow()
            .bar
            .set_message("editor sync unavailable for a stdin document (no file)");
        return;
    }
    let Ok(line) = line.trim().parse::<u32>() else {
        return;
    };
    let (command, file, bar) = {
        let s = shell.borrow();
        (s.editor_command.clone(), s.file.clone(), s.bar.clone())
    };
    // Substitute `%l`/`%f`, then expand a leading `$VAR` per token (so the
    // default `$EDITOR` resolves from the environment at spawn time).
    let argv: Vec<String> = command
        .to_argv(line, &file)
        .into_iter()
        .map(|tok| expand_env_token(&tok))
        .collect();

    match argv.split_first() {
        Some((program, _)) if !program.is_empty() => {
            let owned: Vec<std::ffi::OsString> =
                argv.iter().map(std::ffi::OsString::from).collect();
            let refs: Vec<&std::ffi::OsStr> = owned.iter().map(AsRef::as_ref).collect();
            // gio::Subprocess reaps the child via the main loop and never blocks
            // us; we drop the handle (fire-and-forget), matching zathura.
            match gio::Subprocess::newv(&refs, gio::SubprocessFlags::NONE) {
                Ok(_) => bar.set_message(&format!("editor: {program} at line {line}")),
                Err(e) => bar.set_message(&format!("editor-command failed: {e}")),
            }
        }
        _ => bar.set_message("editor-command has no program (set $EDITOR or editor-command)"),
    }
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

/// Serialize the reader state as the compact JSON object `GetState` returns.
/// The viewport widths (`viewport_width`, `doc_scroll_width`, `diagram_width`,
/// `math_width`) let e2e tests assert the reflow invariants and that MathML laid
/// out with nonzero geometry; `fn_color` lets e2e assert the dark-mode
/// syntax-highlight scoping fix; the rest are unchanged.
#[allow(clippy::too_many_arguments)]
fn state_json(
    file: &str,
    vs: &ViewportState,
    dark: bool,
    zoom: f64,
    text_zoom: f64,
    section: usize,
    toc_len: usize,
    loaded: bool,
    mode: &str,
) -> String {
    format!(
        "{{\"file\":{file},\"scroll_y\":{scroll_y},\"scroll_percent\":{scroll_percent},\
         \"content_width\":{content_width},\"viewport_width\":{viewport_width},\
         \"doc_scroll_width\":{doc_scroll_width},\"diagram_width\":{diagram_width},\
         \"math_width\":{math_width},\"msup_shift_ratio\":{msup_shift_ratio},\
         \"fence_width\":{fence_width},\"fn_color\":{fn_color},\
         \"dark\":{dark},\"zoom\":{zoom},\"text_zoom\":{text_zoom},\"mode\":{mode},\
         \"section\":{section},\"toc_len\":{toc_len},\"loaded\":{loaded}}}",
        file = json_string(file),
        scroll_y = vs.scroll_y,
        scroll_percent = vs.scroll_percent,
        content_width = vs.content_width,
        viewport_width = vs.viewport_width,
        doc_scroll_width = vs.doc_scroll_width,
        diagram_width = vs.diagram_width,
        math_width = vs.math_width,
        msup_shift_ratio = vs.msup_shift_ratio,
        fence_width = vs.fence_width,
        fn_color = json_string(&vs.fn_color),
        mode = json_string(mode),
    )
}

/// The reported mode: `hint` (overlay active) > `command`/`search` (input bar) >
/// the keymap mode (`toc`/`normal`).
fn mode_str(s: &Shell) -> &'static str {
    if matches!(s.input, Input::Hint { .. }) {
        return "hint";
    }
    match s.bar.prompt() {
        Some(Prompt::Command) => return "command",
        Some(Prompt::Search) => return "search",
        None => {}
    }
    match s.mode {
        Mode::Toc => "toc",
        Mode::Normal => "normal",
    }
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

/// Execute one [`Action`], `count` times where meaningful.
fn execute(shell: &Rc<RefCell<Shell>>, action: Action, count: u32) {
    let count_i = count.max(1) as i64;
    let mut s = shell.borrow_mut();
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
            jump_to(shell, |s| {
                s.section = 0;
                s.view.scroll_to_top();
            });
        }
        Action::GotoBottom => {
            drop(s);
            jump_to(shell, |s| {
                s.section = s.toc.len().saturating_sub(1);
                s.view.scroll_to_bottom();
            });
        }
        Action::GotoSection(n) => {
            drop(s);
            jump_to(shell, move |s| {
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
        // Keyboard / D-Bus zoom is immediate (counts already batch) and
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
        Action::SearchStart => s.bar.open_input(Prompt::Search),
        Action::SearchNext => s.view.find_next(),
        Action::SearchPrevious => s.view.find_previous(),
        Action::Recolor => {
            s.dark = !s.dark;
            let dark = s.dark;
            s.view.set_dark(dark);
            s.toc_view.set_dark(dark);
        }
        Action::Reload => {
            drop(s);
            render_and_load(shell, true);
        }
        Action::ToggleToc => {
            drop(s);
            toggle_toc(shell);
        }
        Action::CommandLine => s.bar.open_input(Prompt::Command),
        Action::FollowLink => {
            drop(s);
            start_hints(shell, HintKind::Follow);
        }
        Action::ShowLinkTarget => {
            drop(s);
            start_hints(shell, HintKind::Show);
        }
        Action::QuickmarkSet(c) => {
            let view = s.view.clone();
            let zoom = s.zoom;
            drop(s);
            let sh = shell.clone();
            view.scroll_position(move |y| {
                let mut s = sh.borrow_mut();
                s.marks.set(c, Position { scroll_y: y, zoom });
                s.bar.set_message(&format!("mark {c} set"));
            });
        }
        Action::QuickmarkJump(c) => {
            let pos = s.marks.get(c);
            drop(s);
            match pos {
                Some(p) => {
                    let sh = shell.clone();
                    let view = shell.borrow().view.clone();
                    view.scroll_position(move |cur| {
                        {
                            let mut s = sh.borrow_mut();
                            s.jumplist.push(cur);
                            s.zoom = p.zoom;
                            s.view.set_zoom(p.zoom);
                            s.view.restore_scroll(p.scroll_y);
                        }
                        refresh_status(&sh);
                    });
                }
                None => shell.borrow().bar.set_message(&format!("no mark {c}")),
            }
        }
        Action::JumpBackward => {
            drop(s);
            let sh = shell.clone();
            let view = shell.borrow().view.clone();
            view.scroll_position(move |cur| {
                let target = sh.borrow_mut().jumplist.back(cur);
                if let Some(y) = target {
                    sh.borrow().view.restore_scroll(y);
                }
                refresh_status(&sh);
            });
        }
        Action::JumpForward => {
            let target = s.jumplist.forward();
            if let Some(y) = target {
                s.view.restore_scroll(y);
            }
        }
        Action::TocNext => s.toc_view.move_selection(count_i as i32),
        Action::TocPrevious => s.toc_view.move_selection(-(count_i as i32)),
        Action::TocExpand => s.toc_view.expand_selected(),
        Action::TocCollapse => s.toc_view.collapse_selected(),
        Action::TocSelect => {
            drop(s);
            toc_select(shell);
        }
        Action::Abort => {
            s.matcher.set_mode(Mode::Normal);
            s.mode = Mode::Normal;
            if s.stack.visible_child_name().as_deref() == Some("toc") {
                leave_toc(&mut s);
            }
            if matches!(s.input, Input::Hint { .. }) {
                s.view.clear_hints();
                s.input = Input::None;
                s.bar.set_filename(&s.filename);
            }
            if s.bar.is_input_visible() {
                s.bar.close_input();
                s.view.find_clear();
                s.view.widget().grab_focus();
            }
            s.completion = None;
        }
        Action::Quit => {
            // `close()` synchronously emits close-request, whose handler
            // re-borrows the shell to flush history — so release our borrow first.
            let window = s.window.clone();
            drop(s);
            window.close();
        }
    }
}

/// Refresh the right-hand status (scroll %, pending key echo, zoom) and cache
/// the live scroll offset for the synchronous close-time history flush.
fn refresh_status(shell: &Rc<RefCell<Shell>>) {
    let sh = shell.clone();
    let (view, bar, pending, zoom) = {
        let s = shell.borrow();
        (
            s.view.clone(),
            s.bar.clone(),
            s.matcher.pending_indicator(),
            zoom_indicator(s.zoom, s.text_zoom),
        )
    };
    view.scroll_state(move |vs| {
        sh.borrow_mut().last_scroll = vs.scroll_y;
        bar.set_status_right(vs.scroll_percent, &pending, &zoom);
    });
}

// ---------------------------------------------------------------------------
// TOC mode
// ---------------------------------------------------------------------------

/// Wire double-click / activate on a TOC row to the same jump path as `Enter`.
fn connect_toc_activate(shell: &Rc<RefCell<Shell>>) {
    let handler_shell = shell.clone();
    shell
        .borrow()
        .toc_view
        .set_activate_handler(move || toc_select(&handler_shell));
}

/// Toggle between the content page and the TOC page (zathura `Tab`).
fn toggle_toc(shell: &Rc<RefCell<Shell>>) {
    {
        let mut s = shell.borrow_mut();
        if s.mode == Mode::Toc {
            leave_toc(&mut s);
        } else {
            if s.toc.is_empty() {
                s.bar.set_message("no headings");
                return;
            }
            let (toc, section, dark) = (s.toc.clone(), s.section, s.dark);
            s.toc_view.rebuild(&toc, section, dark);
            s.mode = Mode::Toc;
            s.matcher.set_mode(Mode::Toc);
            s.stack.set_visible_child_name("toc");
            s.bar.set_message("Index");
        }
    }
    refresh_status(shell);
}

/// Return to the content page and Normal mode.
fn leave_toc(s: &mut Shell) {
    s.mode = Mode::Normal;
    s.matcher.set_mode(Mode::Normal);
    s.stack.set_visible_child_name("content");
    s.bar.set_filename(&s.filename);
    s.view.widget().grab_focus();
}

/// Jump to the selected TOC entry's anchor and return to Normal mode (records a
/// jumplist entry, zathura index-select behaviour).
fn toc_select(shell: &Rc<RefCell<Shell>>) {
    let target = {
        let mut s = shell.borrow_mut();
        match s.toc_view.selected() {
            Some(sel) => {
                leave_toc(&mut s);
                Some(sel)
            }
            None => None,
        }
    };
    if let Some((anchor, heading_index)) = target {
        jump_to(shell, move |s| {
            s.section = heading_index;
            s.view.scroll_to_anchor(&anchor);
        });
    }
}

// ---------------------------------------------------------------------------
// Link hints (`f` / `F`)
// ---------------------------------------------------------------------------

/// Enter the link-hint interaction: draw the overlay and start collecting keys.
fn start_hints(shell: &Rc<RefCell<Shell>>, kind: HintKind) {
    let mut s = shell.borrow_mut();
    if s.mode != Mode::Normal {
        return;
    }
    s.input = Input::Hint {
        kind,
        typed: String::new(),
        links: Vec::new(),
    };
    s.bar.set_message(hint_prompt(kind));
    s.view.request_hints();
}

fn hint_prompt(kind: HintKind) -> &'static str {
    match kind {
        HintKind::Follow => "follow link:",
        HintKind::Show => "show target:",
    }
}

/// The overlay JS posted its label→href list (tab-separated, one per line).
fn on_hints_posted(shell: &Rc<RefCell<Shell>>, msg: &str) {
    let links = parse_hints(msg);
    {
        let mut s = shell.borrow_mut();
        match &mut s.input {
            Input::Hint { links: l, .. } => *l = links,
            _ => return,
        }
    }
    if shell.borrow().input_links_empty() {
        // Nothing to hint at: report and drop back to normal.
        {
            let s = shell.borrow();
            s.view.clear_hints();
            s.bar.set_message("no links in view");
        }
        shell.borrow_mut().input = Input::None;
        return;
    }
    match hint_resolve(shell) {
        Some(action) => hint_act(shell, action),
        None => update_hint_status(shell),
    }
}

/// Handle one keypress while the hint overlay is active.
fn on_hint_key(shell: &Rc<RefCell<Shell>>, kp: KeyPress) {
    match kp.key {
        Key::Backspace => {
            {
                let mut s = shell.borrow_mut();
                if let Input::Hint { typed, .. } = &mut s.input {
                    typed.pop();
                }
            }
            // Re-filter; a shorter prefix never triggers an exact match.
            let _ = hint_resolve(shell);
            update_hint_status(shell);
        }
        Key::Char(c) => {
            let accepted = {
                let mut s = shell.borrow_mut();
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
                match hint_resolve(shell) {
                    Some(action) => hint_act(shell, action),
                    None => update_hint_status(shell),
                }
            }
        }
        _ => {}
    }
}

/// What a completed hint resolves to.
enum HintAction {
    Follow(String),
    Show(String),
}

/// If the typed prefix exactly matches a label, consume the overlay and return
/// the resolved action; otherwise narrow the visible hints and return `None`.
fn hint_resolve(shell: &Rc<RefCell<Shell>>) -> Option<HintAction> {
    let mut s = shell.borrow_mut();
    let Input::Hint { kind, typed, links } = &s.input else {
        return None;
    };
    let kind = *kind;
    let typed = typed.clone();
    if let Some(link) = links.iter().find(|l| l.label == typed) {
        let href = link.href.clone();
        s.input = Input::None;
        s.view.clear_hints();
        s.bar.set_filename(&s.filename);
        return Some(match kind {
            HintKind::Follow => HintAction::Follow(href),
            HintKind::Show => HintAction::Show(href),
        });
    }
    s.view.filter_hints(&typed);
    None
}

fn hint_act(shell: &Rc<RefCell<Shell>>, action: HintAction) {
    match action {
        HintAction::Follow(href) => open_uri(shell, &href),
        HintAction::Show(href) => shell.borrow().bar.set_message(&format!("→ {href}")),
    }
}

fn update_hint_status(shell: &Rc<RefCell<Shell>>) {
    let s = shell.borrow();
    if let Input::Hint { kind, typed, .. } = &s.input {
        let prompt = hint_prompt(*kind);
        s.bar.set_message(&format!("{prompt} {typed}"));
    }
}

fn cancel_hints(shell: &Rc<RefCell<Shell>>) {
    let mut s = shell.borrow_mut();
    s.view.clear_hints();
    s.input = Input::None;
    s.bar.set_filename(&s.filename);
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

impl Shell {
    fn input_links_empty(&self) -> bool {
        match &self.input {
            Input::Hint { links, .. } => links.is_empty(),
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Link routing and `:open`
// ---------------------------------------------------------------------------

/// Route a resolved target URI (from a link click or a hint follow):
/// same-document fragment → scroll; local `.md`/`.markdown` → open in-window;
/// anything else → hand to the system (never navigate the webview).
fn open_uri(shell: &Rc<RefCell<Shell>>, uri: &str) {
    let current = gio::File::for_path(&shell.borrow().file).uri().to_string();
    let (base, frag) = match uri.split_once('#') {
        Some((b, f)) => (b.to_string(), Some(f.to_string())),
        None => (uri.to_string(), None),
    };

    // Same-document fragment: scroll to the anchor (recording a jump). We drive
    // the scroll ourselves rather than letting WebKit navigate.
    if let Some(frag) = frag
        && (base.is_empty() || base == current)
    {
        jump_to(shell, move |s| s.view.scroll_to_anchor(&frag));
        return;
    }

    // Local markdown file → open it in this window.
    if let Some(path) = file_uri_to_path(&base) {
        let is_md = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
            .unwrap_or(false);
        if is_md {
            open_file(shell, path);
            return;
        }
    }

    // Everything else (http/https, other local files) → the system default.
    match gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>) {
        Ok(()) => shell
            .borrow()
            .bar
            .set_message(&format!("opened externally: {uri}")),
        Err(e) => shell
            .borrow()
            .bar
            .set_message(&format!("cannot open {uri}: {e}")),
    }
}

/// Open `path` in this window: persist the current position, reset per-document
/// state, re-point the watcher, restore any saved state, and render.
fn open_file(shell: &Rc<RefCell<Shell>>, path: PathBuf) {
    if !path.exists() {
        shell
            .borrow()
            .bar
            .set_message(&format!("no such file: {}", path.display()));
        return;
    }
    {
        let mut s = shell.borrow_mut();
        // Opening a file ends any stdin stream and starts a normal file document
        // (with live reload, history, editor sync). Persist the *previous* file's
        // position first, but not a stream's (it has no history identity).
        if s.is_stdin() {
            s.stdin_buffer = None;
            s._stdin = None;
        } else {
            record_current_state(&mut s);
        }
        if s.mode == Mode::Toc {
            leave_toc(&mut s);
        }
        s.file = path.clone();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        s.filename = name.clone();
        s.bar.set_filename(&name);
        // Per-document navigation state resets on a document switch.
        s.jumplist = Jumplist::new();
        s.marks = Marks::new();
        s.section = 0;
        s.loaded = false;
        // Restore saved state for the new file, or start fresh.
        match s.history.get(&path) {
            Some(st) => {
                s.text_zoom = st.text_zoom;
                s.zoom = st.zoom;
                s.view.set_zoom(st.zoom);
                s.pending_restore = Some(st.scroll_y);
            }
            None => {
                s.text_zoom = 1.0;
                s.zoom = 1.0;
                s.view.set_zoom(1.0);
                s.pending_restore = Some(0.0);
            }
        }
    }
    restart_watch(shell, &path);
    do_render_and_load(shell);
    refresh_status(shell);
}

/// Convert a `file://` URI to a filesystem path; `None` for other schemes.
fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    if !uri.starts_with("file://") {
        return None;
    }
    gio::File::for_uri(uri).path()
}

// ---------------------------------------------------------------------------
// `:` command line
// ---------------------------------------------------------------------------

fn run_command(shell: &Rc<RefCell<Shell>>, query: &str) {
    match command::parse(query) {
        Ok(Command::Open(raw)) => {
            let current = shell.borrow().file.clone();
            let resolved = resolve_open_path(&current, &raw);
            open_file(shell, resolved);
        }
        Ok(Command::Set(key, value)) => apply_set(shell, &key, &value),
        Ok(Command::Exec(action)) => execute(shell, action, 1),
        Ok(Command::Quit) => {
            // Release the borrow before `close()` (its handler re-borrows).
            let window = shell.borrow().window.clone();
            window.close();
        }
        Err(e) => shell.borrow().bar.set_message(&e),
    }
}

/// Apply a `:set key value` and honour the resulting [`SetEffect`].
fn apply_set(shell: &Rc<RefCell<Shell>>, key: &str, value: &str) {
    let effect = shell.borrow_mut().options.set(key, value);
    match effect {
        Err(e) => shell.borrow().bar.set_message(&e),
        Ok(SetEffect::Rerender) => {
            {
                let mut s = shell.borrow_mut();
                let o = s.options.clone();
                s.render_opts.page_width_px = o.page_width_px;
                s.render_opts.font_body = o.font_body.clone();
                s.render_opts.font_mono = o.font_mono.clone();
                s.render_opts.font_size_px = o.font_size_px;
                s.font_base_px = o.font_size_px as f64;
            }
            render_and_load(shell, true);
        }
        Ok(SetEffect::Recolor) => {
            let mut s = shell.borrow_mut();
            s.dark = s.options.default_recolor;
            let dark = s.dark;
            s.view.set_dark(dark);
            s.toc_view.set_dark(dark);
        }
        Ok(SetEffect::None) => {
            let mut s = shell.borrow_mut();
            s.scroll_step = s.options.scroll_step_px as i64;
            s.zoom_step = s.options.zoom_step;
            s.text_zoom_step = s.options.text_zoom_step;
        }
    }
}

/// Tab-complete the `:` command line, cycling on repeated presses.
fn do_completion(shell: &Rc<RefCell<Shell>>) {
    let mut s = shell.borrow_mut();
    if s.bar.prompt() != Some(Prompt::Command) {
        return;
    }
    let current = s.bar.input_query();

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
            let status = completion_status(comp);
            s.bar.set_input_query(&next);
            s.bar.set_message(&status);
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
    s.bar.set_input_query(&first);
    let comp = Completion {
        candidates,
        index: 0,
    };
    let status = completion_status(&comp);
    s.bar.set_message(&status);
    s.completion = Some(comp);
}

/// A compact completion echo: `[i/n] cand  cand  …`.
fn completion_status(comp: &Completion) -> String {
    let shown: Vec<&str> = comp.candidates.iter().map(String::as_str).take(8).collect();
    format!(
        "[{}/{}] {}",
        comp.index + 1,
        comp.candidates.len(),
        shown.join("  ")
    )
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

// ---------------------------------------------------------------------------
// Jumplist, history, themes
// ---------------------------------------------------------------------------

/// Record the current (async-queried) scroll position on the jumplist, then run
/// `after` — the actual jump — in the query's callback.
fn jump_to(shell: &Rc<RefCell<Shell>>, after: impl FnOnce(&mut Shell) + 'static) {
    let sh = shell.clone();
    let view = shell.borrow().view.clone();
    view.scroll_position(move |cur| {
        {
            let mut s = sh.borrow_mut();
            s.jumplist.push(cur);
            after(&mut s);
        }
        refresh_status(&sh);
    });
}

/// Fold the current position into the in-memory history (not yet written).
fn record_current_state(s: &mut Shell) {
    let state = FileState {
        scroll_y: s.last_scroll,
        zoom: s.zoom,
        text_zoom: s.text_zoom,
    };
    let file = s.file.clone();
    s.history.record(&file, state);
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

/// Convert a GDK keyval + modifiers into the core [`KeyPress`] abstraction.
/// Returns `None` for modifier-only or otherwise non-textual presses.
fn to_keypress(keyval: GdkKey, mods: ModifierType) -> Option<KeyPress> {
    let ctrl = mods.contains(ModifierType::CONTROL_MASK);
    let shift = mods.contains(ModifierType::SHIFT_MASK);

    let key = match keyval {
        GdkKey::Escape => Key::Escape,
        GdkKey::Tab | GdkKey::ISO_Left_Tab => Key::Tab,
        GdkKey::Return | GdkKey::KP_Enter => Key::Enter,
        GdkKey::BackSpace => Key::Backspace,
        GdkKey::space => Key::Space,
        other => {
            let c = other.to_unicode()?;
            if c.is_control() {
                return None;
            }
            Key::Char(c)
        }
    };
    Some(KeyPress::new(key, ctrl, shift))
}
