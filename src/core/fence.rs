//! External fence renderers (DESIGN D6, seam 2).
//!
//! Config maps a fence language token to a shell command (`renderers.d2 =
//! "d2 - -"`); a comrak AST pass finds fenced code blocks whose language has a
//! configured renderer, runs the command with the fence body on stdin, and
//! replaces the block with the command's stdout (SVG/HTML) — the same
//! parse → mutate → format shape `diagram.rs` uses for mermaid, but driven by
//! user config instead of a built-in engine. This is the first thing in the
//! core that spawns a subprocess; the no-network rule is unaffected
//! (subprocesses are local I/O, and the page's CSP still blocks all egress).
//!
//! The exec boundary is a small, injectable seam: [`transform_fences`] takes the
//! renderer table plus a `run` closure, so tests drive the transform with a fake
//! while production passes [`run_command`] (the real `sh -c` + stdin runner).
//!
//! Robustness (all failure modes degrade, never crash): a spawn error, a
//! non-zero exit, a hard 5 s timeout (the child is killed), output over a 4 MiB
//! cap, or empty / non-UTF-8 output all fall back to the fence shown as a
//! highlighted code block plus a small styled error note — mirroring
//! `diagram.rs`. Unlike `math.rs`, no `catch_unwind` is needed: subprocess
//! outcomes are `Result`-shaped, so there is no panic to contain.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use comrak::nodes::{AstNode, NodeHtmlBlock, NodeValue};

use super::highlight::{escape_html, highlight_block};

/// The configured renderer table: fence language token → shell command. Keys are
/// normalised to lowercase at the config boundary (see `core::config`), and the
/// fence's language token is lowercased before lookup, so matching is
/// case-insensitive.
pub type Renderers = BTreeMap<String, String>;

/// Hard wall-clock timeout for a single renderer invocation. On expiry the child
/// is killed and the fence degrades. Documented in DESIGN D6.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on a renderer's stdout. Output beyond this is treated as a failure rather
/// than embedded (a runaway command must not bloat the page). 4 MiB comfortably
/// fits any reasonable SVG/HTML diagram.
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// The outcome of running one renderer command.
pub enum RunOutcome {
    /// The command produced this stdout (non-empty, valid UTF-8).
    Output(String),
    /// The command could not be used; carries a human-readable reason (spawn
    /// error, non-zero exit, timeout, over-cap, empty / non-UTF-8 output).
    Failed(String),
}

/// The injectable exec seam: given a `command` and the fence `body`, produce a
/// [`RunOutcome`]. Production passes [`run_command`]; tests pass a fake. Written
/// as a bare `&dyn Fn` (not a type alias) so the trait object borrows for the
/// call's lifetime — a test fake may capture non-`'static` state.
type Run<'r> = dyn Fn(&str, &str) -> RunOutcome + 'r;

/// AST pass: replace each fenced code block whose language has a configured
/// renderer with the command's output (wrapped in a scroll box), or a degraded
/// error block on failure. Blocks without a configured renderer are left
/// untouched for the later passes (built-in mermaid, then syntect highlight).
///
/// Running before `diagram::transform_mermaid` is what lets a configured
/// `mermaid` renderer override the built-in one: a consumed fence is no longer a
/// `CodeBlock` when the mermaid pass runs.
pub fn transform_fences<'a>(root: &'a AstNode<'a>, renderers: &Renderers, run: &Run<'_>) {
    if renderers.is_empty() {
        return;
    }
    for node in root.descendants() {
        let job = {
            let data = node.data.borrow();
            match &data.value {
                NodeValue::CodeBlock(block) if block.fenced => fence_language(&block.info)
                    .and_then(|lang| renderers.get_key_value(&lang))
                    .map(|(lang, cmd)| (lang.clone(), cmd.clone(), block.literal.clone())),
                _ => None,
            }
        };
        if let Some((lang, command, source)) = job {
            let html = match run(&command, &source) {
                RunOutcome::Output(out) => wrap_output(&out),
                RunOutcome::Failed(reason) => degrade(&lang, &source, &reason),
            };
            node.data.borrow_mut().value = NodeValue::HtmlBlock(NodeHtmlBlock {
                block_type: 0,
                literal: html,
            });
        }
    }
}

/// The fence info string's first token, lowercased, e.g. `d2` from `d2 foo=bar`.
/// `None` for a bare (language-less) fence, which no renderer can match.
fn fence_language(info: &str) -> Option<String> {
    info.split_whitespace()
        .next()
        .map(|t| t.to_ascii_lowercase())
}

/// Wrap trusted renderer output in a `.rendered-fence` scroll box so a wide SVG
/// scrolls inside its own container rather than pushing the page horizontally —
/// the same no-page-h-scroll invariant `.mermaid`/`.table-wrap`/`.math-scroll`
/// honour. Unlike `.mermaid`, no intrinsic-width parsing is done: the output is
/// arbitrary (SVG *or* HTML), so a plain scroll box is the honest primitive.
fn wrap_output(out: &str) -> String {
    format!("<div class=\"rendered-fence\">{out}</div>")
}

/// Graceful degradation: an error note above the original fence, rendered as a
/// highlighted code block so the source is never lost. Mirrors `diagram.rs`,
/// reusing the shared `.diagram-error` styling.
fn degrade(lang: &str, source: &str, reason: &str) -> String {
    format!(
        "<div class=\"diagram-error\">\
           <p class=\"diagram-error__note\">\u{26a0} {} renderer failed: {}</p>\
           {}\
         </div>",
        escape_html(lang),
        escape_html(reason),
        highlight_block(lang, source),
    )
}

/// The production runner: run `command` via `sh -c` with `body` on stdin and
/// capture stdout. Enforces the timeout and output cap; kills the child on
/// timeout. Every failure is returned as [`RunOutcome::Failed`] — never a panic.
///
/// The fence body is written on a helper thread (so a command that floods stdout
/// before draining stdin cannot deadlock us), and stdout is read on another
/// thread whose result the main thread awaits with a timeout.
pub fn run_command(command: &str, body: &str) -> RunOutcome {
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return RunOutcome::Failed(format!("could not spawn `sh -c`: {e}")),
    };

    // Feed the fence body on stdin from a helper thread, then drop stdin to
    // signal EOF. Errors are ignored: a command that never reads stdin (and so
    // closes its read end) makes the write fail with EPIPE, which is expected.
    let writer = child.stdin.take().map(|mut stdin| {
        let body = body.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(body.as_bytes());
        })
    });

    // Read stdout (capped at MAX_OUTPUT_BYTES + 1 so an over-cap flood is
    // detectable) on a helper thread; the main thread enforces the timeout.
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let res = stdout
            .by_ref()
            .take(MAX_OUTPUT_BYTES as u64 + 1)
            .read_to_end(&mut buf);
        let _ = tx.send((res, buf));
    });

    let outcome = match rx.recv_timeout(TIMEOUT) {
        // stdout closed within the budget: the command is finishing. Reap it and
        // check the exit status (a well-behaved command exits promptly once its
        // stdout is at EOF).
        Ok((Ok(_), buf)) => match child.wait() {
            Ok(status) if status.success() => finish(buf),
            Ok(status) => RunOutcome::Failed(match status.code() {
                Some(code) => format!("command exited with status {code}"),
                None => "command terminated by signal".to_string(),
            }),
            Err(e) => RunOutcome::Failed(format!("waiting on command failed: {e}")),
        },
        Ok((Err(e), _)) => {
            let _ = child.kill();
            let _ = child.wait();
            RunOutcome::Failed(format!("reading command output failed: {e}"))
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            RunOutcome::Failed(format!("command timed out after {}s", TIMEOUT.as_secs()))
        }
    };
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    outcome
}

/// Validate captured stdout: reject an over-cap flood, non-UTF-8 bytes, or
/// empty/whitespace-only output; otherwise succeed with the string.
fn finish(buf: Vec<u8>) -> RunOutcome {
    if buf.len() > MAX_OUTPUT_BYTES {
        return RunOutcome::Failed(format!("output exceeded the {MAX_OUTPUT_BYTES}-byte cap"));
    }
    match String::from_utf8(buf) {
        Ok(s) if !s.trim().is_empty() => RunOutcome::Output(s),
        Ok(_) => RunOutcome::Failed("command produced no output".to_string()),
        Err(_) => RunOutcome::Failed("command output was not valid UTF-8".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use comrak::{Arena, Options, format_html, parse_document};

    use super::*;

    /// Render `md` through the fence transform with the given renderer table and
    /// runner, returning the formatted HTML. Mirrors the pipeline's parse →
    /// transform → format shape without pulling in the whole pipeline.
    fn transform(md: &str, renderers: &Renderers, run: &Run<'_>) -> String {
        // `render.unsafe` must be on for the emitted raw-HTML blocks (the
        // renderer output and the degraded error block) to pass through
        // verbatim, matching what `pipeline` sets.
        let mut opts = Options::default();
        opts.render.r#unsafe = true;
        let arena = Arena::new();
        let root = parse_document(&arena, md, &opts);
        transform_fences(root, renderers, run);
        let mut out = String::new();
        format_html(root, &opts, &mut out).unwrap();
        out
    }

    fn renderers(pairs: &[(&str, &str)]) -> Renderers {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // --- the transform (fake runner) --------------------------------------

    #[test]
    fn configured_fence_is_replaced_by_command_output() {
        let table = renderers(&[("d2", "unused")]);
        // Fake runner echoes a recognisable SVG regardless of input.
        let run = |_cmd: &str, body: &str| {
            RunOutcome::Output(format!("<svg data-body=\"{}\"></svg>", body.trim()))
        };
        let html = transform("```d2\nx -> y\n```\n", &table, &run);
        assert!(html.contains("<div class=\"rendered-fence\">"));
        assert!(html.contains("<svg data-body=\"x -> y\">"));
        // The literal fence text must not survive as a code block.
        assert!(!html.contains("<code"));
    }

    #[test]
    fn unconfigured_fence_is_untouched() {
        let table = renderers(&[("d2", "unused")]);
        let run = |_: &str, _: &str| RunOutcome::Output("<svg></svg>".to_string());
        // A `rust` fence has no renderer: it stays a code block for the later
        // highlight pass (here, comrak's default `<pre><code>` formatting).
        let html = transform("```rust\nfn main() {}\n```\n", &table, &run);
        assert!(html.contains("<code"));
        assert!(html.contains("fn main"));
        assert!(!html.contains("rendered-fence"));
    }

    #[test]
    fn language_match_is_case_insensitive() {
        let table = renderers(&[("d2", "unused")]);
        let run = |_: &str, _: &str| RunOutcome::Output("<svg></svg>".to_string());
        let html = transform("```D2\nx\n```\n", &table, &run);
        assert!(html.contains("rendered-fence"));
    }

    #[test]
    fn empty_table_is_a_no_op() {
        let run = |_: &str, _: &str| RunOutcome::Output("<svg></svg>".to_string());
        let html = transform("```d2\nx\n```\n", &Renderers::new(), &run);
        assert!(!html.contains("rendered-fence"));
        assert!(html.contains("<code"));
    }

    #[test]
    fn failure_degrades_to_highlighted_code_plus_note() {
        let table = renderers(&[("d2", "unused")]);
        let run = |_: &str, _: &str| RunOutcome::Failed("boom".to_string());
        let html = transform("```d2\nx -> y\n```\n", &table, &run);
        assert!(html.contains("diagram-error__note"));
        assert!(html.contains("d2 renderer failed: boom"));
        // Source preserved as a highlighted code block; no output wrapper.
        assert!(html.contains("<pre class=\"code\">"));
        assert!(html.contains("x -&gt; y"));
        assert!(!html.contains("rendered-fence"));
    }

    #[test]
    fn command_receives_the_fence_body_on_stdin() {
        let table = renderers(&[("echo", "unused")]);
        let seen = RefCell::new(String::new());
        let run = |_cmd: &str, body: &str| {
            *seen.borrow_mut() = body.to_string();
            RunOutcome::Output("<svg></svg>".to_string())
        };
        transform("```echo\nhello world\n```\n", &table, &run);
        assert_eq!(seen.into_inner(), "hello world\n");
    }

    // --- the real runner (deterministic commands) -------------------------

    fn output(command: &str, body: &str) -> String {
        match run_command(command, body) {
            RunOutcome::Output(s) => s,
            RunOutcome::Failed(reason) => panic!("expected output, got failure: {reason}"),
        }
    }

    fn failure(command: &str, body: &str) -> String {
        match run_command(command, body) {
            RunOutcome::Output(s) => panic!("expected failure, got output: {s}"),
            RunOutcome::Failed(reason) => reason,
        }
    }

    #[test]
    fn cat_echoes_stdin() {
        assert_eq!(output("cat", "<svg>hi</svg>"), "<svg>hi</svg>");
    }

    #[test]
    fn command_ignoring_stdin_still_runs() {
        assert_eq!(output("printf '<svg/>'", "ignored"), "<svg/>");
    }

    #[test]
    fn nonzero_exit_is_a_failure() {
        let reason = failure("false", "");
        assert!(reason.contains("status"), "{reason}");
    }

    #[test]
    fn empty_output_is_a_failure() {
        let reason = failure("true", "");
        assert!(reason.contains("no output"), "{reason}");
    }

    #[test]
    fn spawn_error_never_panics() {
        // `sh -c` of a non-existent program: sh exits non-zero (not a spawn
        // error of sh itself), so this is a clean failure, not a panic.
        let reason = failure("this-command-does-not-exist-xyz", "");
        assert!(!reason.is_empty());
    }

    #[test]
    fn timeout_kills_a_slow_command() {
        // `sleep 10` never closes stdout within the 5 s budget; it must be killed
        // and reported as a timeout well before 10 s elapse.
        let start = std::time::Instant::now();
        let reason = failure("sleep 10", "");
        assert!(reason.contains("timed out"), "{reason}");
        assert!(
            start.elapsed() < Duration::from_secs(9),
            "timeout should fire near {}s, took {:?}",
            TIMEOUT.as_secs(),
            start.elapsed()
        );
    }

    #[test]
    fn over_cap_output_is_a_failure() {
        // Emit more than the cap; `yes` streams unbounded, `head -c` bounds it.
        let bytes = MAX_OUTPUT_BYTES + 1024;
        let reason = failure(&format!("yes x | head -c {bytes}"), "");
        assert!(reason.contains("cap"), "{reason}");
    }

    #[test]
    fn non_utf8_output_is_a_failure() {
        let reason = failure("printf '\\377\\376'", "");
        assert!(reason.contains("UTF-8"), "{reason}");
    }
}
