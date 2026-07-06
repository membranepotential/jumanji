//! Functional core: pure, GTK-free, unit-tested.
//!
//! Everything in this module must be testable without a display.

pub mod config;
pub mod keymap;
pub mod pipeline;
pub mod toc;

mod diagram;
mod highlight;

/// A heading in the document, used for the TOC and section navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// 1..=6
    pub level: u8,
    pub text: String,
    /// HTML fragment id, e.g. `#installation` (GitHub-style slug, unique).
    pub anchor: String,
}

/// Result of rendering a markdown document.
#[derive(Debug, Clone)]
pub struct RenderedDocument {
    /// A complete, self-contained HTML document (embedded CSS, inline SVG
    /// diagrams, no external resources except document-relative images).
    pub html: String,
    pub toc: Vec<Heading>,
}

/// Scroll directions for [`Action::Scroll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Everything a key sequence can do. The single vocabulary shared between
/// the keymap (core) and the shell that executes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Scroll by `scroll-step` pixels (multiplied by count).
    Scroll(Direction),
    /// Scroll half a viewport (multiplied by count).
    HalfPage(Direction),
    GotoTop,
    /// `G` without a count. With a count N, the keymap emits `GotoSection(N)`.
    GotoBottom,
    /// 1-based index into the document's heading list.
    GotoSection(u32),
    SectionNext,
    SectionPrevious,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    SearchStart,
    SearchNext,
    SearchPrevious,
    /// Toggle dark-mode recoloring.
    Recolor,
    Reload,
    ToggleToc,
    CommandLine,
    Abort,
    Quit,
}

/// Input modes, girara-style. Keybindings are scoped per mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Normal,
    /// Table-of-contents overlay.
    Toc,
}
