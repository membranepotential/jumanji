//! TOML configuration: typed options and remappable key tables.
//!
//! Pure and GTK-free. Path resolution is parameterized so tests never touch
//! the real filesystem. A missing file yields defaults; a malformed file
//! surfaces an error (the caller prints it to stderr) and still yields
//! defaults, so the reader always opens.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::keymap::{Key, KeyPress, KeySequence, Keymap};
use super::{Action, Direction, Mode};

/// Typed rendering/interaction options with zathura-style defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// Pixels scrolled per `j`/`k`/`h`/`l` (before count multiplication).
    pub scroll_step_px: u32,
    /// Multiplicative zoom step per `+`/`-`.
    pub zoom_step: f64,
    /// Rendered content column width, in pixels.
    pub page_width_px: u32,
    /// Whether dark-mode recoloring is on at startup.
    pub default_recolor: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scroll_step_px: 60,
            zoom_step: 0.1,
            page_width_px: 720,
            default_recolor: false,
        }
    }
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

        let defaults = Options::default();
        let options = Options {
            scroll_step_px: raw.scroll_step.unwrap_or(defaults.scroll_step_px),
            zoom_step: raw.zoom_step.unwrap_or(defaults.zoom_step),
            page_width_px: raw.page_width.unwrap_or(defaults.page_width_px),
            default_recolor: raw.default_recolor.unwrap_or(defaults.default_recolor),
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
        let action = parse_action(&action).map_err(|message| ConfigError::KeyBinding {
            mode: mode_name,
            key: key.clone(),
            message,
        })?;
        keymap.bind(mode, seq, action);
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
pub fn parse_action(s: &str) -> Result<Action, String> {
    let normalized = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
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
        "zoom reset" => ZoomReset,
        "search" | "search start" => SearchStart,
        "search next" => SearchNext,
        "search previous" | "search prev" => SearchPrevious,
        "recolor" => Recolor,
        "reload" => Reload,
        "toggle toc" | "toc" => ToggleToc,
        "command" | "command line" => CommandLine,
        "abort" => Abort,
        "quit" => Quit,
        other => return Err(format!("unknown action '{other}'")),
    })
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(rename = "scroll-step")]
    scroll_step: Option<u32>,
    #[serde(rename = "zoom-step")]
    zoom_step: Option<f64>,
    #[serde(rename = "page-width")]
    page_width: Option<u32>,
    #[serde(rename = "default-recolor")]
    default_recolor: Option<bool>,
    keys: Option<RawKeys>,
}

#[derive(Debug, Deserialize)]
struct RawKeys {
    normal: Option<BTreeMap<String, String>>,
    toc: Option<BTreeMap<String, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::keymap::{MatchResult, Matcher};

    #[test]
    fn empty_config_is_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.options, Options::default());
    }

    #[test]
    fn options_parse() {
        let c = Config::parse(
            r#"
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
        let c = Config::parse("scroll-step = 42").unwrap();
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
    fn missing_file_is_defaults() {
        let c = Config::load(Some(Path::new("/nonexistent/xyz"))).unwrap();
        assert_eq!(c.options, Options::default());
    }
}
