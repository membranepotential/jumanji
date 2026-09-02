//! Imperative shell: GTK4 window, WebKit view, bars, main-loop host.
//!
//! As thin as possible — logic lives in `core` and `controller`. Each of the
//! three toolkit traits (`controller::toolkit`) has exactly one implementation
//! here: [`view::View`], [`chrome::GtkChrome`], [`host::GlibHost`].

pub mod app;
mod bar;
mod chrome;
pub mod dbus;
mod host;
mod toc;
mod view;
