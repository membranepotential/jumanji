use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use clap::{CommandFactory, Parser};
use gtk::glib;

use jumanji::core::config::{self, Config};
use jumanji::core::source::Source;
use jumanji::shell;

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

    /// Detach from the terminal at startup: the reader opens and the shell
    /// prompt returns immediately, as if launched with a trailing `&`.
    /// Overrides the `background` config option.
    #[arg(long, overrides_with = "foreground")]
    background: bool,

    /// Stay in the foreground, holding the terminal until the window closes.
    /// Overrides the `background` config option.
    #[arg(long, overrides_with = "background")]
    foreground: bool,
}

impl Cli {
    /// Whether to detach from the terminal, given the configured default. An
    /// explicit flag always wins; `--background` and `--foreground` override
    /// each other POSIX-style (last one on the line wins), so at most one of
    /// the two is ever set here.
    fn should_background(&self, configured: bool) -> bool {
        match (self.background, self.foreground) {
            (true, _) => true,
            (_, true) => false,
            _ => configured,
        }
    }
}

/// Re-execute ourselves detached from the terminal and report whether the child
/// is on its way, so the caller can exit and hand the prompt back.
///
/// Re-exec rather than `fork`: forking a process that is about to bring up GTK,
/// WebKit and D-Bus — all of which spawn threads — only reliably copies the
/// calling thread, and the standard fix (`libc::fork` behind `unsafe`) would add
/// a dependency to sidestep a problem a fresh process does not have.
///
/// A failure here is never fatal: backgrounding is a convenience, and it must
/// not be the reason the reader fails to open. The caller falls through to
/// running in the foreground.
fn spawn_detached() -> bool {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("jumanji: cannot background (locating own binary: {err}); staying in front");
            return false;
        }
    };

    let mut command = Command::new(exe);
    command
        // Our own arguments, verbatim — the child re-resolves them in the same
        // working directory, so relative paths still mean the same file.
        .args(std::env::args_os().skip(1))
        // `--foreground` last, so the child never backgrounds itself again: the
        // two flags override each other last-one-wins, which neutralises both a
        // configured `background = true` and an explicit `--background`.
        .arg("--foreground")
        // Leave the terminal's process group, so the shell does not track the
        // child as a job and a terminal hangup (SIGHUP to the foreground group)
        // does not take the reader down with it.
        .process_group(0)
        // Detach the standard streams too. A backgrounded process that outlives
        // its terminal would otherwise write into a closed tty, where `eprintln!`
        // panics on the broken pipe — and until then it would scribble over the
        // user's prompt.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match command.spawn() {
        // Deliberately not waited on: the child is reparented to init when we
        // exit, which is the whole point.
        Ok(_) => true,
        Err(err) => {
            eprintln!("jumanji: cannot background ({err}); staying in the foreground");
            false
        }
    }
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

    // Backgrounding a stream is impossible: we are the consumer of a pipe the
    // shell's producer is still writing into, and detaching means nulling stdin.
    // A configured default is silently skipped further down, but an explicit
    // request deserves an explicit "no" rather than a window that renders
    // nothing.
    if cli.background && source.is_stdin() {
        Cli::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "--background requires a file argument; it cannot be used with stdin (`-`)",
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

    // Hand the prompt back, last thing before the window opens. Everything above
    // reports its problems on the terminal — a bad path, a malformed config, a
    // forwarded jump — and the user must still see all of it, so the detach
    // happens only once there is nothing left to say. A stdin source never
    // detaches (see the `--background` conflict above); with `background = true`
    // configured, piping simply keeps working in the foreground.
    if !source.is_stdin() && cli.should_background(config.options.background) && spawn_detached() {
        return ExitCode::SUCCESS;
    }

    let exit = shell::app::run(source, config, cli.forward);
    if exit == glib::ExitCode::SUCCESS {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("jumanji").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn background_flags_override_the_config_value() {
        assert!(!parse(&["a.md"]).should_background(false));
        assert!(parse(&["a.md"]).should_background(true));
        assert!(parse(&["--background", "a.md"]).should_background(false));
        assert!(!parse(&["--foreground", "a.md"]).should_background(true));
    }

    #[test]
    fn the_last_background_flag_wins() {
        // Load-bearing for the re-exec, which appends `--foreground` to the
        // original argument list to stop the child backgrounding itself again:
        // that only works if a trailing `--foreground` beats a leading
        // `--background` instead of erroring out as a conflict.
        assert!(!parse(&["--background", "a.md", "--foreground"]).should_background(true));
        assert!(parse(&["--foreground", "a.md", "--background"]).should_background(false));
    }
}
