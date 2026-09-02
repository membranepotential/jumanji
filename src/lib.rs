//! jumanji — a zathura-inspired markdown reader.
//!
//! Three layers (DESIGN D2a): a functional core (`core`, pure and
//! toolkit-free), a toolkit-agnostic controller (`controller`, the session
//! and every flow, generic over the `Viewport` / `Chrome` / `Host` traits it
//! defines) and a toolkit shell (`shell::gtk`, GTK4 + WebKitGTK, wiring
//! only). Exposed as a library, in addition to the `jumanji` binary, so
//! integration benches and tests can reach `core::pipeline` directly.

pub mod controller;
pub mod core;
pub mod shell;
