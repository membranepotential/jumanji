//! The GTK4 + WebKitGTK shell: the Linux implementation of the three toolkit
//! traits (`controller::toolkit`), plus the wiring that adapts GTK events into
//! [`Controller`](crate::controller::session::Controller) calls.
//!
//! Each trait has exactly one implementation here: [`view::View`],
//! [`chrome::GtkChrome`], [`host::GlibHost`]; [`app::Gtk`] bundles the three.

mod app;
mod bar;
mod chrome;
pub mod dbus;
mod host;
mod toc;
mod view;

pub use app::run;
