//! The `:` command line: parsing and tab completion.
//!
//! Pure and GTK-free. [`parse`] turns a typed line (without the leading `:`)
//! into a typed [`Command`]; [`complete`] powers tab completion. Side effects
//! — opening files, applying `:set`, running actions — belong to the shell,
//! which matches on the returned [`Command`].
//!
// Consumed by the shell's `:`-command handling (parallel M2 shell-integration
// work); the pure API and its tests land first, hence the allow.
#![allow(dead_code)]

use super::Action;
use super::config::{self, action_names, option_keys};

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `:open <path>` — path resolution (globbing, `~`, relative) is shell-side.
    Open(String),
    /// `:set <option> <value>` — applied via [`super::config::Options::set`].
    Set(String, String),
    /// Any [`config::parse_action`] string, e.g. `:zoom in`, `:reload`.
    Exec(Action),
    /// `:q` / `:quit`.
    Quit,
}

/// Parse a command line (already stripped of its leading `:`).
pub fn parse(input: &str) -> Result<Command, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty command".to_string());
    }
    let (cmd, rest) = split_first_word(trimmed);
    match cmd {
        "q" | "quit" => {
            if rest.trim().is_empty() {
                Ok(Command::Quit)
            } else {
                Err(format!("`{cmd}` takes no arguments"))
            }
        }
        "open" | "o" => {
            let path = rest.trim();
            if path.is_empty() {
                Err("open: missing path".to_string())
            } else {
                Ok(Command::Open(path.to_string()))
            }
        }
        "set" | "se" => {
            let (option, value) = split_first_word(rest.trim());
            if option.is_empty() {
                return Err("set: missing option".to_string());
            }
            let value = value.trim();
            if value.is_empty() {
                return Err(format!("set: `{option}` needs a value"));
            }
            Ok(Command::Set(option.to_string(), value.to_string()))
        }
        // Anything else is an action-exec string (`:reload`, `:zoom in`, …).
        _ => config::parse_action(trimmed).map(Command::Exec),
    }
}

/// Completion outcome for the current input (without the leading `:`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completions {
    /// Full replacement lines, e.g. `["set scroll-step", "set selection-…"]`.
    Candidates(Vec<String>),
    /// The argument is a filesystem path; the shell completes `prefix` against
    /// the filesystem (this core stays I/O-free).
    Path { prefix: String },
}

/// Compute completions for a partial command line (without the leading `:`).
pub fn complete(input: &str) -> Completions {
    let s = input.trim_start();
    match s.find(' ') {
        // Still typing the command word: complete command/action names.
        None => Completions::Candidates(
            command_names()
                .filter(|name| name.starts_with(s))
                .map(str::to_string)
                .collect(),
        ),
        // A command word plus (partial) arguments.
        Some(idx) => {
            let cmd = &s[..idx];
            let rest = &s[idx + 1..];
            match cmd {
                "open" | "o" => Completions::Path {
                    prefix: rest.trim_start().to_string(),
                },
                "set" | "se" => {
                    let partial = rest.trim_start();
                    // Once the option word is finished (another space), we have
                    // no value candidates to offer.
                    if partial.contains(' ') {
                        Completions::Candidates(Vec::new())
                    } else {
                        Completions::Candidates(
                            option_keys()
                                .iter()
                                .filter(|k| k.starts_with(partial))
                                .map(|k| format!("set {k}"))
                                .collect(),
                        )
                    }
                }
                _ => Completions::Candidates(Vec::new()),
            }
        }
    }
}

/// The full set of first-word completions: the built-in commands plus every
/// action-exec name.
fn command_names() -> impl Iterator<Item = &'static str> {
    ["open", "set", "quit"]
        .into_iter()
        .chain(action_names().iter().copied())
}

/// Split off the first whitespace-delimited word, returning `(word, rest)`
/// where `rest` retains its interior spacing.
fn split_first_word(s: &str) -> (&str, &str) {
    match s.find(char::is_whitespace) {
        Some(idx) => (&s[..idx], &s[idx + 1..]),
        None => (s, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Direction;

    #[test]
    fn parse_quit_variants() {
        assert_eq!(parse("q").unwrap(), Command::Quit);
        assert_eq!(parse("quit").unwrap(), Command::Quit);
        assert_eq!(parse("  quit  ").unwrap(), Command::Quit);
        assert!(parse("q now").is_err());
    }

    #[test]
    fn parse_open_path() {
        assert_eq!(
            parse("open ~/notes.md").unwrap(),
            Command::Open("~/notes.md".to_string())
        );
        assert_eq!(
            parse("o /tmp/a.md").unwrap(),
            Command::Open("/tmp/a.md".to_string())
        );
        // Paths with spaces survive intact (rest is not re-split).
        assert_eq!(
            parse("open /tmp/my notes.md").unwrap(),
            Command::Open("/tmp/my notes.md".to_string())
        );
        assert!(parse("open").is_err());
        assert!(parse("open    ").is_err());
    }

    #[test]
    fn parse_set_option_value() {
        assert_eq!(
            parse("set scroll-step 80").unwrap(),
            Command::Set("scroll-step".to_string(), "80".to_string())
        );
        // The value keeps interior spacing (font families).
        assert_eq!(
            parse("set font-body Fira Sans").unwrap(),
            Command::Set("font-body".to_string(), "Fira Sans".to_string())
        );
        assert!(parse("set").is_err());
        assert!(parse("set page-width").is_err());
    }

    #[test]
    fn parse_action_exec_fallthrough() {
        assert_eq!(parse("zoom in").unwrap(), Command::Exec(Action::ZoomIn));
        assert_eq!(parse("reload").unwrap(), Command::Exec(Action::Reload));
        assert_eq!(
            parse("scroll down").unwrap(),
            Command::Exec(Action::Scroll(Direction::Down))
        );
        // A quickmark with an explicit register works through exec.
        assert_eq!(
            parse("mark set a").unwrap(),
            Command::Exec(Action::QuickmarkSet('a'))
        );
    }

    #[test]
    fn parse_unknown_command_errors() {
        assert!(parse("frobnicate").is_err());
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn complete_command_names_by_prefix() {
        let Completions::Candidates(c) = complete("op") else {
            panic!("expected candidates");
        };
        assert!(c.contains(&"open".to_string()));

        let Completions::Candidates(c) = complete("zoom") else {
            panic!("expected candidates");
        };
        assert!(c.contains(&"zoom in".to_string()));
        assert!(c.contains(&"zoom out".to_string()));
        assert!(c.contains(&"zoom reset".to_string()));
    }

    #[test]
    fn complete_empty_offers_everything() {
        let Completions::Candidates(c) = complete("") else {
            panic!("expected candidates");
        };
        assert!(c.contains(&"open".to_string()));
        assert!(c.contains(&"set".to_string()));
        assert!(c.contains(&"quit".to_string()));
        assert!(c.contains(&"reload".to_string()));
    }

    #[test]
    fn complete_set_offers_option_keys() {
        let Completions::Candidates(c) = complete("set ") else {
            panic!("expected candidates");
        };
        assert!(c.contains(&"set scroll-step".to_string()));
        assert!(c.contains(&"set page-width".to_string()));

        let Completions::Candidates(c) = complete("set font") else {
            panic!("expected candidates");
        };
        assert!(c.contains(&"set font-body".to_string()));
        assert!(c.contains(&"set font-mono".to_string()));
        assert!(c.contains(&"set font-size".to_string()));
        assert!(!c.contains(&"set scroll-step".to_string()));
    }

    #[test]
    fn complete_set_value_has_no_candidates() {
        // Past the option word, we don't guess values.
        assert_eq!(
            complete("set scroll-step 8"),
            Completions::Candidates(Vec::new())
        );
    }

    #[test]
    fn complete_open_yields_path_prefix() {
        assert_eq!(
            complete("open ~/doc"),
            Completions::Path {
                prefix: "~/doc".to_string()
            }
        );
        assert_eq!(
            complete("open "),
            Completions::Path {
                prefix: String::new()
            }
        );
    }

    #[test]
    fn every_completed_command_name_parses() {
        // Completion must never offer a name that `parse` then rejects.
        let Completions::Candidates(names) = complete("") else {
            panic!("expected candidates");
        };
        for name in names {
            // Some completions are prefixes that legitimately need an argument
            // before they parse: `open <path>`, `set <opt> <val>`, and the
            // `mark set`/`mark jump` register prefixes.
            if matches!(name.as_str(), "open" | "set") || name.starts_with("mark ") {
                continue;
            }
            assert!(parse(&name).is_ok(), "`{name}` completed but did not parse");
        }
    }
}
