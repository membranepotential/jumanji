//! Controller: the toolkit-agnostic imperative half of the reader.
//!
//! Sits between the pure [`core`](crate::core) and a toolkit shell
//! (`shell::gtk` on Linux). Everything here is display-free in the sense that
//! matters: it never imports `gtk`, `glib`, or `webkit6`, and it drives the
//! window through the small traits in [`toolkit`] — which a fake can implement
//! for unit tests, and a second shell can implement for another platform
//! (see `docs/research/05-macos-port.md`).
//!
//! The one thing this layer *is* allowed to know about the web platform is the
//! DOM: [`scripts`] holds the user scripts and JS snippets the shells inject,
//! byte-identical on every toolkit. That is "shell viewport glue" in DESIGN
//! D12's terms — never content-pipeline JS, which D3 forbids.

#[cfg(test)]
mod fake;
pub mod page;
pub mod scripts;
pub mod session;
pub mod stdin;
#[cfg(test)]
mod tests;
pub mod toolkit;
pub mod watch;
