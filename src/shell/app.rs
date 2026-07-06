//! The GTK4 application: window, capture-phase key dispatch, and the mapping
//! from [`Action`]s to `webkit6` calls. As thin as the design allows — the
//! keymap, config, and render pipeline all live in `core`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gdk::{Key as GdkKey, ModifierType};
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, Box as GtkBox, EventControllerKey, Orientation,
    PropagationPhase,
};
use webkit6::LoadEvent;
use webkit6::prelude::*;

use crate::core::config::Config;
use crate::core::keymap::{Key, KeyPress, Keymap, MatchResult, Matcher};
use crate::core::pipeline::{self, Options as RenderOptions};
use crate::core::{Action, Direction, Heading, Mode};

use super::bar::Bar;
use super::view::View;
use super::watch::{FileEvent, Watch};

const APP_ID: &str = "org.membranepotential.jumanji";

/// Mutable shell state shared across GTK callbacks.
struct Shell {
    file: PathBuf,
    render_opts: RenderOptions,
    scroll_step: i64,
    zoom_step: f64,
    matcher: Matcher,
    view: View,
    bar: Bar,
    window: ApplicationWindow,
    toc: Vec<Heading>,
    section: usize,
    dark: bool,
    /// Scroll offset to restore once the next load finishes (reload only).
    pending_restore: Option<f64>,
    _watch: Option<Watch>,
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
    let view = View::new();
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
        },
        scroll_step: options.scroll_step_px as i64,
        zoom_step: options.zoom_step,
        matcher: Matcher::new(Mode::Normal),
        view: view.clone(),
        bar: bar.clone(),
        window: window.clone(),
        toc: Vec::new(),
        section: 0,
        dark: options.default_recolor,
        pending_restore: None,
        _watch: None,
    }));

    connect_load_finished(&shell);
    connect_keys(&shell, keymap);
    connect_search_entry(&shell);
    start_watch(&shell);

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
            if let Some(y) = s.pending_restore {
                s.view.restore_scroll(y);
            }
        }
        shell.borrow_mut().pending_restore = None;
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
        Action::ZoomReset => s.view.set_zoom(1.0),
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
    let (view, bar, pending) = {
        let s = shell.borrow();
        (s.view.clone(), s.bar.clone(), s.matcher.pending_indicator())
    };
    view.scroll_percent(move |pct| bar.set_status_right(pct, &pending));
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
