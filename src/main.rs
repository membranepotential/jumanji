mod core;
mod shell;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use gtk::glib;

use crate::core::config::{self, Config};

/// A zathura-inspired markdown reader.
#[derive(Debug, Parser)]
#[command(name = "jumanji", version, about)]
struct Cli {
    /// The markdown file to open.
    file: PathBuf,
}

fn main() -> ExitCode {
    // WebKitGTK's DMABUF renderer intermittently drops composited layers while
    // scrolling on some Intel/Mesa X11 GPUs (tables, code blocks and diagrams
    // each live in their own `overflow-x: auto` scroll box, which WebKit
    // promotes to a composited layer that then flickers out and back) — a known
    // upstream artifact (WebKit bug 262607 family). Disabling that renderer is
    // the ecosystem-standard workaround. Set only when the user hasn't: any
    // pre-existing value (even "0"/empty) wins, so this stays an escape hatch
    // without a config option.
    //
    // This MUST run before WebKit spawns its first render/web process — i.e.
    // before any GTK/WebKit initialisation, which only happens inside
    // `shell::app::run`; nothing above it touches GTK. SAFETY: `set_var` is
    // `unsafe` under edition 2024 because a concurrent getenv/setenv is UB, but
    // this is the first statement in `main`, single-threaded, long before any
    // thread (GTK's included) is spawned.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        unsafe {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }

    let cli = Cli::parse();

    let file = match std::path::absolute(&cli.file) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("jumanji: {}: {err}", cli.file.display());
            return ExitCode::FAILURE;
        }
    };

    if !file.exists() {
        eprintln!("jumanji: {}: no such file", file.display());
        return ExitCode::FAILURE;
    }

    // Malformed config is surfaced but non-fatal: the reader must still open.
    let config = match Config::load(config::xdg_config_dir().as_deref()) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("jumanji: config error, using defaults: {err}");
            Config::default()
        }
    };

    let exit = shell::app::run(file, config);
    if exit == glib::ExitCode::SUCCESS {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
