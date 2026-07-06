//! Imperative shell: GTK4 window, WebKit view, bars, file watching.
//!
//! As thin as possible — logic lives in `core`.

pub mod app;
mod bar;
mod dbus;
mod view;
mod watch;
