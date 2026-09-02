//! The GTK/glib implementation of [`Host`]: the main loop and the operating
//! system, as the controller needs them.

use std::time::Duration;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{ApplicationWindow, gdk};

use crate::controller::toolkit::Host;
use crate::core::config::SelectionClipboard;

use super::view::LastSelection;

/// A running glib timeout, cancelled when dropped.
///
/// glib's `SourceId` does *not* remove its source on drop, so every repeating
/// timer the controller owns would otherwise outlive the thing that started it
/// (and keep firing into a dead window). This is the one place that pairing
/// lives.
pub struct SourceGuard(Option<glib::SourceId>);

impl Drop for SourceGuard {
    fn drop(&mut self) {
        if let Some(id) = self.0.take() {
            id.remove();
        }
    }
}

/// The GTK host: the application window (for `quit`) and the shared record of
/// the user's last real selection (for the find-clobbers-PRIMARY workaround in
/// [`View::new`](super::view::View::new)).
#[derive(Clone)]
pub struct GlibHost {
    window: ApplicationWindow,
    last_selection: LastSelection,
}

impl GlibHost {
    pub fn new(window: ApplicationWindow, last_selection: LastSelection) -> Self {
        Self {
            window,
            last_selection,
        }
    }
}

impl Host for GlibHost {
    type Timer = SourceGuard;

    fn defer(&self, delay: Duration, f: impl FnOnce() + 'static) {
        glib::timeout_add_local_once(delay, f);
    }

    fn interval(&self, period: Duration, mut f: impl FnMut() + 'static) -> Self::Timer {
        SourceGuard(Some(glib::timeout_add_local(period, move || {
            f();
            glib::ControlFlow::Continue
        })))
    }

    fn spawn_blocking<R: Send + 'static>(
        &self,
        work: impl FnOnce() -> R + Send + 'static,
        done: impl FnOnce(Option<R>) + 'static,
    ) {
        glib::spawn_future_local(async move {
            // `Err` is a panic in the worker; the controller's contract is that
            // it gets `None` and carries on.
            let result = gio::spawn_blocking(work).await;
            done(result.ok());
        });
    }

    fn open_external(&self, uri: &str) -> Result<(), String> {
        gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>)
            .map_err(|e| e.to_string())
    }

    fn copy_selection(&self, text: &str, target: SelectionClipboard) {
        *self.last_selection.borrow_mut() = Some(text.to_string());
        if let Some(display) = gdk::Display::default() {
            let clipboard = match target {
                SelectionClipboard::Primary => display.primary_clipboard(),
                SelectionClipboard::Clipboard => display.clipboard(),
            };
            clipboard.set_text(text);
        }
    }

    fn spawn_detached(&self, argv: &[String]) -> Result<(), String> {
        let owned: Vec<std::ffi::OsString> = argv.iter().map(std::ffi::OsString::from).collect();
        let refs: Vec<&std::ffi::OsStr> = owned.iter().map(AsRef::as_ref).collect();
        // gio::Subprocess reaps the child via the main loop and never blocks
        // us; we drop the handle (fire-and-forget), matching zathura.
        gio::Subprocess::newv(&refs, gio::SubprocessFlags::NONE)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn quit(&self) {
        // `close()` goes through the same close-request path a window-manager
        // close takes, so the controller's history flush runs either way.
        self.window.close();
    }
}
