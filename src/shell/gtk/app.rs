//! The GTK4 application: the window, the capture-phase event controllers, and
//! the D-Bus surface. Wiring only — every GTK event is translated into a call
//! on [`Controller`], which owns all of the reader's behaviour; nothing here
//! decides anything.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk::{Key as GdkKey, ModifierType};
use gtk::gio;
use gtk::glib;
use gtk::glib::variant::ToVariant;
use gtk::prelude::*;
use gtk::{
    Application, ApplicationWindow, EventControllerKey, EventControllerMotion,
    EventControllerScroll, EventControllerScrollFlags, EventSequenceState, GestureClick,
    PropagationPhase,
};
use webkit6::LoadEvent;
use webkit6::prelude::*;

use crate::controller::session::{Controller, KeyOutcome};
use crate::controller::toolkit::{Toolkit, Viewport};
use crate::core::Action;
use crate::core::config::{Config, Options};
use crate::core::keymap::{Key, KeyPress, Keymap};
use crate::core::source::Source;

use super::chrome::GtkChrome;
use super::dbus;
use super::host::GlibHost;
use super::view::{LastSelection, View};

const APP_ID: &str = "org.membranepotential.jumanji";

/// The GTK4 + WebKitGTK toolkit: the three implementations this module wires
/// the controller to.
pub struct Gtk;

impl Toolkit for Gtk {
    type Viewport = View;
    type Chrome = GtkChrome;
    type Host = GlibHost;
}

/// Launch the application for `source` with the resolved `config`. `forward` is
/// an optional `--forward <line>` to jump to once the initial load finishes
/// (file sources only; rejected for stdin before we get here).
pub fn run(source: Source, config: Config, forward: Option<u32>) -> glib::ExitCode {
    // NON_UNIQUE: every `jumanji <file>` is its own independent process, like
    // zathura. Without it, GApplication's single-instance negotiation makes a
    // second launch forward *activation* to the first process (which then
    // re-runs `build_ui` for its own file — wrong document, a duplicate window,
    // and a clash with our per-PID D-Bus surface: the object is re-exported at
    // the same path and the PID name is re-owned, both of which fail). Each
    // instance still owns a distinct `…jumanji.PID-<pid>` name for automation.
    let app = Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let keymap = config.keymap;
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

/// Build the widgets, hand them to a [`Controller`] — which takes the reader
/// all the way to its first render — and adapt this window's events onto it.
fn build_ui(
    app: &Application,
    source: Source,
    options: Options,
    keymap: Keymap,
    forward: Option<u32>,
) {
    // Shared with the host so the copy-on-select write and the find-clobbers-
    // PRIMARY restore agree on what the user's last real selection was.
    let last_selection: LastSelection = Rc::new(RefCell::new(None));
    let view = View::new(last_selection.clone());
    let chrome = GtkChrome::new(view.widget());

    let window = ApplicationWindow::builder()
        .application(app)
        .title("jumanji")
        .default_width((options.page_width_px + 80).max(640) as i32)
        .default_height(800)
        .child(chrome.widget())
        .build();

    let host = GlibHost::new(window.clone(), last_selection);

    let controller = Controller::<Gtk>::new(
        view.clone(),
        chrome.clone(),
        host,
        source,
        options,
        keymap,
        forward,
    );

    install_view_handlers(&controller, &view);
    connect_toc_activate(&controller, &chrome);
    connect_load_finished(&controller, &view);
    connect_keys(&controller, &window);
    connect_scroll(&controller, &window);
    connect_buttons(&controller, &window);
    connect_motion(&controller, &window, &view);
    connect_input_entry(&controller, &chrome);
    connect_close(&controller, &window);
    serve_dbus(&controller);

    window.present();
    view.focus();
}

/// Wire the view's controller-supplied callbacks: the in-page message channel
/// (selection, native scroll, link hints, editor sync) and navigation routing
/// (link clicks).
fn install_view_handlers(controller: &Controller<Gtk>, view: &View) {
    {
        let controller = controller.clone();
        view.set_message_handler(move |name, payload| controller.on_message(name, payload));
    }
    {
        let controller = controller.clone();
        view.set_navigate_handler(move |uri| controller.on_navigate(&uri));
    }
}

/// Wire double-click / activate on a TOC row to the same jump path as `Enter`.
fn connect_toc_activate(controller: &Controller<Gtk>, chrome: &GtkChrome) {
    let controller = controller.clone();
    chrome.set_toc_activate_handler(move || controller.on_toc_activated());
}

fn connect_load_finished(controller: &Controller<Gtk>, view: &View) {
    let webview = view.widget().clone();
    let controller = controller.clone();
    webview.connect_load_changed(move |_, event| {
        if event == LoadEvent::Finished {
            controller.on_load_finished();
        }
    });
}

/// Capture-phase key dispatch on the toplevel: every press becomes a
/// [`KeyPress`] (or `None`, for a press with no textual meaning) and the
/// controller decides whether it was consumed.
fn connect_keys(controller: &Controller<Gtk>, window: &ApplicationWindow) {
    let keys = EventControllerKey::new();
    keys.set_propagation_phase(PropagationPhase::Capture);

    let controller = controller.clone();
    keys.connect_key_pressed(move |_, keyval, _keycode, mods| {
        match controller.on_key(to_keypress(keyval, mods)) {
            KeyOutcome::Consumed => glib::Propagation::Stop,
            KeyOutcome::PassThrough => glib::Propagation::Proceed,
        }
    });

    window.add_controller(keys);
}

/// Bind Ctrl+wheel → geometric zoom and Ctrl+Shift+wheel → text zoom on the
/// window (capture phase, so we intercept before WebKit's own scroll handling).
fn connect_scroll(controller: &Controller<Gtk>, window: &ApplicationWindow) {
    let scroll = EventControllerScroll::new(EventControllerScrollFlags::BOTH_AXES);
    scroll.set_propagation_phase(PropagationPhase::Capture);

    let controller = controller.clone();
    scroll.connect_scroll(move |ctrl, _dx, dy| {
        let mods = ctrl.current_event_state();
        if !mods.contains(ModifierType::CONTROL_MASK) || dy == 0.0 {
            return glib::Propagation::Proceed;
        }
        controller.on_wheel_zoom(dy, mods.contains(ModifierType::SHIFT_MASK));
        glib::Propagation::Stop
    });

    window.add_controller(scroll);
}

/// X11/evdev numbering for the two side buttons every mouse with a thumb rest
/// reports. GDK has named constants only for the primary three, so these are
/// spelled out here rather than guessed at the call site.
const BUTTON_BACK: u32 = 8;
const BUTTON_FORWARD: u32 = 9;

/// Bind the mouse's back/forward side buttons to the cross-document jumplist
/// (DESIGN D10), which is jumanji's browser-history analogue: they do exactly
/// what `Ctrl-o` / `Ctrl-i` do, so a thumb click and a keystroke cannot
/// disagree about where "back" goes.
///
/// Capture phase on the toplevel, like the key/scroll controllers — both
/// because a controller on the WebView never sees these events (D5a), and
/// because WebKit would otherwise walk its *own* session history, which is not
/// the history the reader navigates: jumanji loads each document itself.
fn connect_buttons(controller: &Controller<Gtk>, window: &ApplicationWindow) {
    let clicks = GestureClick::new();
    // GestureSingle listens to the primary button only until told otherwise;
    // 0 means "any button", which is the only way to hear buttons 8 and 9.
    clicks.set_button(0);
    clicks.set_propagation_phase(PropagationPhase::Capture);

    let controller = controller.clone();
    clicks.connect_pressed(move |ctrl, _n_press, _x, _y| {
        let action = match ctrl.current_button() {
            BUTTON_BACK => Action::JumpBackward,
            BUTTON_FORWARD => Action::JumpForward,
            _ => return,
        };
        ctrl.set_state(EventSequenceState::Claimed);
        controller.execute(action, 1);
    });

    window.add_controller(clicks);
}

/// Track the pointer so Ctrl+wheel can anchor at the cursor. Capture phase on
/// the toplevel, mirroring the key/scroll controllers — a controller on the
/// WebView itself never sees these events (DESIGN.md D5a) — so the coordinates
/// arrive in *window* space and are translated into the view here, which is the
/// one step that needs a widget hierarchy.
fn connect_motion(controller: &Controller<Gtk>, window: &ApplicationWindow, view: &View) {
    let motion = EventControllerMotion::new();
    motion.set_propagation_phase(PropagationPhase::Capture);

    let controller = controller.clone();
    let webview = view.widget().clone();
    let toplevel = window.clone();
    motion.connect_motion(move |_, x, y| {
        let src = gtk::graphene::Point::new(x as f32, y as f32);
        let p = toplevel.compute_point(&webview, &src).unwrap_or(src);
        controller.on_pointer_moved(p.x() as f64, p.y() as f64);
    });

    window.add_controller(motion);
}

/// Wire the input bar's `Enter`.
fn connect_input_entry(controller: &Controller<Gtk>, chrome: &GtkChrome) {
    let entry = chrome.entry().clone();
    let controller = controller.clone();
    entry.connect_activate(move |_| controller.on_input_submitted());
}

/// Wire window close to the controller's flush hook, so `q` and a
/// window-manager close both persist per-file window-state.
fn connect_close(controller: &Controller<Gtk>, window: &ApplicationWindow) {
    let controller = controller.clone();
    window.connect_close_request(move |_| {
        controller.on_close();
        glib::Propagation::Proceed
    });
}

/// Register the per-instance D-Bus automation surface (DESIGN.md D7
/// foundation) over the controller's automation methods.
fn serve_dbus(controller: &Controller<Gtk>) {
    let get_state = {
        let controller = controller.clone();
        Rc::new(move |invocation: gio::DBusMethodInvocation| {
            controller.state(move |json| {
                invocation.return_value(Some(&(json,).to_variant()));
            });
        }) as dbus::GetState
    };

    let execute_action = {
        let controller = controller.clone();
        Rc::new(move |action: &str, count: u32| controller.execute_str(action, count))
            as dbus::ExecuteAction
    };

    let goto_line = {
        let controller = controller.clone();
        Rc::new(move |line: u32| controller.goto_source_line(line)) as dbus::GotoLine
    };

    // The name is owned for the process lifetime either way: the id is a plain
    // handle, not a guard, so dropping it releases nothing (see `dbus::serve`).
    let _owner = dbus::serve(dbus::Automation {
        get_state,
        execute_action,
        goto_line,
    });
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
