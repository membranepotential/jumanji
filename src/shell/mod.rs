//! Imperative shell: GTK4 window, WebKit view, bars, file watching.
//!
//! As thin as possible — logic lives in `core`.

pub mod app;
mod bar;
pub mod dbus;
mod stdin;
mod toc;
mod view;
mod watch;
