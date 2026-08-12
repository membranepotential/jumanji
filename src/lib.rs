//! jumanji — a zathura-inspired markdown reader.
//!
//! Split into a functional core (`core`, pure and GTK-free) and an imperative
//! shell (`shell`, GTK4 + WebKitGTK); see `docs/DESIGN.md`. Exposed as a
//! library, in addition to the `jumanji` binary, so integration benches and
//! tests can reach `core::pipeline` directly.

pub mod core;
pub mod shell;
