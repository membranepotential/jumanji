//! TOML configuration: typed options and remappable key tables.
//!
//! Pure and GTK-free. Path resolution is parameterized so tests never touch
//! the real filesystem. A missing file yields defaults; a malformed file
//! surfaces an error (the caller prints it to stderr) and still yields
//! defaults, so the reader always opens.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::editor::EditorCommand;
use super::graph::View as GraphView;
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

/// An sRGB colour with alpha, for colour options. Parsed from the forms
/// zathura's config takes — `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb(r, g, b)`,
/// `rgba(r, g, b, a)` — so a zathurarc value copies over. Emitted as CSS
/// `rgba(…)`; being validated, it cannot break out of the generated rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    r: u8,
    g: u8,
    b: u8,
    /// Opacity in `0.0..=1.0`.
    a: f64,
}

impl Rgba {
    /// zathura's default `highlight-color`, the yellow-green of its search hits.
    pub const ZATHURA_HIGHLIGHT: Self = Self {
        r: 159,
        g: 251,
        b: 0,
        a: 0.5,
    };

    /// zathura's default `highlight-active-color`, the green of its current
    /// search hit.
    pub const ZATHURA_HIGHLIGHT_ACTIVE: Self = Self {
        r: 0,
        g: 188,
        b: 0,
        a: 0.5,
    };

    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let invalid = || {
            format!(
                "expected #rgb, #rrggbb, #rrggbbaa, rgb(r, g, b) or rgba(r, g, b, a), got {s:?}"
            )
        };
        if let Some(hex) = s.strip_prefix('#') {
            let digits = |range: std::ops::Range<usize>, width: usize| {
                let v = u8::from_str_radix(hex.get(range).ok_or_else(invalid)?, 16)
                    .map_err(|_| invalid())?;
                // `#abc` is `#aabbcc`: a single digit repeats.
                Ok::<u8, String>(if width == 1 { v * 17 } else { v })
            };
            if !hex.is_ascii() {
                return Err(invalid());
            }
            return match hex.len() {
                3 => Ok(Self {
                    r: digits(0..1, 1)?,
                    g: digits(1..2, 1)?,
                    b: digits(2..3, 1)?,
                    a: 1.0,
                }),
                6 | 8 => Ok(Self {
                    r: digits(0..2, 2)?,
                    g: digits(2..4, 2)?,
                    b: digits(4..6, 2)?,
                    a: if hex.len() == 8 {
                        f64::from(digits(6..8, 2)?) / 255.0
                    } else {
                        1.0
                    },
                }),
                _ => Err(invalid()),
            };
        }
        let lower = s.to_ascii_lowercase();
        let (args, has_alpha) = if let Some(rest) = lower.strip_prefix("rgba(") {
            (rest, true)
        } else if let Some(rest) = lower.strip_prefix("rgb(") {
            (rest, false)
        } else {
            return Err(invalid());
        };
        let parts: Vec<&str> = args
            .strip_suffix(')')
            .ok_or_else(invalid)?
            .split(',')
            .map(str::trim)
            .collect();
        let channel = |p: &str| p.parse::<u8>().map_err(|_| invalid());
        match (parts.as_slice(), has_alpha) {
            ([r, g, b], false) => Ok(Self {
                r: channel(r)?,
                g: channel(g)?,
                b: channel(b)?,
                a: 1.0,
            }),
            ([r, g, b, a], true) => {
                let a = a.parse::<f64>().map_err(|_| invalid())?;
                if !(0.0..=1.0).contains(&a) {
                    return Err(invalid());
                }
                Ok(Self {
                    r: channel(r)?,
                    g: channel(g)?,
                    b: channel(b)?,
                    a,
                })
            }
            _ => Err(invalid()),
        }
    }
}

impl fmt::Display for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rgba({}, {}, {}, {})", self.r, self.g, self.b, self.a)
    }
}

/// The `<html>` class that gates every wide-block breakout: the master switch
/// [`Action::ToggleWide`](crate::core::Action::ToggleWide) flips at runtime,
/// exactly the contract `html.dark` has with the recolor. Each eligible kind
/// also carries `{WIDE_CLASS}-<kind>`; the stylesheet ANDs the two, so the
/// per-kind classes say *what may* break out and this one says *whether it
/// does*. The pipeline emits both onto `<html>`; the controller only ever
/// touches this one.
pub const WIDE_CLASS: &str = "jmnj-wide";

/// The `<html>` class that puts every diagram in **fit-to-width** mode: the
/// stylesheet caps `.mermaid svg` at `min(100%, var(--dw))`, so a diagram wider
/// than its box is scaled down to show all of it, and one already narrower is
/// left at its intrinsic size rather than being blown up (DESIGN D5a.2).
///
/// Document-wide, and the same two-part contract [`WIDE_CLASS`] has: the
/// pipeline emits it when the `diagram-fit` option is on, and
/// [`Action::ToggleDiagramFit`](crate::core::Action::ToggleDiagramFit) flips it
/// at runtime as a class flip — no re-render.
pub const DIAGRAM_FIT_CLASS: &str = "jmnj-diagram-fit";

/// The per-diagram Ctrl+wheel scale, as a CSS custom property on one `.mermaid`
/// box: the stylesheet multiplies the intrinsic width `--dw` by it, so `1` (the
/// default) is exactly today's geometry.
///
/// Transient DOM state, not session state — a look-closer gesture does not
/// survive a reload — so unlike `--dw` the pipeline never emits it; the
/// controller's zoom script is the only writer. The name lives here because
/// `assets/style.css` is the reader.
pub const DIAGRAM_ZOOM_VAR: &str = "--dz";

/// The class a `.mermaid` **box** carries while its [`DIAGRAM_ZOOM_VAR`] scale
/// is off 1: it bounds the box's height and gives it scrollers on both axes, so
/// a diagram zoomed to 400% scrolls *inside its box* instead of making the
/// document four times taller or pushing the page sideways. At scale 1 the
/// class is absent and the box keeps exactly its unzoomed geometry.
pub const DIAGRAM_ZOOM_CLASS: &str = "jmnj-diagram-zoomed";

/// The CSS highlight (the Custom Highlight API's `::highlight(<name>)`) that
/// paints every `/` match but the current one, in `--highlight`. The
/// controller's search script registers it; `assets/style.css` styles it.
pub const SEARCH_HIGHLIGHT: &str = "jmnj-find";

/// The CSS highlight that paints the current `/` match, in
/// `--highlight-active`. The current match is never in [`SEARCH_HIGHLIGHT`]
/// too, so the two colours never blend.
pub const SEARCH_ACTIVE_HIGHLIGHT: &str = "jmnj-find-active";

/// One kind of block that may break out of the reading column to window width.
///
/// Bounded measure is right for prose and wrong for pictures: a 1800 px diagram
/// scrolled through a 912 px porthole wastes the window it is being read in
/// (DESIGN D5a). These are the block kinds that carry their own `overflow-x`
/// scroller today, i.e. the ones for which "wider" means "more of it visible".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WideBlock {
    /// Mermaid diagrams (`.mermaid`).
    Diagrams,
    /// External fence-renderer output (`.rendered-fence`, DESIGN D6.2).
    Fences,
    /// GFM tables (`.table-wrap`).
    Tables,
    /// Highlighted code blocks (`pre.code`).
    Code,
    /// Display math (`.math-scroll`).
    Math,
}

impl WideBlock {
    /// Every kind, in the canonical order the config spelling and [`Display`]
    /// use.
    ///
    /// [`Display`]: std::fmt::Display
    pub const ALL: [WideBlock; 5] = [
        WideBlock::Diagrams,
        WideBlock::Fences,
        WideBlock::Tables,
        WideBlock::Code,
        WideBlock::Math,
    ];

    /// The kind's canonical name: its config spelling, and — suffixed onto
    /// [`WIDE_CLASS`] — its `<html>` class. One place, so the Rust and the
    /// stylesheet cannot drift.
    pub const fn name(self) -> &'static str {
        match self {
            WideBlock::Diagrams => "diagrams",
            WideBlock::Fences => "fences",
            WideBlock::Tables => "tables",
            WideBlock::Code => "code",
            WideBlock::Math => "math",
        }
    }

    /// The `<html>` class the pipeline emits when this kind is eligible.
    pub fn css_class(self) -> String {
        format!("{WIDE_CLASS}-{}", self.name())
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.name().eq_ignore_ascii_case(name))
    }
}

/// Which block kinds are eligible to break out of the reading column.
///
/// A set, not a flag per kind: the config value is one comma-separated list
/// (`none`, `all`, or `diagrams,tables`), and [`Display`] round-trips it, so
/// the `:set` echo and the file spelling agree by construction.
///
/// [`Display`]: std::fmt::Display
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideBlocks(BTreeSet<WideBlock>);

impl Default for WideBlocks {
    /// The shipped default, `diagrams,fences,tables`: the kinds that are
    /// pictures or grids. Code and math stay in the column, where a reader
    /// wants them next to the prose that explains them.
    fn default() -> Self {
        Self(
            [WideBlock::Diagrams, WideBlock::Fences, WideBlock::Tables]
                .into_iter()
                .collect(),
        )
    }
}

impl WideBlocks {
    /// No kind breaks out (`none`).
    pub fn none() -> Self {
        Self(BTreeSet::new())
    }

    /// Every kind breaks out (`all`).
    pub fn all() -> Self {
        Self(WideBlock::ALL.into_iter().collect())
    }

    /// Parse the config value: `none`, `all`, or a comma-separated list of kind
    /// names. Case-insensitive and whitespace-tolerant (a trailing comma is
    /// fine); an unknown name is an error that names the offending token.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("none") {
            return Ok(Self::none());
        }
        if s.eq_ignore_ascii_case("all") {
            return Ok(Self::all());
        }
        let mut kinds = BTreeSet::new();
        for token in s.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            let kind = WideBlock::parse(token).ok_or_else(|| {
                let valid: Vec<&str> = WideBlock::ALL.iter().map(|k| k.name()).collect();
                format!(
                    "unknown block kind {token:?}: expected \"none\", \"all\", or a \
                     comma-separated list of {}",
                    valid.join(", ")
                )
            })?;
            kinds.insert(kind);
        }
        Ok(Self(kinds))
    }

    /// The kinds in this set, in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = WideBlock> + '_ {
        self.0.iter().copied()
    }

    /// The `<html>` classes the pipeline emits for this set, in canonical
    /// order. The master [`WIDE_CLASS`] is not among them — it is the runtime
    /// state, not the eligibility.
    pub fn classes(&self) -> impl Iterator<Item = String> + '_ {
        self.iter().map(WideBlock::css_class)
    }
}

impl fmt::Display for WideBlocks {
    /// The canonical spelling, which [`WideBlocks::parse`] round-trips: `none`
    /// for the empty set, `all` for the full one, else the kind names in
    /// canonical order.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        if self.0.len() == WideBlock::ALL.len() {
            return f.write_str("all");
        }
        let names: Vec<&str> = self.iter().map(|k| k.name()).collect();
        f.write_str(&names.join(","))
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
    /// Whether a document's YAML frontmatter is shown. Off by default (DESIGN
    /// D11): a note should open as prose, not as a block of metadata.
    pub show_frontmatter: bool,
    /// Whether the process detaches from the terminal at startup, so the shell
    /// prompt returns immediately instead of the reader holding the foreground
    /// job (`jumanji notes.md` behaving like `jumanji notes.md &`). Off by
    /// default: blocking is what a command is expected to do, and the terminal
    /// is where startup diagnostics land. Config-only and consumed once in
    /// `main` before the window exists — not a `:set` target.
    pub background: bool,
    /// Body/prose font family (empty = the stylesheet's default serif stack).
    pub font_body: String,
    /// Monospace/code font family (empty = the stylesheet's default stack).
    pub font_mono: String,
    /// Base body font size in pixels; also the text-zoom 100% reference.
    pub font_size_px: u32,
    /// Which block kinds may break out of the reading column to window width
    /// when the master switch is on (DESIGN D5a). Default:
    /// `diagrams,fences,tables`.
    pub wide_blocks: WideBlocks,
    /// The breakout master switch's initial state — what `s`
    /// ([`Action::ToggleWide`](crate::core::Action::ToggleWide)) starts out
    /// flipping. On by default: a diagram scrolled through a porthole while the
    /// window sits empty is the behaviour worth defaulting away from.
    pub wide: bool,
    /// Whether diagrams open in fit-to-width mode — scaled down to their box
    /// instead of rendered at their intrinsic width (DESIGN D5a.2). What `a`
    /// ([`Action::ToggleDiagramFit`](crate::core::Action::ToggleDiagramFit))
    /// starts out flipping. **Off** by default: intrinsic is D5a's decided
    /// behaviour (a shrunk-to-fit diagram is unreadably small), and the
    /// wide-block breakout already recovers most of the missing width.
    pub diagram_fit: bool,
    /// What the document graph shows around its spine (DESIGN D14): the
    /// current note's links (`links`, the default) or the whole spanning tree
    /// (`tree`). What `v` starts out flipping; `:set` applies it to an open
    /// graph.
    pub graph_view: GraphView,
    /// The colour of a text selection and of every `/` match. zathura's
    /// `highlight-color` and default.
    pub highlight_color: Rgba,
    /// The colour of the current `/` match, the one `n`/`N` step from.
    /// zathura's `highlight-active-color` and default.
    pub highlight_active_color: Rgba,
    /// Whether a finished pointer selection is copied without a keypress.
    /// Off by default. Ctrl-C (WebKit's own copy, to CLIPBOARD) and WebKit's
    /// own claim of PRIMARY work either way.
    pub copy_on_select: bool,
    /// Which clipboard copy-on-select writes to.
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
            page_width_px: 960,
            default_recolor: false,
            show_frontmatter: false,
            background: false,
            font_body: String::new(),
            font_mono: String::new(),
            font_size_px: 18,
            wide_blocks: WideBlocks::default(),
            wide: true,
            diagram_fit: false,
            graph_view: GraphView::Links,
            highlight_color: Rgba::ZATHURA_HIGHLIGHT,
            highlight_active_color: Rgba::ZATHURA_HIGHLIGHT_ACTIVE,
            copy_on_select: false,
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
    /// Re-lay out the document graph, if it is open.
    Relayout,
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
            "show-frontmatter" => {
                self.show_frontmatter = parse_scalar::<bool>(value, "show-frontmatter")?;
                Ok(SetEffect::Rerender)
            }
            "wide-blocks" => {
                // Re-render rather than a class flip: the pipeline emits the
                // per-kind classes, so a changed *set* is only honest after the
                // document carries the new ones. The reading position survives a
                // re-render (as `toggle frontmatter` relies on too).
                self.wide_blocks =
                    WideBlocks::parse(unquote(value)).map_err(|m| format!("wide-blocks: {m}"))?;
                Ok(SetEffect::Rerender)
            }
            "wide" => {
                self.wide = parse_scalar::<bool>(value, "wide")?;
                Ok(SetEffect::Rerender)
            }
            "diagram-fit" => {
                // Re-render for the same reason `wide` does: the pipeline emits
                // the class, so the next render must agree with the live state.
                // The *toggle* (`a`) is still a class flip with no re-render.
                self.diagram_fit = parse_scalar::<bool>(value, "diagram-fit")?;
                Ok(SetEffect::Rerender)
            }
            "graph-view" => {
                self.graph_view =
                    GraphView::parse(value).map_err(|m| format!("graph-view: {m}"))?;
                Ok(SetEffect::Relayout)
            }
            "highlight-color" => {
                self.highlight_color =
                    Rgba::parse(unquote(value)).map_err(|m| format!("highlight-color: {m}"))?;
                Ok(SetEffect::Rerender)
            }
            "highlight-active-color" => {
                self.highlight_active_color = Rgba::parse(unquote(value))
                    .map_err(|m| format!("highlight-active-color: {m}"))?;
                Ok(SetEffect::Rerender)
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
            "copy-on-select" => {
                self.copy_on_select = parse_scalar::<bool>(value, "copy-on-select")?;
                Ok(SetEffect::None)
            }
            "selection-clipboard" => {
                // Validate for a helpful message, but reject regardless: the
                // selection target is a launch-time choice, not a live one.
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
        let wide_blocks = match raw_opts.wide_blocks {
            Some(s) => WideBlocks::parse(&s).map_err(|message| ConfigError::OptionValue {
                key: "wide-blocks",
                message,
            })?,
            None => defaults.wide_blocks.clone(),
        };
        let graph_view = match raw_opts.graph_view {
            Some(s) => GraphView::parse(&s).map_err(|message| ConfigError::OptionValue {
                key: "graph-view",
                message,
            })?,
            None => defaults.graph_view,
        };
        let highlight_color = match raw_opts.highlight_color {
            Some(s) => Rgba::parse(&s).map_err(|message| ConfigError::OptionValue {
                key: "highlight-color",
                message,
            })?,
            None => defaults.highlight_color,
        };
        let highlight_active_color = match raw_opts.highlight_active_color {
            Some(s) => Rgba::parse(&s).map_err(|message| ConfigError::OptionValue {
                key: "highlight-active-color",
                message,
            })?,
            None => defaults.highlight_active_color,
        };
        let options = Options {
            scroll_step_px: raw_opts.scroll_step.unwrap_or(defaults.scroll_step_px),
            zoom_step: raw_opts.zoom_step.unwrap_or(defaults.zoom_step),
            text_zoom_step: raw_opts.text_zoom_step.unwrap_or(defaults.text_zoom_step),
            page_width_px: raw_opts.page_width.unwrap_or(defaults.page_width_px),
            default_recolor: raw_opts.default_recolor.unwrap_or(defaults.default_recolor),
            show_frontmatter: raw_opts
                .show_frontmatter
                .unwrap_or(defaults.show_frontmatter),
            background: raw_opts.background.unwrap_or(defaults.background),
            font_body: raw_opts.font_body.unwrap_or(defaults.font_body),
            font_mono: raw_opts.font_mono.unwrap_or(defaults.font_mono),
            font_size_px: raw_opts.font_size.unwrap_or(defaults.font_size_px),
            wide_blocks,
            wide: raw_opts.wide.unwrap_or(defaults.wide),
            diagram_fit: raw_opts.diagram_fit.unwrap_or(defaults.diagram_fit),
            graph_view,
            highlight_color,
            highlight_active_color,
            copy_on_select: raw_opts.copy_on_select.unwrap_or(defaults.copy_on_select),
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
            apply_key_table(&mut keymap, Mode::Graph, "graph", keys.graph)?;
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
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
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
        "toggle frontmatter" | "frontmatter" => ToggleFrontmatter,
        "toggle wide" | "wide" => ToggleWide,
        "toggle diagram fit" | "diagram fit" => ToggleDiagramFit,
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
        "toggle graph" | "graph" => ToggleGraph,
        "graph next" => GraphNext,
        "graph previous" | "graph prev" => GraphPrevious,
        "graph parent" => GraphParent,
        "graph child" => GraphChild,
        "graph fold" => GraphFold,
        "graph open" => GraphOpen,
        "graph view" => GraphToggleView,
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
        "show-frontmatter",
        "font-body",
        "font-mono",
        "font-size",
        "wide-blocks",
        "wide",
        "diagram-fit",
        "graph-view",
        "highlight-color",
        "highlight-active-color",
        "copy-on-select",
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
        "toggle frontmatter",
        "toggle wide",
        "toggle diagram fit",
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
        "toggle graph",
        "graph next",
        "graph previous",
        "graph parent",
        "graph child",
        "graph fold",
        "graph open",
        "graph view",
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
    #[serde(rename = "show-frontmatter")]
    show_frontmatter: Option<bool>,
    background: Option<bool>,
    #[serde(rename = "font-body")]
    font_body: Option<String>,
    #[serde(rename = "font-mono")]
    font_mono: Option<String>,
    #[serde(rename = "font-size")]
    font_size: Option<u32>,
    #[serde(rename = "wide-blocks")]
    wide_blocks: Option<String>,
    wide: Option<bool>,
    #[serde(rename = "diagram-fit")]
    diagram_fit: Option<bool>,
    #[serde(rename = "graph-view")]
    graph_view: Option<String>,
    #[serde(rename = "highlight-color")]
    highlight_color: Option<String>,
    #[serde(rename = "highlight-active-color")]
    highlight_active_color: Option<String>,
    #[serde(rename = "copy-on-select")]
    copy_on_select: Option<bool>,
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
    graph: Option<BTreeMap<String, String>>,
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
            parse_key_sequence("<Down><up>").unwrap(),
            KeySequence(vec![
                KeyPress::new(Key::Down, false, false),
                KeyPress::new(Key::Up, false, false),
            ])
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
    fn copy_on_select_is_off_by_default_and_settable() {
        assert!(!Config::parse("").unwrap().options.copy_on_select);
        assert!(
            Config::parse("[options]\ncopy-on-select = true")
                .unwrap()
                .options
                .copy_on_select
        );
        let mut o = Options::default();
        assert_eq!(o.set("copy-on-select", "true").unwrap(), SetEffect::None);
        assert!(o.copy_on_select);
        assert!(o.set("copy-on-select", "yes").is_err());
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
    fn graph_key_table_remaps_and_t_opens_the_graph_by_default() {
        let c = Config::parse(
            r#"
            [keys.graph]
            "o" = "graph open"
            "#,
        )
        .unwrap();
        let mut m = Matcher::new(Mode::Graph);
        assert_eq!(
            m.feed(KeyPress::char('o'), &c.keymap),
            MatchResult::Matched {
                action: Action::GraphOpen,
                count: None
            }
        );
        let mut n = Matcher::new(Mode::Normal);
        assert_eq!(
            n.feed(KeyPress::char('t'), &c.keymap),
            MatchResult::Matched {
                action: Action::ToggleGraph,
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
    fn highlight_color_parses_zathuras_forms() {
        let hl = |s: &str| Rgba::parse(s).map(|c| c.to_string());
        assert_eq!(hl("#ff0").unwrap(), "rgba(255, 255, 0, 1)");
        assert_eq!(hl("#9FFB00").unwrap(), "rgba(159, 251, 0, 1)");
        assert_eq!(
            hl("#9ffb0080").unwrap(),
            "rgba(159, 251, 0, 0.5019607843137255)"
        );
        assert_eq!(hl("rgb(1, 2, 3)").unwrap(), "rgba(1, 2, 3, 1)");
        assert_eq!(
            hl(" RGBA(159,251,0,0.5) ").unwrap(),
            "rgba(159, 251, 0, 0.5)"
        );
        for bad in [
            "",
            "yellow",
            "#12",
            "#12345",
            "#gg0000",
            "#ffé",
            "rgb(256, 0, 0)",
            "rgb(1, 2)",
            "rgb(1, 2, 3, 0.5)",
            "rgba(1, 2, 3)",
            "rgba(1, 2, 3, 1.5)",
            "rgba(1, 2, 3, NaN)",
            "rgb(1, 2, 3); } body { color: red",
        ] {
            assert!(hl(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn highlight_color_defaults_to_zathuras_and_is_settable() {
        let mut o = Options::default();
        assert_eq!(o.highlight_color, Rgba::ZATHURA_HIGHLIGHT);
        assert_eq!(
            o.set("highlight-color", "\"#ffff00\"").unwrap(),
            SetEffect::Rerender
        );
        assert_eq!(o.highlight_color.to_string(), "rgba(255, 255, 0, 1)");
        assert!(o.set("highlight-color", "nope").is_err());

        let c = Config::parse("[options]\nhighlight-color = \"rgba(0, 188, 0, 0.5)\"\n").unwrap();
        assert_eq!(
            c.options.highlight_color.to_string(),
            "rgba(0, 188, 0, 0.5)"
        );
        assert!(Config::parse("[options]\nhighlight-color = \"blue\"\n").is_err());
    }

    #[test]
    fn highlight_active_color_defaults_to_zathuras_and_is_settable() {
        let mut o = Options::default();
        assert_eq!(o.highlight_active_color, Rgba::ZATHURA_HIGHLIGHT_ACTIVE);
        assert_eq!(o.highlight_active_color.to_string(), "rgba(0, 188, 0, 0.5)");
        assert_eq!(
            o.set("highlight-active-color", "\"#f00\"").unwrap(),
            SetEffect::Rerender
        );
        assert_eq!(o.highlight_active_color.to_string(), "rgba(255, 0, 0, 1)");
        assert!(o.set("highlight-active-color", "nope").is_err());

        let c = Config::parse("[options]\nhighlight-active-color = \"#00ff0080\"\n").unwrap();
        assert_eq!(
            c.options.highlight_active_color.to_string(),
            "rgba(0, 255, 0, 0.5019607843137255)"
        );
        assert!(Config::parse("[options]\nhighlight-active-color = \"green\"\n").is_err());
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
    fn show_frontmatter_is_off_by_default_and_settable_at_runtime() {
        let mut o = Options::default();
        assert!(!o.show_frontmatter);
        assert_eq!(
            o.set("show-frontmatter", "true").unwrap(),
            SetEffect::Rerender
        );
        assert!(o.show_frontmatter);
        assert!(o.set("show-frontmatter", "yes").is_err());
    }

    #[test]
    fn show_frontmatter_reads_from_the_config_file() {
        let c = Config::parse("[options]\nshow-frontmatter = true\n").unwrap();
        assert!(c.options.show_frontmatter);
    }

    #[test]
    fn wide_blocks_parses_none_all_and_lists() {
        assert_eq!(WideBlocks::parse("none").unwrap(), WideBlocks::none());
        assert_eq!(WideBlocks::parse("  NONE ").unwrap(), WideBlocks::none());
        assert_eq!(WideBlocks::parse("").unwrap(), WideBlocks::none());
        assert_eq!(WideBlocks::parse("all").unwrap(), WideBlocks::all());
        assert_eq!(WideBlocks::parse("All").unwrap(), WideBlocks::all());
        // Case-insensitive, whitespace-tolerant, order-independent, and a
        // trailing comma is a typo the reader should survive.
        let expected: Vec<WideBlock> = vec![WideBlock::Diagrams, WideBlock::Tables];
        for spelling in ["diagrams,tables", " Tables , DIAGRAMS ", "tables,diagrams,"] {
            let parsed = WideBlocks::parse(spelling).unwrap();
            assert_eq!(
                parsed.iter().collect::<Vec<_>>(),
                expected,
                "parsing {spelling:?}"
            );
        }
    }

    #[test]
    fn wide_blocks_rejects_an_unknown_kind_by_name() {
        let err = WideBlocks::parse("diagrams,widgets").unwrap_err();
        assert!(err.contains("widgets"), "{err}");
        // The message has to be actionable: every valid spelling in it.
        for valid in [
            "none", "all", "diagrams", "fences", "tables", "code", "math",
        ] {
            assert!(err.contains(valid), "{err} should list {valid}");
        }
    }

    #[test]
    fn wide_blocks_display_round_trips_through_parse() {
        for set in [
            WideBlocks::none(),
            WideBlocks::all(),
            WideBlocks::default(),
            WideBlocks::parse("code").unwrap(),
            WideBlocks::parse("math,code,fences").unwrap(),
        ] {
            let shown = set.to_string();
            assert_eq!(
                WideBlocks::parse(&shown).unwrap(),
                set,
                "{shown:?} did not round-trip"
            );
        }
        assert_eq!(WideBlocks::none().to_string(), "none");
        assert_eq!(WideBlocks::all().to_string(), "all");
        assert_eq!(WideBlocks::default().to_string(), "diagrams,fences,tables");
    }

    #[test]
    fn a_wide_block_names_its_own_css_class() {
        // The stylesheet spells these out; the Rust must agree with it, and
        // this is the one place the two meet.
        assert_eq!(WideBlock::Diagrams.css_class(), "jmnj-wide-diagrams");
        assert_eq!(WideBlock::Math.css_class(), "jmnj-wide-math");
        assert_eq!(
            WideBlocks::default().classes().collect::<Vec<_>>(),
            vec!["jmnj-wide-diagrams", "jmnj-wide-fences", "jmnj-wide-tables"]
        );
    }

    #[test]
    fn wide_options_default_on_and_read_from_the_config_file() {
        let d = Options::default();
        assert!(d.wide, "the breakout ships on");
        assert_eq!(d.wide_blocks, WideBlocks::default());

        let c = Config::parse("[options]\nwide = false\nwide-blocks = \"all\"\n").unwrap();
        assert!(!c.options.wide);
        assert_eq!(c.options.wide_blocks, WideBlocks::all());

        let err = Config::parse("[options]\nwide-blocks = \"widgets\"\n").unwrap_err();
        assert!(err.to_string().contains("widgets"), "{err}");
    }

    #[test]
    fn wide_options_are_set_targets_that_re_render() {
        let mut o = Options::default();
        // The pipeline emits the per-kind classes, so a changed *set* only
        // takes effect through a re-render.
        assert_eq!(o.set("wide-blocks", "code").unwrap(), SetEffect::Rerender);
        assert_eq!(o.wide_blocks, WideBlocks::parse("code").unwrap());
        assert_eq!(o.set("wide", "false").unwrap(), SetEffect::Rerender);
        assert!(!o.wide);
        assert!(
            o.set("wide-blocks", "widgets")
                .unwrap_err()
                .contains("wide-blocks")
        );
        assert!(o.set("wide", "sometimes").is_err());
    }

    #[test]
    fn toggle_wide_parses_under_both_spellings() {
        assert_eq!(parse_action("toggle wide").unwrap(), Action::ToggleWide);
        assert_eq!(parse_action("wide").unwrap(), Action::ToggleWide);
        assert!(action_names().contains(&"toggle wide"));
        assert!(option_keys().contains(&"wide-blocks"));
        assert!(option_keys().contains(&"wide"));
    }

    #[test]
    fn diagram_fit_is_off_by_default_and_is_a_set_target_that_re_renders() {
        // Off by default: DESIGN D5a decided intrinsic, and the wide-block
        // breakout already recovers most of the width fit would buy.
        assert!(!Options::default().diagram_fit);
        assert!(
            Config::parse("[options]\ndiagram-fit = true\n")
                .unwrap()
                .options
                .diagram_fit
        );
        let mut o = Options::default();
        assert_eq!(o.set("diagram-fit", "true").unwrap(), SetEffect::Rerender);
        assert!(o.diagram_fit);
        assert!(o.set("diagram-fit", "sometimes").is_err());
        assert!(option_keys().contains(&"diagram-fit"));
    }

    #[test]
    fn toggle_diagram_fit_parses_under_both_spellings() {
        assert_eq!(
            parse_action("toggle diagram fit").unwrap(),
            Action::ToggleDiagramFit
        );
        assert_eq!(
            parse_action("diagram fit").unwrap(),
            Action::ToggleDiagramFit
        );
        assert!(action_names().contains(&"toggle diagram fit"));
    }

    #[test]
    fn background_is_off_by_default_and_reads_from_the_config_file() {
        assert!(!Options::default().background);
        assert!(
            Config::parse("[options]\nbackground = true\n")
                .unwrap()
                .options
                .background
        );
        assert!(
            !Config::parse("[options]\nbackground = false\n")
                .unwrap()
                .options
                .background
        );
        // Startup-only: it is consumed before the window exists, so it is not a
        // `:set` target and must not silently pretend to apply at runtime.
        assert!(Options::default().set("background", "true").is_err());
    }

    #[test]
    fn toggle_frontmatter_parses_under_both_spellings() {
        assert_eq!(
            parse_action("toggle frontmatter").unwrap(),
            Action::ToggleFrontmatter
        );
        assert_eq!(
            parse_action("frontmatter").unwrap(),
            Action::ToggleFrontmatter
        );
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
    fn graph_view_defaults_to_links_and_is_a_set_target_that_re_lays_out() {
        assert_eq!(Options::default().graph_view, GraphView::Links);
        let c = Config::parse("[options]\ngraph-view = \"tree\"\n").unwrap();
        assert_eq!(c.options.graph_view, GraphView::Tree);
        let err = Config::parse("[options]\ngraph-view = \"forest\"\n").unwrap_err();
        assert!(err.to_string().contains("forest"), "{err}");

        let mut o = Options::default();
        assert_eq!(o.set("graph-view", "tree").unwrap(), SetEffect::Relayout);
        assert_eq!(o.graph_view, GraphView::Tree);
        assert!(
            o.set("graph-view", "forest")
                .unwrap_err()
                .contains("graph-view")
        );
        assert!(option_keys().contains(&"graph-view"));
        assert_eq!(parse_action("graph view").unwrap(), Action::GraphToggleView);
        assert!(action_names().contains(&"graph view"));
    }

    /// The example config lists every built-in binding, one commented line
    /// each. Uncommented and applied to an empty keymap, that listing must be
    /// exactly the defaults — a new default key, or a changed one, has to show
    /// up there too.
    #[test]
    fn the_example_config_lists_exactly_the_default_bindings() {
        let example = include_str!("../../resources/config.example.toml");
        // The key section starts at the `[keys.normal]` table header line
        // (the prose above it mentions the name too).
        let keys = &example[example
            .find("\n[keys.normal]\n")
            .expect("the example has a key section")..];
        let listing: String = keys
            .lines()
            .map(|line| {
                let binding = line.starts_with("# \"") && line.contains("\" = ");
                if line.starts_with("# [keys.") || binding {
                    &line[2..]
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let raw: RawConfig = toml::from_str(&listing).expect("the listing is valid TOML");
        let tables = raw.keys.expect("the listing has key tables");
        let mut keymap = Keymap::empty();
        apply_key_table(&mut keymap, Mode::Normal, "normal", tables.normal).unwrap();
        apply_key_table(&mut keymap, Mode::Toc, "toc", tables.toc).unwrap();
        apply_key_table(&mut keymap, Mode::Graph, "graph", tables.graph).unwrap();
        assert_eq!(keymap, Keymap::default());
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
