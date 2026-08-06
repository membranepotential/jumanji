//! The `:` command line: parsing and tab completion.
//!
//! Pure and GTK-free. [`parse`] turns a typed line (without the leading `:`)
//! into a typed [`Command`]; [`complete`] powers tab completion. Side effects
//! — opening files, applying `:set`, running actions — belong to the shell,
//! which matches on the returned [`Command`].
use std::ops::Range;

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

/// Marks the selected candidate. Every candidate reserves the column, so page
/// boundaries do not shift as the selection moves through them.
const SELECTED: char = '▸';

/// One statusbar line of tab-completion candidates: the page that holds
/// `index`, with the selection marked, prefixed by `[i/n]` and — when there is
/// more than one — the page counter.
///
/// Candidates are packed into pages from the start of the list, so repeated
/// `Tab` walks the pages in order and *every* candidate is eventually shown,
/// rather than the line always echoing the same first few.
pub fn completion_line(candidates: &[String], index: usize, max_cols: usize) -> String {
    if candidates.is_empty() {
        return String::new();
    }
    let n = candidates.len();
    // The header is written with the real numbers below; reserve its widest
    // possible form here so the packed page always fits beside it.
    let reserve = format!("[{n}/{n}] ({n}/{n}) ").chars().count();
    let pages = paginate(candidates, max_cols.saturating_sub(reserve).max(1));
    let page = pages.iter().position(|p| p.contains(&index)).unwrap_or(0);

    let shown: Vec<String> = pages[page]
        .clone()
        .map(|i| {
            let marker = if i == index { SELECTED } else { ' ' };
            format!("{marker}{}", candidates[i])
        })
        .collect();
    let counter = if pages.len() > 1 {
        format!("({}/{}) ", page + 1, pages.len())
    } else {
        String::new()
    };
    format!("[{}/{n}] {counter}{}", index + 1, shown.join(" "))
}

/// Pack candidate indices into pages of at most `budget` columns — always at
/// least one candidate per page, however narrow the window.
fn paginate(candidates: &[String], budget: usize) -> Vec<Range<usize>> {
    let mut pages = Vec::new();
    let mut start = 0;
    while start < candidates.len() {
        let mut end = start;
        let mut cols = 0;
        while end < candidates.len() {
            // Each candidate carries its marker column; a space joins them.
            let width = cols + candidates[end].chars().count() + if end == start { 1 } else { 2 };
            if end > start && width > budget {
                break;
            }
            cols = width;
            end += 1;
        }
        pages.push(start..end);
        start = end;
    }
    pages
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

    /// Twelve five-column candidates (`aaa00` … `aaa11`).
    fn many() -> Vec<String> {
        (0..12).map(|i| format!("aaa{i:02}")).collect()
    }

    #[test]
    fn completion_line_marks_the_selection_and_counts_candidates() {
        let line = completion_line(&many(), 2, 200);
        assert!(line.starts_with("[3/12] "), "{line}");
        assert!(line.contains("▸aaa02"), "{line}");
        // One page at this width, so no page counter.
        assert!(!line.contains("(1/"), "{line}");
    }

    #[test]
    fn completion_line_pages_and_follows_the_selection() {
        // Room for the header plus three 6-column entries (marker + 5) per page.
        let cands = many();
        let cols = format!("[{n}/{n}] ({n}/{n}) ", n = cands.len()).len() + 20;
        let first = completion_line(&cands, 0, cols);
        assert!(first.contains("(1/4) ▸aaa00  aaa01  aaa02"), "{first}");
        assert!(!first.contains("aaa03"), "{first}");
        // Tabbing past the page boundary turns the page instead of cutting off.
        let second = completion_line(&cands, 3, cols);
        assert!(second.contains("(2/4) ▸aaa03  aaa04  aaa05"), "{second}");
        assert!(second.starts_with("[4/12] "), "{second}");
    }

    #[test]
    fn completion_line_pages_cover_every_candidate() {
        let cands = many();
        let cols = 60;
        let mut seen: Vec<&String> = Vec::new();
        for (i, cand) in cands.iter().enumerate() {
            let line = completion_line(&cands, i, cols);
            assert!(line.chars().count() <= cols, "overflow at {i}: {line}");
            assert!(
                line.contains(&format!("▸{cand}")),
                "{cand} unreachable: {line}"
            );
            seen.push(cand);
        }
        assert_eq!(seen.len(), cands.len());
    }

    #[test]
    fn completion_line_shows_one_candidate_even_when_absurdly_narrow() {
        let line = completion_line(&many(), 5, 1);
        assert!(line.contains("▸aaa05"), "{line}");
        assert!(!line.contains("aaa04"), "{line}");
        assert_eq!(completion_line(&[], 0, 80), "");
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
