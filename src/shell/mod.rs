//! Imperative shells: one per toolkit.
//!
//! [`gtk`] is the Linux shell — GTK4 window, WebKitGTK view, bars, glib host —
//! and today the only one. It is deliberately a *sibling* rather than the
//! shell: everything above it (`core`, `controller`) is toolkit-agnostic, so a
//! second shell (a tao + wry macOS one, see `docs/research/05-macos-port.md`)
//! sits beside this module and implements the same three traits rather than
//! displacing anything.
pub mod gtk;
