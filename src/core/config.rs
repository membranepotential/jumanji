//! TOML configuration: typed options and remappable key tables.
//!
//! Pure and GTK-free. Path resolution is parameterized so tests never touch
//! the real filesystem. A missing file yields defaults; a malformed file
//! surfaces an error (the caller prints it to stderr) and still yields
//! defaults, so the reader always opens.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::editor::EditorCommand;
use super::keymap::{CharArgKind, Key, KeyPress, KeySequence, Keymap};
use super::{Action, Direction, Mode};

/// Which system clipboard a text selection is copied to, zathura-style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionClipboard {
    /// The X11 PRIMARY selection (middle-click paste). Zathura's default.
    #[default]
    Primary,
    /// The CLIPBOARD selection (Ctrl-V paste).
    Clipboard,
}

impl SelectionClipboard {
    fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "primary" => Ok(Self::Primary),
            "clipboard" => Ok(Self::Clipboard),
            other => Err(format!(
                "expected \"primary\" or \"clipboard\", got {other:?}"
            )),
        }
    }
}

/// Typed rendering/interaction options with zathura-style defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// Pixels scrolled per `j`/`k`/`h`/`l` (before count multiplication).
    pub scroll_step_px: u32,
    /// Geometric zoom step (added to the webkit `zoom_level` per step).
    pub zoom_step: f64,
    /// Text zoom step: fraction of the base font size added per step.
    pub text_zoom_step: f64,
    /// Rendered content column width, in pixels.
    pub page_width_px: u32,
    /// Whether dark-mode recoloring is on at startup.
    pub default_recolor: bool,
    /// Body/prose font family (empty = the stylesheet's default serif stack).
    pub font_body: String,
    /// Monospace/code font family (empty = the stylesheet's default stack).
    pub font_mono: String,
    /// Base body font size in pixels; also the text-zoom 100% reference.
    pub font_size_px: u32,
    /// Which clipboard a selection is copied to on select.
    pub selection_clipboard: SelectionClipboard,
    /// Reverse editor sync (DESIGN D7): the command spawned on Ctrl+click, with
    /// `%l`/`%f` substituted for the source line and file. Config-only (parsed
    /// once at load; not a `:set` target). Default: `$EDITOR +%l %f`.
    pub editor_command: EditorCommand,
    /// External fence renderers (DESIGN D6.2): fence language token → shell
    /// command run via `sh -c` with the fence body on stdin, producing SVG/HTML
    /// on stdout. Keys are normalised to lowercase (matching is
    /// case-insensitive). Not a `:set` target — it is a table, wired at render
    /// time. Empty by default (built-in pipeline only).
    pub renderers: BTreeMap<String, String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scroll_step_px: 60,
            zoom_step: 0.1,
            text_zoom_step: 0.1,
            page_width_px: 720,
            default_recolor: false,
            font_body: String::new(),
            font_mono: String::new(),
            font_size_px: 18,
            selection_clipboard: SelectionClipboard::Primary,
            editor_command: EditorCommand::default(),
            renderers: BTreeMap::new(),
        }
    }
}

/// What the shell must do after a successful runtime `:set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetEffect {
    /// Re-run the render pipeline (the option feeds generated CSS/HTML).
    Rerender,
    /// Only re-apply the dark/light recolor state; no re-render needed.
    Recolor,
    /// Nothing to do now — picked up the next time the option is used.
    None,
}

impl Options {
    /// Apply a runtime `:set key value`. Returns the [`SetEffect`] the shell
    /// must honour, or `Err(msg)` for an unknown key, an unparseable value, or
    /// an option that cannot change at runtime.
    pub fn set(&mut self, key: &str, value: &str) -> Result<SetEffect, String> {
        let value = value.trim();
        match key.trim() {
            "page-width" => {
                self.page_width_px = parse_scalar::<u32>(value, "page-width")?;
                Ok(SetEffect::Rerender)
            }
            "font-size" => {
                self.font_size_px = parse_scalar::<u32>(value, "font-size")?;
                Ok(SetEffect::Rerender)
            }
            "font-body" => {
                self.font_body = unquote(value).to_string();
                Ok(SetEffect::Rerender)
            }
            "font-mono" => {
                self.font_mono = unquote(value).to_string();
                Ok(SetEffect::Rerender)
            }
            "default-recolor" => {
                self.default_recolor = parse_scalar::<bool>(value, "default-recolor")?;
                Ok(SetEffect::Recolor)
            }
            "scroll-step" => {
                self.scroll_step_px = parse_scalar::<u32>(value, "scroll-step")?;
                Ok(SetEffect::None)
            }
            "zoom-step" => {
                self.zoom_step = parse_scalar::<f64>(value, "zoom-step")?;
                Ok(SetEffect::None)
            }
            "text-zoom-step" => {
                self.text_zoom_step = parse_scalar::<f64>(value, "text-zoom-step")?;
                Ok(SetEffect::None)
            }
            "selection-clipboard" => {
                // Validate for a helpful message, but reject regardless: the
                // clipboard target is wired at view-construction time.
                SelectionClipboard::parse(value)
                    .map_err(|m| format!("selection-clipboard: {m}"))?;
                Err("selection-clipboard cannot be changed at runtime".to_string())
            }
            other => Err(format!("unknown option `{other}`")),
        }
    }
}

/// Parse a scalar option value with a typed error message. Reuses each type's
/// own `FromStr`, the same coercion serde performs on the TOML scalar.
fn parse_scalar<T>(value: &str, key: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse::<T>()
        .map_err(|e| format!("{key}: invalid value {value:?}: {e}"))
}

/// Strip a single matched pair of surrounding double quotes, so both
/// `:set font-body Inter` and `:set font-body "Fira Code"` behave.
fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

/// A fully-resolved configuration: options plus the effective keymap.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub options: Options,
    pub keymap: Keymap,
}

/// A configuration parse error, with enough context to point the user at the
/// offending line or key.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config syntax error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("[keys.{mode}] {key:?}: {message}")]
    KeyBinding {
        mode: &'static str,
        key: String,
        message: String,
    },
    #[error("option `{key}`: {message}")]
    OptionValue { key: &'static str, message: String },
}

impl Config {
    /// Resolve `<config_dir>/jumanji/config.toml`, read and parse it. A missing
    /// file is not an error (returns defaults); a malformed file returns the
    /// error so the caller can log it and fall back to defaults.
    pub fn load(config_dir: Option<&Path>) -> Result<Self, ConfigError> {
        let Some(path) = config_dir.map(|d| d.join("jumanji").join("config.toml")) else {
            return Ok(Self::default());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Ok(Self::default()),
        }
    }

    /// Parse config text into a [`Config`], overlaying user key bindings onto
    /// the defaults.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(text)?;
        let raw_opts = raw.options.unwrap_or_default();

        let defaults = Options::default();
        let selection_clipboard = match raw_opts.selection_clipboard {
            Some(s) => {
                SelectionClipboard::parse(&s).map_err(|message| ConfigError::OptionValue {
                    key: "selection-clipboard",
                    message,
                })?
            }
            None => defaults.selection_clipboard,
        };
        let editor_command = match raw_opts.editor_command {
            Some(s) => EditorCommand::parse(&s).map_err(|message| ConfigError::OptionValue {
                key: "editor-command",
                message,
            })?,
            None => defaults.editor_command,
        };
        let options = Options {
            scroll_step_px: raw_opts.scroll_step.unwrap_or(defaults.scroll_step_px),
            zoom_step: raw_opts.zoom_step.unwrap_or(defaults.zoom_step),
            text_zoom_step: raw_opts.text_zoom_step.unwrap_or(defaults.text_zoom_step),
            page_width_px: raw_opts.page_width.unwrap_or(defaults.page_width_px),
            default_recolor: raw_opts.default_recolor.unwrap_or(defaults.default_recolor),
            font_body: raw_opts.font_body.unwrap_or(defaults.font_body),
            font_mono: raw_opts.font_mono.unwrap_or(defaults.font_mono),
            font_size_px: raw_opts.font_size.unwrap_or(defaults.font_size_px),
            selection_clipboard,
            editor_command,
            // Normalise fence-language keys to lowercase so the lookup (which
            // lowercases the fence token) is case-insensitive.
            renderers: raw
                .renderers
                .unwrap_or_default()
                .into_iter()
                .map(|(lang, cmd)| (lang.to_ascii_lowercase(), cmd))
                .collect(),
        };

        let mut keymap = Keymap::default();
        if let Some(keys) = raw.keys {
            apply_key_table(&mut keymap, Mode::Normal, "normal", keys.normal)?;
            apply_key_table(&mut keymap, Mode::Toc, "toc", keys.toc)?;
        }

        Ok(Self { options, keymap })
    }
}

/// The XDG config base directory (`$XDG_CONFIG_HOME` or `$HOME/.config`).
pub fn xdg_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
}

fn apply_key_table(
    keymap: &mut Keymap,
    mode: Mode,
    mode_name: &'static str,
    table: Option<BTreeMap<String, String>>,
) -> Result<(), ConfigError> {
    let Some(table) = table else { return Ok(()) };
    for (key, action) in table {
        let seq = parse_key_sequence(&key).map_err(|message| ConfigError::KeyBinding {
            mode: mode_name,
            key: key.clone(),
            message,
        })?;
        let binding = parse_binding(&action).map_err(|message| ConfigError::KeyBinding {
            mode: mode_name,
            key: key.clone(),
            message,
        })?;
        match binding {
            ParsedBinding::Action(action) => keymap.bind(mode, seq, action),
            ParsedBinding::CharArg(kind) => keymap.bind_char_arg(mode, seq, kind),
        }
    }
    Ok(())
}

/// Parse a zathura-style key notation into a [`KeySequence`].
///
/// Bare characters are literal (`gg`, `J`); angle brackets denote specials and
/// modifiers (`<C-r>`, `<Tab>`, `<Esc>`, `<S-Tab>`, `<Space>`).
pub fn parse_key_sequence(s: &str) -> Result<KeySequence, String> {
    if s.is_empty() {
        return Err("empty key sequence".to_string());
    }
    let mut presses = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut token = String::new();
            let mut closed = false;
            for tc in chars.by_ref() {
                if tc == '>' {
                    closed = true;
                    break;
                }
                token.push(tc);
            }
            if !closed {
                return Err(format!("unclosed '<' in {s:?}"));
            }
            presses.push(parse_bracketed(&token)?);
        } else {
            presses.push(KeyPress::char(c));
        }
    }
    Ok(KeySequence(presses))
}

fn parse_bracketed(token: &str) -> Result<KeyPress, String> {
    // Split leading modifier prefixes like `C-`, `S-`, `A-`.
    let mut ctrl = false;
    let mut shift = false;
    let mut rest = token;
    loop {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b'-' {
            match bytes[0].to_ascii_uppercase() {
                b'C' => ctrl = true,
                b'S' => shift = true,
                other => {
                    return Err(format!(
                        "unknown modifier '{}-' in <{token}>",
                        other as char
                    ));
                }
            }
            rest = &rest[2..];
        } else {
            break;
        }
    }

    let key = parse_key_name(rest)?;
    // Fold `<S-x>` into an uppercase char so it matches literal `X`.
    if shift && let Key::Char(c) = key {
        return Ok(KeyPress::new(
            Key::Char(c.to_ascii_uppercase()),
            ctrl,
            false,
        ));
    }
    Ok(KeyPress::new(key, ctrl, shift))
}

fn parse_key_name(name: &str) -> Result<Key, String> {
    let mut ch = name.chars();
    if let (Some(c), None) = (ch.next(), ch.clone().next()) {
        // Single character.
        return Ok(Key::Char(c));
    }
    Ok(match name.to_ascii_lowercase().as_str() {
        "esc" | "escape" => Key::Escape,
        "tab" => Key::Tab,
        "cr" | "enter" | "return" => Key::Enter,
        "space" => Key::Space,
        "bs" | "backspace" => Key::Backspace,
        _ => return Err(format!("unknown key name '{name}'")),
    })
}

/// Parse an action string (`"section next"`, `"goto bottom"`, `"recolor"`)
/// into a typed [`Action`]. Case-insensitive; extra whitespace tolerated.
///
/// The one case-*sensitive* exception is a quickmark with an explicit register:
/// `"mark set <c>"` / `"mark jump <c>"` (used by the D-Bus `ExecuteAction`
/// path), where `<c>` is a single character kept verbatim (`ma` ≠ `mA`).
pub fn parse_action(s: &str) -> Result<Action, String> {
    // Quickmark-with-register: handled before lowercasing so the register keeps
    // its case. In key tables the char is omitted (a `CharArg` binding instead);
    // here, supplying it produces a concrete action for automation/testing.
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() >= 2 && tokens[0].eq_ignore_ascii_case("mark") {
        let kind = tokens[1].to_ascii_lowercase();
        if kind == "set" || kind == "jump" {
            let reg = tokens.get(2).copied().unwrap_or("");
            let mut chars = reg.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(if kind == "set" {
                    Action::QuickmarkSet(c)
                } else {
                    Action::QuickmarkJump(c)
                }),
                _ => Err(format!(
                    "`mark {kind}` needs a single-character register, e.g. `mark {kind} a`"
                )),
            };
        }
    }

    let normalized = tokens.join(" ").to_lowercase();
    use Action::*;
    use Direction::*;
    Ok(match normalized.as_str() {
        "scroll down" => Scroll(Down),
        "scroll up" => Scroll(Up),
        "scroll left" => Scroll(Left),
        "scroll right" => Scroll(Right),
        "half-page down" | "halfpage down" => HalfPage(Down),
        "half-page up" | "halfpage up" => HalfPage(Up),
        "section next" => SectionNext,
        "section previous" | "section prev" => SectionPrevious,
        "goto top" => GotoTop,
        "goto bottom" => GotoBottom,
        "zoom in" => ZoomIn,
        "zoom out" => ZoomOut,
        "text zoom in" => TextZoomIn,
        "text zoom out" => TextZoomOut,
        "zoom reset" => ZoomReset,
        "search" | "search start" => SearchStart,
        "search next" => SearchNext,
        "search previous" | "search prev" => SearchPrevious,
        "recolor" => Recolor,
        "reload" => Reload,
        "toggle toc" | "toc" => ToggleToc,
        "command" | "command line" => CommandLine,
        "follow link" => FollowLink,
        "show link target" => ShowLinkTarget,
        "jump backward" => JumpBackward,
        "jump forward" => JumpForward,
        "toc next" => TocNext,
        "toc previous" | "toc prev" => TocPrevious,
        "toc expand" => TocExpand,
        "toc collapse" => TocCollapse,
        "toc select" => TocSelect,
        "abort" => Abort,
        "quit" => Quit,
        other => return Err(format!("unknown action '{other}'")),
    })
}

/// A parsed key-table binding: either a fixed action, or a char-argument prefix
/// (`mark set` / `mark jump` with no register — the register is captured live).
pub(crate) enum ParsedBinding {
    Action(Action),
    CharArg(CharArgKind),
}

/// Parse a key-table action string into a [`ParsedBinding`]. Bare `mark set` /
/// `mark jump` (no register) become a [`CharArgKind`] prefix binding; anything
/// else defers to [`parse_action`].
fn parse_binding(s: &str) -> Result<ParsedBinding, String> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() == 2 && tokens[0].eq_ignore_ascii_case("mark") {
        match tokens[1].to_ascii_lowercase().as_str() {
            "set" => return Ok(ParsedBinding::CharArg(CharArgKind::QuickmarkSet)),
            "jump" => return Ok(ParsedBinding::CharArg(CharArgKind::QuickmarkJump)),
            _ => {}
        }
    }
    parse_action(s).map(ParsedBinding::Action)
}

/// The canonical option keys (`:set <key>` completion; also the TOML spelling).
pub fn option_keys() -> &'static [&'static str] {
    &[
        "scroll-step",
        "zoom-step",
        "text-zoom-step",
        "page-width",
        "default-recolor",
        "font-body",
        "font-mono",
        "font-size",
        "selection-clipboard",
    ]
}

/// The canonical action strings, one per action, for command-line completion
/// (`:` exec names). Char-argument quickmarks are offered by their bare prefix.
pub fn action_names() -> &'static [&'static str] {
    &[
        "scroll down",
        "scroll up",
        "scroll left",
        "scroll right",
        "half-page down",
        "half-page up",
        "section next",
        "section previous",
        "goto top",
        "goto bottom",
        "zoom in",
        "zoom out",
        "text zoom in",
        "text zoom out",
        "zoom reset",
        "search",
        "search next",
        "search previous",
        "recolor",
        "reload",
        "toggle toc",
        "follow link",
        "show link target",
        "mark set",
        "mark jump",
        "jump backward",
        "jump forward",
        "toc next",
        "toc previous",
        "toc expand",
        "toc collapse",
        "toc select",
        "abort",
    ]
}

/// The file's top level: an `[options]` table and `[keys.<mode>]` tables, as
/// documented in the README. `deny_unknown_fields` everywhere: a misplaced or
/// misspelled key must error loudly (surfaced non-fatally by the caller), not
/// be silently ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    options: Option<RawOptions>,
    keys: Option<RawKeys>,
    /// `[renderers]` table: fence language token → shell command string. A free
    /// map (any language key is valid), so no `deny_unknown_fields` here.
    renderers: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOptions {
    #[serde(rename = "scroll-step")]
    scroll_step: Option<u32>,
    #[serde(rename = "zoom-step")]
    zoom_step: Option<f64>,
    #[serde(rename = "text-zoom-step")]
    text_zoom_step: Option<f64>,
    #[serde(rename = "page-width")]
    page_width: Option<u32>,
    #[serde(rename = "default-recolor")]
    default_recolor: Option<bool>,
    #[serde(rename = "font-body")]
    font_body: Option<String>,
    #[serde(rename = "font-mono")]
    font_mono: Option<String>,
    #[serde(rename = "font-size")]
    font_size: Option<u32>,
    #[serde(rename = "selection-clipboard")]
    selection_clipboard: Option<String>,
    #[serde(rename = "editor-command")]
    editor_command: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeys {
    normal: Option<BTreeMap<String, String>>,
    toc: Option<BTreeMap<String, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::keymap::{MatchResult, Matcher};

    #[test]
    fn unknown_or_misplaced_keys_error_loudly() {
        // Top-level option keys (the pre-[options] format) must error, not be
        // silently ignored — regression for default-recolor being swallowed.
        assert!(Config::parse("default-recolor = true").is_err());
        assert!(Config::parse("[options]\ntypo-key = 1").is_err());
    }

    #[test]
    fn empty_config_is_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.options, Options::default());
    }

    #[test]
    fn options_parse() {
        let c = Config::parse(
            r#"
            [options]
            scroll-step = 120
            zoom-step = 0.25
            page-width = 900
            default-recolor = true
            "#,
        )
        .unwrap();
        assert_eq!(c.options.scroll_step_px, 120);
        assert_eq!(c.options.zoom_step, 0.25);
        assert_eq!(c.options.page_width_px, 900);
        assert!(c.options.default_recolor);
    }

    #[test]
    fn partial_options_keep_defaults() {
        let c = Config::parse("[options]\nscroll-step = 42").unwrap();
        assert_eq!(c.options.scroll_step_px, 42);
        assert_eq!(c.options.zoom_step, Options::default().zoom_step);
    }

    #[test]
    fn parse_key_sequence_variants() {
        assert_eq!(
            parse_key_sequence("gg").unwrap(),
            KeySequence(vec![KeyPress::char('g'), KeyPress::char('g')])
        );
        assert_eq!(
            parse_key_sequence("<C-r>").unwrap(),
            KeySequence::single(KeyPress::new(Key::Char('r'), true, false))
        );
        assert_eq!(
            parse_key_sequence("<Tab>").unwrap(),
            KeySequence::single(KeyPress::new(Key::Tab, false, false))
        );
        assert_eq!(
            parse_key_sequence("<S-j>").unwrap(),
            KeySequence::single(KeyPress::char('J'))
        );
    }

    #[test]
    fn parse_key_sequence_errors() {
        assert!(parse_key_sequence("").is_err());
        assert!(parse_key_sequence("<C-r").is_err());
        assert!(parse_key_sequence("<Bogus>").is_err());
    }

    #[test]
    fn remap_override_applies() {
        let c = Config::parse(
            r#"
            [keys.normal]
            j = "quit"
            "#,
        )
        .unwrap();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(
            m.feed(KeyPress::char('j'), &c.keymap),
            MatchResult::Matched {
                action: Action::Quit,
                count: None
            }
        );
    }

    #[test]
    fn remap_new_sequence() {
        let c = Config::parse(
            r#"
            [keys.normal]
            "gg" = "goto bottom"
            "<C-r>" = "reload"
            "#,
        )
        .unwrap();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(m.feed(KeyPress::char('g'), &c.keymap), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('g'), &c.keymap),
            MatchResult::Matched {
                action: Action::GotoBottom,
                count: None
            }
        );
    }

    #[test]
    fn bad_action_string_errors() {
        let err = Config::parse(
            r#"
            [keys.normal]
            x = "explode"
            "#,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("normal"), "{msg}");
        assert!(msg.contains('x'), "{msg}");
        assert!(msg.contains("explode"), "{msg}");
    }

    #[test]
    fn bad_key_notation_errors() {
        let err = Config::parse(
            r#"
            [keys.normal]
            "<Nope>" = "quit"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Nope"));
    }

    #[test]
    fn action_parsing_is_lenient() {
        assert_eq!(parse_action("Section  Next").unwrap(), Action::SectionNext);
        assert_eq!(parse_action("goto top").unwrap(), Action::GotoTop);
    }

    #[test]
    fn zoom_actions_cover_both_axes() {
        assert_eq!(parse_action("zoom in").unwrap(), Action::ZoomIn);
        assert_eq!(parse_action("zoom out").unwrap(), Action::ZoomOut);
        assert_eq!(parse_action("text zoom in").unwrap(), Action::TextZoomIn);
        assert_eq!(parse_action("text zoom out").unwrap(), Action::TextZoomOut);
        assert_eq!(parse_action("zoom reset").unwrap(), Action::ZoomReset);
    }

    #[test]
    fn plus_minus_default_to_geometric_zoom() {
        let km = Keymap::default();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(
            m.feed(KeyPress::char('+'), &km),
            MatchResult::Matched {
                action: Action::ZoomIn,
                count: None
            }
        );
        assert_eq!(
            m.feed(KeyPress::char('-'), &km),
            MatchResult::Matched {
                action: Action::ZoomOut,
                count: None
            }
        );
    }

    #[test]
    fn font_and_zoom_options_parse() {
        let c = Config::parse(
            r#"
            [options]
            font-body = "Inter"
            font-mono = "Fira Code"
            font-size = 20
            text-zoom-step = 0.2
            "#,
        )
        .unwrap();
        assert_eq!(c.options.font_body, "Inter");
        assert_eq!(c.options.font_mono, "Fira Code");
        assert_eq!(c.options.font_size_px, 20);
        assert_eq!(c.options.text_zoom_step, 0.2);
        // Untouched fields keep their defaults.
        assert_eq!(c.options.font_size_px, 20);
    }

    #[test]
    fn selection_clipboard_parses_and_defaults() {
        assert_eq!(
            Config::parse("").unwrap().options.selection_clipboard,
            SelectionClipboard::Primary
        );
        assert_eq!(
            Config::parse("[options]\nselection-clipboard = \"clipboard\"")
                .unwrap()
                .options
                .selection_clipboard,
            SelectionClipboard::Clipboard
        );
        assert_eq!(
            Config::parse("[options]\nselection-clipboard = \"PRIMARY\"")
                .unwrap()
                .options
                .selection_clipboard,
            SelectionClipboard::Primary
        );
    }

    #[test]
    fn bad_selection_clipboard_errors() {
        let err = Config::parse("selection-clipboard = \"middle\"").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("selection-clipboard"), "{msg}");
        assert!(msg.contains("middle"), "{msg}");
    }

    #[test]
    fn renderers_table_parses_and_lowercases_keys() {
        let c = Config::parse(
            r#"
            [renderers]
            d2 = "d2 - -"
            Graphviz = "dot -Tsvg"
            "#,
        )
        .unwrap();
        assert_eq!(c.options.renderers.get("d2").unwrap(), "d2 - -");
        // Key is normalised to lowercase for case-insensitive matching.
        assert_eq!(c.options.renderers.get("graphviz").unwrap(), "dot -Tsvg");
        assert!(!c.options.renderers.contains_key("Graphviz"));
    }

    #[test]
    fn no_renderers_table_is_empty() {
        assert!(Config::parse("").unwrap().options.renderers.is_empty());
    }

    #[test]
    fn editor_command_defaults_and_parses() {
        use std::path::Path;
        // Default is the `$EDITOR +%l %f` template.
        let default = Config::parse("").unwrap().options.editor_command;
        assert_eq!(
            default.to_argv(12, Path::new("/a/b.md")),
            vec!["$EDITOR", "+12", "/a/b.md"]
        );
        // A configured template is parsed and applied.
        let c = Config::parse("[options]\neditor-command = \"code -g %f:%l\"").unwrap();
        assert_eq!(
            c.options.editor_command.to_argv(3, Path::new("n.md")),
            vec!["code", "-g", "n.md:3"]
        );
    }

    #[test]
    fn empty_editor_command_errors() {
        let err = Config::parse("[options]\neditor-command = \"\"").unwrap_err();
        assert!(err.to_string().contains("editor-command"), "{err}");
    }

    #[test]
    fn missing_file_is_defaults() {
        let c = Config::load(Some(Path::new("/nonexistent/xyz"))).unwrap();
        assert_eq!(c.options, Options::default());
    }

    #[test]
    fn m2_action_strings_parse() {
        assert_eq!(parse_action("follow link").unwrap(), Action::FollowLink);
        assert_eq!(
            parse_action("show link target").unwrap(),
            Action::ShowLinkTarget
        );
        assert_eq!(parse_action("jump backward").unwrap(), Action::JumpBackward);
        assert_eq!(parse_action("jump forward").unwrap(), Action::JumpForward);
        assert_eq!(parse_action("toc next").unwrap(), Action::TocNext);
        assert_eq!(parse_action("toc previous").unwrap(), Action::TocPrevious);
        assert_eq!(parse_action("toc expand").unwrap(), Action::TocExpand);
        assert_eq!(parse_action("toc collapse").unwrap(), Action::TocCollapse);
        assert_eq!(parse_action("toc select").unwrap(), Action::TocSelect);
    }

    #[test]
    fn mark_action_string_with_register_is_case_sensitive() {
        assert_eq!(
            parse_action("mark set a").unwrap(),
            Action::QuickmarkSet('a')
        );
        assert_eq!(
            parse_action("mark set A").unwrap(),
            Action::QuickmarkSet('A')
        );
        assert_eq!(
            parse_action("mark jump z").unwrap(),
            Action::QuickmarkJump('z')
        );
        // The `mark` keyword itself is case-insensitive.
        assert_eq!(
            parse_action("Mark Set q").unwrap(),
            Action::QuickmarkSet('q')
        );
    }

    #[test]
    fn mark_action_string_requires_single_char_register() {
        assert!(parse_action("mark set").is_err());
        assert!(parse_action("mark set ab").is_err());
        assert!(parse_action("mark jump").is_err());
    }

    #[test]
    fn key_table_mark_without_register_is_char_arg_binding() {
        let c = Config::parse(
            r#"
            [keys.normal]
            "g" = "mark set"
            "b" = "mark jump"
            "#,
        )
        .unwrap();
        let mut m = Matcher::new(Mode::Normal);
        // `g` then `x` sets quickmark x.
        assert_eq!(m.feed(KeyPress::char('g'), &c.keymap), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('x'), &c.keymap),
            MatchResult::Matched {
                action: Action::QuickmarkSet('x'),
                count: None
            }
        );
        assert_eq!(m.feed(KeyPress::char('b'), &c.keymap), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('y'), &c.keymap),
            MatchResult::Matched {
                action: Action::QuickmarkJump('y'),
                count: None
            }
        );
    }

    #[test]
    fn toc_key_table_remaps() {
        let c = Config::parse(
            r#"
            [keys.toc]
            "n" = "toc next"
            "#,
        )
        .unwrap();
        let mut m = Matcher::new(Mode::Toc);
        assert_eq!(
            m.feed(KeyPress::char('n'), &c.keymap),
            MatchResult::Matched {
                action: Action::TocNext,
                count: None
            }
        );
    }

    #[test]
    fn runtime_set_rerender_options() {
        let mut o = Options::default();
        assert_eq!(o.set("page-width", "900").unwrap(), SetEffect::Rerender);
        assert_eq!(o.page_width_px, 900);
        assert_eq!(o.set("font-size", "22").unwrap(), SetEffect::Rerender);
        assert_eq!(o.font_size_px, 22);
        assert_eq!(
            o.set("font-body", "Fira Sans").unwrap(),
            SetEffect::Rerender
        );
        assert_eq!(o.font_body, "Fira Sans");
        // Quoted string value is unwrapped.
        assert_eq!(
            o.set("font-mono", "\"JetBrains Mono\"").unwrap(),
            SetEffect::Rerender
        );
        assert_eq!(o.font_mono, "JetBrains Mono");
    }

    #[test]
    fn runtime_set_recolor_and_none_effects() {
        let mut o = Options::default();
        assert_eq!(
            o.set("default-recolor", "true").unwrap(),
            SetEffect::Recolor
        );
        assert!(o.default_recolor);
        assert_eq!(o.set("scroll-step", "80").unwrap(), SetEffect::None);
        assert_eq!(o.scroll_step_px, 80);
        assert_eq!(o.set("zoom-step", "0.25").unwrap(), SetEffect::None);
        assert_eq!(o.zoom_step, 0.25);
        assert_eq!(o.set("text-zoom-step", "0.2").unwrap(), SetEffect::None);
    }

    #[test]
    fn runtime_set_rejects_bad_and_immutable() {
        let mut o = Options::default();
        assert!(o.set("page-width", "wide").is_err());
        assert!(o.set("zoom-step", "lots").is_err());
        assert!(o.set("no-such-option", "1").is_err());
        // selection-clipboard cannot change at runtime, even with a valid value.
        assert!(o.set("selection-clipboard", "clipboard").is_err());
        assert_eq!(o.selection_clipboard, SelectionClipboard::Primary);
    }

    #[test]
    fn option_keys_cover_the_options_surface() {
        // Every advertised option key must be a real `:set` target.
        let mut o = Options::default();
        for key in option_keys() {
            let r = o.set(key, "1");
            // Known key: either applied, or a deliberate runtime rejection —
            // never the "unknown option" error.
            if let Err(msg) = r {
                assert!(!msg.contains("unknown option"), "{key}: {msg}");
            }
        }
    }
}
