//! The GTK4 application: window, capture-phase key dispatch, and the mapping
//! from [`Action`]s to `webkit6` calls. As thin as the design allows — the
//! keymap, config, and render pipeline all live in `core`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gdk::{Key as GdkKey, ModifierType};
use gtk::glib;
use gtk::glib::variant::ToVariant;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, EventControllerKey, EventControllerScroll,
    EventControllerScrollFlags, Orientation, PropagationPhase,
};
use webkit6::LoadEvent;
use webkit6::prelude::*;

use crate::core::config::{self, Config};
use crate::core::keymap::{Key, KeyPress, Keymap, MatchResult, Matcher};
use crate::core::pipeline::{self, Options as RenderOptions};
use crate::core::{Action, Direction, Heading, Mode};

use super::bar::Bar;
use super::dbus;
use super::view::View;
use super::watch::{FileEvent, Watch};

const APP_ID: &str = "org.membranepotential.jumanji";

/// Mutable shell state shared across GTK callbacks.
struct Shell {
    file: PathBuf,
    render_opts: RenderOptions,
    scroll_step: i64,
    /// Geometric zoom step (added to the webkit `zoom_level` per step).
    zoom_step: f64,
    /// Text-zoom step: fraction of the base font size added per step.
    text_zoom_step: f64,
    /// Base body font size in px (text-zoom 100% reference; from config).
    font_base_px: f64,
    /// Current text-zoom factor (1.0 = 100%); geometric zoom lives in the webview.
    text_zoom: f64,
    matcher: Matcher,
    view: View,
    bar: Bar,
    window: ApplicationWindow,
    toc: Vec<Heading>,
    section: usize,
    dark: bool,
    /// Whether the initial [`LoadEvent::Finished`] has fired. Key/D-Bus actions
    /// are no-ops before this; the D-Bus `loaded` flag lets clients (tests,
    /// editor integrations) wait for a driveable window.
    loaded: bool,
    /// Scroll offset to restore once the next load finishes (reload only).
    pending_restore: Option<f64>,
    _watch: Option<Watch>,
    /// Keeps the per-instance D-Bus name owned for the process lifetime.
    _dbus: Option<gtk::gio::OwnerId>,
}

/// Launch the application for `file` with the resolved `config`.
pub fn run(file: PathBuf, config: Config) -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();

    let keymap = Rc::new(config.keymap);
    let options = config.options;

    app.connect_activate(move |app| {
        build_ui(app, file.clone(), options.clone(), keymap.clone());
    });

    // We parse args ourselves (see `main`); don't let GTK interpret argv.
    app.run_with_args::<&str>(&[])
}

fn build_ui(
    app: &Application,
    file: PathBuf,
    options: crate::core::config::Options,
    keymap: Rc<Keymap>,
) {
    let view = View::new(options.selection_clipboard);
    let bar = Bar::new();

    let layout = GtkBox::new(Orientation::Vertical, 0);
    layout.append(view.widget());
    layout.append(bar.widget());

    let window = ApplicationWindow::builder()
        .application(app)
        .title("jumanji")
        .default_width((options.page_width_px + 80).max(640) as i32)
        .default_height(800)
        .child(&layout)
        .build();

    let filename = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_string_lossy().into_owned());
    bar.set_filename(&filename);

    let shell = Rc::new(RefCell::new(Shell {
        file,
        render_opts: RenderOptions {
            page_width_px: options.page_width_px,
            font_body: options.font_body.clone(),
            font_mono: options.font_mono.clone(),
            font_size_px: options.font_size_px,
        },
        scroll_step: options.scroll_step_px as i64,
        zoom_step: options.zoom_step,
        text_zoom_step: options.text_zoom_step,
        font_base_px: options.font_size_px as f64,
        text_zoom: 1.0,
        matcher: Matcher::new(Mode::Normal),
        view: view.clone(),
        bar: bar.clone(),
        window: window.clone(),
        toc: Vec::new(),
        section: 0,
        dark: options.default_recolor,
        loaded: false,
        pending_restore: None,
        _watch: None,
        _dbus: None,
    }));

    connect_load_finished(&shell);
    connect_keys(&shell, keymap);
    connect_scroll(&shell);
    connect_search_entry(&shell);
    start_watch(&shell);
    serve_dbus(&shell);

    window.present();
    view.widget().grab_focus();

    // Initial render + load.
    render_and_load(&shell, false);
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
    let path = s.file.clone();
    match std::fs::read_to_string(&path) {
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
        Err(err) => {
            s.bar
                .set_message(&format!("cannot read {}: {err}", path.display()));
        }
    }
}

/// On load completion, apply recolor state, restore any pending scroll offset,
/// and refresh the percentage indicator.
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
            // Re-apply both zoom axes: the inline `--font-size`/`--zoom`
            // custom properties are lost on reload.
            if s.text_zoom != 1.0 {
                s.view.set_text_zoom_px(s.font_base_px * s.text_zoom);
            }
            let zoom = s.view.zoom_level();
            if zoom != 1.0 {
                s.view.set_zoom(zoom);
            }
            if let Some(y) = s.pending_restore {
                s.view.restore_scroll(y);
            }
        }
        {
            let mut s = shell.borrow_mut();
            s.pending_restore = None;
            s.loaded = true;
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
        // While the input bar is open, let the entry consume typing; only
        // intercept Esc to cancel it.
        if shell.borrow().bar.is_input_visible() {
            if keyval == GdkKey::Escape {
                execute(&shell, Action::Abort, 1);
                return glib::Propagation::Stop;
            }
            return glib::Propagation::Proceed;
        }

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
/// webview (capture phase, so we intercept before WebKit's own scroll handling).
/// A plain wheel with no Ctrl is passed through untouched, so normal scrolling
/// still works. Wheel-up (`dy < 0`) zooms in, wheel-down zooms out.
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
        let text = mods.contains(ModifierType::SHIFT_MASK);
        let action = match (text, dy < 0.0) {
            (false, true) => Action::ZoomIn,
            (false, false) => Action::ZoomOut,
            (true, true) => Action::TextZoomIn,
            (true, false) => Action::TextZoomOut,
        };
        execute(&shell, action, 1);
        refresh_status(&shell);
        glib::Propagation::Stop
    });

    window.add_controller(controller);
}

fn connect_search_entry(shell: &Rc<RefCell<Shell>>) {
    let entry = shell.borrow().bar.entry().clone();
    let shell = shell.clone();
    entry.connect_activate(move |_| {
        let query = shell.borrow().bar.input_query();
        {
            let s = shell.borrow();
            if query.is_empty() {
                s.view.find_clear();
            } else {
                s.view.find(&query);
            }
            s.bar.close_input();
            s.view.widget().grab_focus();
        }
        refresh_status(&shell);
    });
}

fn start_watch(shell: &Rc<RefCell<Shell>>) {
    let path = shell.borrow().file.clone();
    let handler_shell = shell.clone();
    let watch = Watch::start(&path, move |event| match event {
        FileEvent::Changed => render_and_load(&handler_shell, true),
        FileEvent::Removed => {
            handler_shell
                .borrow()
                .bar
                .set_message("file removed — showing last render");
        }
    });
    match watch {
        Ok(w) => shell.borrow_mut()._watch = Some(w),
        Err(err) => shell
            .borrow()
            .bar
            .set_message(&format!("live reload disabled: {err}")),
    }
}

/// Register the per-instance D-Bus automation surface (DESIGN.md D7 foundation).
/// `GetState` snapshots shell state plus a live (async) scroll query;
/// `ExecuteAction` parses an action string and runs it through the same
/// [`execute`] path the keyboard uses. Failure to own the name is non-fatal.
fn serve_dbus(shell: &Rc<RefCell<Shell>>) {
    let get_state = {
        let shell = shell.clone();
        Rc::new(move |invocation: gtk::gio::DBusMethodInvocation| {
            // Snapshot the synchronous state under a short borrow, then query
            // the live scroll offset async and complete the reply in its
            // callback — never blocking the main loop.
            let (view, file, dark, zoom, text_zoom, section, toc_len, loaded) = {
                let s = shell.borrow();
                (
                    s.view.clone(),
                    s.file.to_string_lossy().into_owned(),
                    s.dark,
                    s.view.zoom_level(),
                    s.text_zoom,
                    s.section,
                    s.toc.len(),
                    s.loaded,
                )
            };
            view.scroll_state(move |y, pct, content_width| {
                let json = state_json(
                    &file,
                    y,
                    pct,
                    content_width,
                    dark,
                    zoom,
                    text_zoom,
                    section,
                    toc_len,
                    loaded,
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

    let owner = dbus::serve(dbus::Automation {
        get_state,
        execute_action,
    });
    shell.borrow_mut()._dbus = owner;
}

/// Serialize the reader state as the compact JSON object `GetState` returns.
/// `mode` is fixed to `"normal"` in M1 (the only user-reachable mode; TOC mode
/// arrives in M2).
#[allow(clippy::too_many_arguments)]
fn state_json(
    file: &str,
    scroll_y: f64,
    scroll_percent: u32,
    content_width: f64,
    dark: bool,
    zoom: f64,
    text_zoom: f64,
    section: usize,
    toc_len: usize,
    loaded: bool,
) -> String {
    format!(
        "{{\"file\":{file},\"scroll_y\":{scroll_y},\"scroll_percent\":{scroll_percent},\
         \"content_width\":{content_width},\
         \"dark\":{dark},\"zoom\":{zoom},\"text_zoom\":{text_zoom},\"mode\":\"normal\",\
         \"section\":{section},\"toc_len\":{toc_len},\"loaded\":{loaded}}}",
        file = json_string(file),
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

/// The right-hand zoom indicator: `{geometric}%/{text}%T` (e.g. `150%/120%T`)
/// when *either* axis differs from 100%, empty when both are exactly 100%.
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
        Action::GotoTop => {
            s.section = 0;
            s.view.scroll_to_top();
        }
        Action::GotoBottom => {
            s.section = s.toc.len().saturating_sub(1);
            s.view.scroll_to_bottom();
        }
        Action::GotoSection(n) => {
            if !s.toc.is_empty() {
                let idx = ((n as usize).saturating_sub(1)).min(s.toc.len() - 1);
                s.section = idx;
                let anchor = s.toc[idx].anchor.clone();
                s.view.scroll_to_anchor(&anchor);
            }
        }
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
        Action::ZoomIn => {
            let level = s.view.zoom_level() + s.zoom_step * count as f64;
            s.view.set_zoom(level);
        }
        Action::ZoomOut => {
            let level = s.view.zoom_level() - s.zoom_step * count as f64;
            s.view.set_zoom(level);
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
            // Reset both axes to 100%.
            s.view.set_zoom(1.0);
            s.text_zoom = 1.0;
            s.view.set_text_zoom_px(s.font_base_px);
        }
        Action::SearchStart => {
            s.bar.open_input("/");
        }
        Action::SearchNext => s.view.find_next(),
        Action::SearchPrevious => s.view.find_previous(),
        Action::Recolor => {
            s.dark = !s.dark;
            s.view.set_dark(s.dark);
        }
        Action::Reload => {
            drop(s);
            render_and_load(shell, true);
        }
        Action::ToggleToc => s.bar.set_message("table of contents: not implemented (M2)"),
        Action::CommandLine => s.bar.set_message("command line: not implemented (M2)"),
        Action::Abort => {
            s.matcher.set_mode(Mode::Normal);
            if s.bar.is_input_visible() {
                s.bar.close_input();
                s.view.find_clear();
                s.view.widget().grab_focus();
            }
        }
        Action::Quit => s.window.close(),
    }
}

/// Refresh the right-hand status: scroll percentage plus the pending-key echo.
fn refresh_status(shell: &Rc<RefCell<Shell>>) {
    let (view, bar, pending, zoom) = {
        let s = shell.borrow();
        (
            s.view.clone(),
            s.bar.clone(),
            s.matcher.pending_indicator(),
            zoom_indicator(s.view.zoom_level(), s.text_zoom),
        )
    };
    view.scroll_percent(move |pct| bar.set_status_right(pct, &pending, &zoom));
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
