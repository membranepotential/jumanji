mod core;
mod shell;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use gtk::glib;

use crate::core::config::{self, Config};
use crate::core::source::Source;

/// A zathura-inspired markdown reader.
#[derive(Debug, Parser)]
#[command(name = "jumanji", version, about)]
struct Cli {
    /// The markdown file to open. Use `-` to read from standard input; with no
    /// argument at all, a piped stdin is read (`some-tool | jumanji`).
    file: Option<PathBuf>,

    /// Forward editor sync: jump to the rendered element nearest at-or-before
    /// this 1-based source line. If an instance already has the file open, the
    /// jump is forwarded to it over D-Bus and this process exits without opening
    /// a window (like zathura's `--synctex-forward`). Requires a file argument.
    #[arg(long, value_name = "LINE")]
    forward: Option<u32>,
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

    // Classify the input: a file path, an explicit `-`, or a bare pipe. `None`
    // means no file and stdin is an interactive terminal — nothing to read.
    let source = match Source::resolve(cli.file.as_deref(), std::io::stdin().is_terminal()) {
        Some(s) => s,
        None => {
            Cli::command()
                .error(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    "no input: give a markdown file, `-` to read stdin, or pipe into jumanji",
                )
                .exit();
        }
    };

    // `--forward` is a file-only feature (it targets a source line in a saved
    // document and can hand off to an instance that already has that file open).
    // It is meaningless for a stream, so reject the combination up front.
    if cli.forward.is_some() && source.is_stdin() {
        Cli::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "--forward requires a file argument; it cannot be used with stdin (`-`)",
            )
            .exit();
    }

    // Resolve a file source to an absolute, existing path (and take the D-Bus
    // forward-to-running-instance shortcut). Stdin passes straight through.
    let source = match source {
        Source::File(path) => {
            let path = match std::path::absolute(&path) {
                Ok(path) => path,
                Err(err) => {
                    eprintln!("jumanji: {}: {err}", path.display());
                    return ExitCode::FAILURE;
                }
            };
            if !path.exists() {
                eprintln!("jumanji: {}: no such file", path.display());
                return ExitCode::FAILURE;
            }
            // Forward editor sync (DESIGN D7): if `--forward <line>` is given and
            // an instance already has this file open, hand it the jump over
            // D-Bus and exit without opening a second window (zathura's
            // `--synctex-forward`). Otherwise fall through and open normally.
            if let Some(line) = cli.forward
                && shell::dbus::forward_to_running_instance(&path, line)
            {
                return ExitCode::SUCCESS;
            }
            Source::File(path)
        }
        Source::Stdin => Source::Stdin,
    };

    // Malformed config is surfaced but non-fatal: the reader must still open.
    let config = match Config::load(config::xdg_config_dir().as_deref()) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("jumanji: config error, using defaults: {err}");
            Config::default()
        }
    };

    let exit = shell::app::run(source, config, cli.forward);
    if exit == glib::ExitCode::SUCCESS {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
