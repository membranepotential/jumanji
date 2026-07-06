//! Where the document comes from: a file on disk or standard input.
//!
//! Pure and GTK-free. This module only *classifies* the CLI input (file vs
//! stdin) and derives the small facts the shell needs from that choice (the
//! statusbar label, whether history/watching apply). The actual I/O — reading
//! the file, the stdin reader thread — lives in the shell.

use std::path::{Path, PathBuf};

/// The resolved document source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A markdown file on disk.
    File(PathBuf),
    /// Markdown streamed from standard input (`jumanji -`, or a bare pipe with
    /// no file argument).
    Stdin,
}

impl Source {
    /// Classify the optional CLI file argument into a [`Source`].
    ///
    /// - an explicit `-` always selects stdin;
    /// - any other path is a file;
    /// - **no** file argument selects stdin *only* when stdin is not a terminal
    ///   (i.e. something is piped in) — a bare `jumanji` at an interactive
    ///   prompt has nothing to read, so this yields `None`.
    ///
    /// `stdin_is_terminal` is injected by the caller (the isatty read is shell
    /// I/O) so the matrix stays pure and unit-testable.
    pub fn resolve(file: Option<&Path>, stdin_is_terminal: bool) -> Option<Source> {
        match file {
            Some(p) if p.as_os_str() == "-" => Some(Source::Stdin),
            Some(p) => Some(Source::File(p.to_path_buf())),
            None if !stdin_is_terminal => Some(Source::Stdin),
            None => None,
        }
    }

    /// Whether this source is standard input.
    pub fn is_stdin(&self) -> bool {
        matches!(self, Source::Stdin)
    }

    /// The name shown in the statusbar's left field: the file's basename, or the
    /// literal `stdin` for piped input.
    pub fn display_name(&self) -> String {
        match self {
            Source::File(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned()),
            Source::Stdin => "stdin".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dash_is_stdin_regardless_of_tty() {
        assert_eq!(
            Source::resolve(Some(Path::new("-")), true),
            Some(Source::Stdin)
        );
        assert_eq!(
            Source::resolve(Some(Path::new("-")), false),
            Some(Source::Stdin)
        );
    }

    #[test]
    fn a_path_is_a_file() {
        assert_eq!(
            Source::resolve(Some(Path::new("notes.md")), false),
            Some(Source::File(PathBuf::from("notes.md")))
        );
    }

    #[test]
    fn no_arg_with_pipe_is_stdin() {
        // Piped stdin (not a terminal) + no file argument = read the pipe.
        assert_eq!(Source::resolve(None, false), Some(Source::Stdin));
    }

    #[test]
    fn no_arg_at_a_terminal_is_no_input() {
        // Interactive prompt, nothing piped, no file: there is nothing to read.
        assert_eq!(Source::resolve(None, true), None);
    }

    #[test]
    fn is_stdin_matches_variant() {
        assert!(Source::Stdin.is_stdin());
        assert!(!Source::File(PathBuf::from("x.md")).is_stdin());
    }

    #[test]
    fn display_name_is_basename_or_stdin() {
        assert_eq!(Source::Stdin.display_name(), "stdin");
        assert_eq!(
            Source::File(PathBuf::from("/home/u/a/b.md")).display_name(),
            "b.md"
        );
    }
}
