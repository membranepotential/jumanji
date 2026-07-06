//! Functional core: pure, GTK-free, unit-tested.
//!
//! Everything in this module must be testable without a display.

pub mod command;
pub mod config;
pub mod editor;
pub mod history;
pub mod jumplist;
pub mod keymap;
pub mod marks;
pub mod pipeline;
pub mod toc;

mod diagram;
mod fence;
mod highlight;
mod math;

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
    /// Geometric zoom in (webkit `zoom_level`): scales the whole page,
    /// diagrams included. Zathura's `zoom in` maps here for muscle memory.
    ZoomIn,
    /// Geometric zoom out.
    ZoomOut,
    /// Text zoom in (`--font-size` CSS variable): reflows text without
    /// touching layout geometry or diagram sizing.
    TextZoomIn,
    /// Text zoom out.
    TextZoomOut,
    /// Reset *both* zoom axes to 100%.
    ZoomReset,
    SearchStart,
    SearchNext,
    SearchPrevious,
    /// Toggle dark-mode recoloring.
    Recolor,
    Reload,
    ToggleToc,
    CommandLine,
    /// Enter link-hint mode and jump to the chosen link (hint interaction and
    /// overlay live in the shell; the keymap only fires the entry action).
    FollowLink,
    /// Enter link-hint mode and show the chosen link's target in the statusbar
    /// instead of navigating to it.
    ShowLinkTarget,
    /// Set quickmark `char` to the current reading position (zathura `m<x>`).
    QuickmarkSet(char),
    /// Jump to the position stored in quickmark `char` (zathura `'<x>`).
    QuickmarkJump(char),
    /// Jumplist back (`Ctrl-o`).
    JumpBackward,
    /// Jumplist forward (`Ctrl-i`).
    JumpForward,
    /// TOC-mode: move the selection to the next visible entry.
    TocNext,
    /// TOC-mode: move the selection to the previous visible entry.
    TocPrevious,
    /// TOC-mode: expand the selected entry's children.
    TocExpand,
    /// TOC-mode: collapse the selected entry.
    TocCollapse,
    /// TOC-mode: jump to the selected entry and leave TOC mode.
    TocSelect,
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
