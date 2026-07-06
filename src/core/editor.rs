//! Reverse editor sync (DESIGN D7): the `editor-command` template.
//!
//! zathura's `synctex-editor-command` analogue. A Ctrl+click on a rendered
//! element resolves the source line under it and shells out to the configured
//! editor so the cursor jumps there. The template is a small, fully-typed value:
//! the raw string is split into whitespace-separated argv tokens once at parse
//! time, and each token is a sequence of literal text and `%l`/`%f` placeholders
//! (zathura's synctex placeholder style). Substituting a `(line, file)` yields
//! the concrete argv to spawn.
//!
//! Pure and GTK-free: parsing and substitution live here (unit-tested); the
//! shell owns the `$VAR` expansion and the detached spawn.

use std::path::Path;

/// The built-in default: open `$EDITOR` at the clicked line. `$EDITOR` is a
/// literal token the shell expands from the environment before spawning.
pub const DEFAULT_EDITOR_COMMAND: &str = "$EDITOR +%l %f";

/// One segment of an argv token in an editor-command template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// Verbatim text.
    Literal(String),
    /// `%l` — the 1-based source line under the clicked element.
    Line,
    /// `%f` — the document's file path.
    File,
}

/// A parsed `editor-command` template. Substituting is argv-based, not a shell
/// invocation, so a file path containing spaces stays a single argument (it is
/// substituted into one `%f` token, never re-split).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCommand {
    /// argv template; `args[0]` is the program to run.
    args: Vec<Vec<Segment>>,
}

impl EditorCommand {
    /// Parse a template. `%l`/`%f` are placeholders and `%%` is a literal `%`;
    /// any other `%x` is kept verbatim (a literal `%` followed by `x`), matching
    /// zathura's lenient substitution. Errors only on an empty (whitespace-only)
    /// template — there would be no program to run.
    pub fn parse(template: &str) -> Result<Self, String> {
        let args: Vec<Vec<Segment>> = template.split_whitespace().map(parse_token).collect();
        if args.is_empty() {
            return Err("editor-command is empty".to_string());
        }
        Ok(Self { args })
    }

    /// Render the concrete argv for a Ctrl+click on `line` in `file`. `args[0]`
    /// is the program; the shell expands a leading `$VAR` and spawns it detached.
    pub fn to_argv(&self, line: u32, file: &Path) -> Vec<String> {
        let file = file.to_string_lossy();
        self.args
            .iter()
            .map(|segments| {
                let mut out = String::new();
                for seg in segments {
                    match seg {
                        Segment::Literal(s) => out.push_str(s),
                        Segment::Line => out.push_str(&line.to_string()),
                        Segment::File => out.push_str(&file),
                    }
                }
                out
            })
            .collect()
    }
}

impl Default for EditorCommand {
    fn default() -> Self {
        // The default template is a compile-time constant known to parse.
        Self::parse(DEFAULT_EDITOR_COMMAND).expect("default editor-command parses")
    }
}

/// Split one whitespace-delimited token into its literal/placeholder segments.
fn parse_token(token: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('l') => {
                    flush(&mut segments, &mut literal);
                    segments.push(Segment::Line);
                }
                Some('f') => {
                    flush(&mut segments, &mut literal);
                    segments.push(Segment::File);
                }
                Some('%') => literal.push('%'),
                Some(other) => {
                    literal.push('%');
                    literal.push(other);
                }
                None => literal.push('%'),
            }
        } else {
            literal.push(c);
        }
    }
    flush(&mut segments, &mut literal);
    segments
}

/// Push any accumulated literal text as a segment and clear the buffer.
fn flush(segments: &mut Vec<Segment>, literal: &mut String) {
    if !literal.is_empty() {
        segments.push(Segment::Literal(std::mem::take(literal)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn default_template_substitutes_line_and_file() {
        let cmd = EditorCommand::default();
        let argv = cmd.to_argv(120, Path::new("/home/u/notes.md"));
        assert_eq!(argv, vec!["$EDITOR", "+120", "/home/u/notes.md"]);
    }

    #[test]
    fn placeholders_inside_a_token_substitute_in_place() {
        // `+%l` is one token: literal `+` then the line, yielding `+42`.
        let cmd = EditorCommand::parse("nvim +%l %f").unwrap();
        assert_eq!(
            cmd.to_argv(42, Path::new("/x/y.md")),
            vec!["nvim", "+42", "/x/y.md"]
        );
    }

    #[test]
    fn file_with_spaces_stays_one_argument() {
        // argv-based substitution: a path with spaces is a single `%f` token, so
        // it is never split into multiple arguments.
        let cmd = EditorCommand::parse("code -g %f:%l").unwrap();
        assert_eq!(
            cmd.to_argv(7, Path::new("/tmp/my notes/a b.md")),
            vec!["code", "-g", "/tmp/my notes/a b.md:7"]
        );
    }

    #[test]
    fn percent_percent_is_a_literal_percent() {
        let cmd = EditorCommand::parse("e 100%%done %f").unwrap();
        assert_eq!(
            cmd.to_argv(3, Path::new("f.md")),
            vec!["e", "100%done", "f.md"]
        );
    }

    #[test]
    fn unknown_placeholder_is_kept_verbatim() {
        let cmd = EditorCommand::parse("e %c %f").unwrap();
        assert_eq!(cmd.to_argv(3, Path::new("f.md")), vec!["e", "%c", "f.md"]);
    }

    #[test]
    fn trailing_percent_is_literal() {
        let cmd = EditorCommand::parse("e 50% %f").unwrap();
        assert_eq!(cmd.to_argv(3, Path::new("f.md")), vec!["e", "50%", "f.md"]);
    }

    #[test]
    fn empty_template_is_an_error() {
        assert!(EditorCommand::parse("").is_err());
        assert!(EditorCommand::parse("   ").is_err());
    }

    #[test]
    fn extra_whitespace_is_collapsed() {
        let cmd = EditorCommand::parse("  nvim   +%l    %f  ").unwrap();
        assert_eq!(
            cmd.to_argv(9, Path::new("z.md")),
            vec!["nvim", "+9", "z.md"]
        );
    }
}
