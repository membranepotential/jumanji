//! Girara-style key dispatch: `mode × count × key-sequence → Action`.
//!
//! Pure and GTK-free. The shell converts a GDK keyval + modifiers into a
//! [`KeyPress`] and feeds it to a [`Matcher`], which handles count prefixes
//! (`5j`) and multi-key sequences (`gg`) generically — never per-binding.

use std::collections::HashMap;

use super::{Action, Direction, Mode};

/// A single key, GTK-free. Printable keys carry their (case-significant)
/// character directly, so `Shift` is folded into the character for those;
/// named keys are enumerated so specials survive without a keyval table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A printable character, exactly as typed (`j`, `J`, `/`, `+`, `5`).
    Char(char),
    Escape,
    Tab,
    Enter,
    Space,
    Backspace,
}

/// A key plus active modifiers. `Shift` is only meaningful for named keys;
/// for [`Key::Char`] the case already encodes it, so [`KeyPress::new`]
/// normalizes `shift` away to keep equality/hashing well-defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyPress {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
}

impl KeyPress {
    /// Construct a keypress, folding `shift` into the character for
    /// [`Key::Char`] so that `Shift+j` and a literal `J` compare equal.
    pub fn new(key: Key, ctrl: bool, shift: bool) -> Self {
        let shift = if matches!(key, Key::Char(_)) {
            false
        } else {
            shift
        };
        Self { key, ctrl, shift }
    }

    /// A plain (unmodified) character keypress.
    pub fn char(c: char) -> Self {
        Self::new(Key::Char(c), false, false)
    }
}

/// An ordered sequence of keypresses, e.g. `gg` or `<C-r>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeySequence(pub Vec<KeyPress>);

impl KeySequence {
    pub fn single(kp: KeyPress) -> Self {
        Self(vec![kp])
    }
}

/// A key sequence that captures the *next* keypress as a `char` argument,
/// zathura-style: `m<x>` sets quickmark `x`, `'<x>` jumps to it. The captured
/// character is not known until the follow-up press, so it cannot be baked
/// into a plain [`Action`] at bind time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharArgKind {
    QuickmarkSet,
    QuickmarkJump,
}

impl CharArgKind {
    /// Build the concrete [`Action`] once the argument char is captured.
    fn with_char(self, c: char) -> Action {
        match self {
            CharArgKind::QuickmarkSet => Action::QuickmarkSet(c),
            CharArgKind::QuickmarkJump => Action::QuickmarkJump(c),
        }
    }
}

/// What a key sequence maps to: either a fixed action, or a prefix that
/// captures the next character as its argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    Action(Action),
    CharArg(CharArgKind),
}

/// The bindings for every mode. Built from [`Keymap::default`] and then
/// overlaid with user overrides from the config.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: HashMap<Mode, HashMap<KeySequence, Binding>>,
}

impl Keymap {
    /// Install or replace a plain action binding for `mode`.
    pub fn bind(&mut self, mode: Mode, seq: KeySequence, action: Action) {
        self.bindings
            .entry(mode)
            .or_default()
            .insert(seq, Binding::Action(action));
    }

    /// Install or replace a char-argument binding (e.g. `m`/`'`) for `mode`.
    pub fn bind_char_arg(&mut self, mode: Mode, seq: KeySequence, kind: CharArgKind) {
        self.bindings
            .entry(mode)
            .or_default()
            .insert(seq, Binding::CharArg(kind));
    }

    fn lookup(&self, mode: Mode, seq: &[KeyPress]) -> Lookup {
        let Some(table) = self.bindings.get(&mode) else {
            return Lookup::None;
        };
        if let Some(binding) = table.get(seq_as_slice_key(seq)) {
            return Lookup::Exact(binding.clone());
        }
        // Partial match: some binding starts with `seq`.
        let is_prefix = table
            .keys()
            .any(|k| k.0.len() > seq.len() && k.0.starts_with(seq));
        if is_prefix {
            Lookup::Prefix
        } else {
            Lookup::None
        }
    }
}

// `HashMap<KeySequence, _>` is keyed by the newtype; borrow a slice into it by
// wrapping. Since `KeySequence` is `Vec`-backed we look up via a temporary.
fn seq_as_slice_key(seq: &[KeyPress]) -> &KeySeqSlice {
    // Safe: `KeySeqSlice` is a `#[repr(transparent)]` wrapper over `[KeyPress]`.
    unsafe { &*(seq as *const [KeyPress] as *const KeySeqSlice) }
}

#[repr(transparent)]
struct KeySeqSlice([KeyPress]);

// Make `HashMap<KeySequence, _>::get` accept a `&KeySeqSlice`.
impl std::borrow::Borrow<KeySeqSlice> for KeySequence {
    fn borrow(&self) -> &KeySeqSlice {
        seq_as_slice_key(&self.0)
    }
}
impl PartialEq for KeySeqSlice {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for KeySeqSlice {}
impl std::hash::Hash for KeySeqSlice {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

enum Lookup {
    Exact(Binding),
    Prefix,
    None,
}

impl Default for Keymap {
    fn default() -> Self {
        let mut km = Keymap {
            bindings: HashMap::new(),
        };
        use Action::*;
        use Direction::*;
        let n = Mode::Normal;
        let c = |ch| KeySequence::single(KeyPress::char(ch));
        let ctrl = |ch| KeySequence::single(KeyPress::new(Key::Char(ch), true, false));

        km.bind(n, c('j'), Scroll(Down));
        km.bind(n, c('k'), Scroll(Up));
        km.bind(n, c('h'), Scroll(Left));
        km.bind(n, c('l'), Scroll(Right));
        km.bind(n, c('d'), HalfPage(Down));
        km.bind(n, c('u'), HalfPage(Up));
        km.bind(n, c('J'), SectionNext);
        km.bind(n, c('K'), SectionPrevious);
        km.bind(
            n,
            KeySequence(vec![KeyPress::char('g'), KeyPress::char('g')]),
            GotoTop,
        );
        // `G` alone → GotoBottom; the matcher rewrites `<count>G` → GotoSection.
        km.bind(n, c('G'), GotoBottom);
        // `+`/`-` drive geometric zoom (zathura muscle memory; scales diagrams
        // too). Text zoom has no default key (reachable via config
        // `text zoom in`/`text zoom out` and Ctrl+Shift+wheel). `=` resets
        // both axes.
        km.bind(n, c('+'), ZoomIn);
        km.bind(n, c('-'), ZoomOut);
        km.bind(n, c('='), ZoomReset);
        km.bind(n, c('/'), SearchStart);
        km.bind(n, c('n'), SearchNext);
        km.bind(n, c('N'), SearchPrevious);
        km.bind(n, ctrl('r'), Recolor);
        km.bind(n, c('r'), Reload);
        km.bind(
            n,
            KeySequence::single(KeyPress::new(Key::Tab, false, false)),
            ToggleToc,
        );
        km.bind(n, c(':'), CommandLine);
        // Link hints, quickmarks, and the jumplist (M2). `f`/`F` enter the
        // shell-side hint overlay; `m`/`'` capture the next char as the
        // quickmark register; `Ctrl-o`/`Ctrl-i` walk the jumplist.
        km.bind(n, c('f'), FollowLink);
        km.bind(n, c('F'), ShowLinkTarget);
        km.bind_char_arg(n, c('m'), CharArgKind::QuickmarkSet);
        km.bind_char_arg(n, c('\''), CharArgKind::QuickmarkJump);
        km.bind(n, ctrl('o'), JumpBackward);
        km.bind(n, ctrl('i'), JumpForward);
        km.bind(
            n,
            KeySequence::single(KeyPress::new(Key::Escape, false, false)),
            Abort,
        );
        km.bind(n, c('q'), Quit);

        // TOC-mode keys (zathura index navigation). Reachable once the shell
        // switches the matcher into `Mode::Toc`; all remappable via
        // `[keys.toc]`.
        let t = Mode::Toc;
        km.bind(t, c('j'), TocNext);
        km.bind(t, c('k'), TocPrevious);
        km.bind(t, c('l'), TocExpand);
        km.bind(t, c('h'), TocCollapse);
        km.bind(
            t,
            KeySequence::single(KeyPress::new(Key::Enter, false, false)),
            TocSelect,
        );
        km.bind(
            t,
            KeySequence::single(KeyPress::new(Key::Tab, false, false)),
            ToggleToc,
        );
        km.bind(t, c('q'), Quit);
        km
    }
}

/// Result of feeding one keypress to the [`Matcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    /// A partial match: more input may complete a binding (`g`, count digits).
    Pending,
    /// A binding fired. `count` is the numeric prefix, if any and not already
    /// consumed by the action (e.g. `GotoSection` bakes the count in).
    Matched { action: Action, count: Option<u32> },
    /// No binding matches; the matcher has reset itself.
    NoMatch,
}

/// Stateful key-sequence matcher for a single mode.
#[derive(Debug, Clone)]
pub struct Matcher {
    mode: Mode,
    count: Option<u32>,
    pending: Vec<KeyPress>,
    /// When set, the previous keypress was a char-argument prefix (`m`/`'`) and
    /// the next printable press is captured as its argument.
    capture: Option<CharArgKind>,
}

impl Matcher {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            count: None,
            pending: Vec::new(),
            capture: None,
        }
    }

    /// Reset to `mode`, clearing any buffered count/sequence. Used by `Abort`.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.reset();
    }

    /// A short human-readable echo of the buffered count/sequence, for the
    /// statusbar (e.g. `5`, `g`, `5g`). Empty when nothing is pending.
    pub fn pending_indicator(&self) -> String {
        let mut s = String::new();
        if let Some(count) = self.count {
            s.push_str(&count.to_string());
        }
        for kp in &self.pending {
            if kp.ctrl {
                s.push('^');
            }
            match kp.key {
                Key::Char(c) => s.push(c),
                Key::Escape => s.push_str("<Esc>"),
                Key::Tab => s.push_str("<Tab>"),
                Key::Enter => s.push_str("<CR>"),
                Key::Space => s.push_str("<Space>"),
                Key::Backspace => s.push_str("<BS>"),
            }
        }
        s
    }

    fn reset(&mut self) {
        self.count = None;
        self.pending.clear();
        self.capture = None;
    }

    /// Feed one keypress; advance the matcher and report the outcome.
    pub fn feed(&mut self, kp: KeyPress, keymap: &Keymap) -> MatchResult {
        // A char-argument prefix (`m`/`'`) is pending: the next press is its
        // argument. Any non-control character is captured; Escape (or any
        // non-character key) aborts the pending capture.
        if let Some(kind) = self.capture.take() {
            self.reset();
            return match kp.key {
                Key::Char(c) if !c.is_control() => MatchResult::Matched {
                    action: kind.with_char(c),
                    count: None,
                },
                _ => MatchResult::NoMatch,
            };
        }

        // Count digits accumulate only at the start of a sequence, unmodified.
        if self.pending.is_empty()
            && !kp.ctrl
            && let Key::Char(ch) = kp.key
            && let Some(digit) = ch.to_digit(10)
            // A leading `0` is not a count (no binding uses it either).
            && (self.count.is_some() || digit != 0)
        {
            let acc = self.count.unwrap_or(0);
            self.count = Some(acc.saturating_mul(10).saturating_add(digit));
            return MatchResult::Pending;
        }

        self.pending.push(kp);
        match keymap.lookup(self.mode, &self.pending) {
            Lookup::Exact(Binding::CharArg(kind)) => {
                // Arm the char-argument capture; the mark register comes on the
                // next press. Quickmarks take no count, so drop any prefix.
                self.reset();
                self.capture = Some(kind);
                MatchResult::Pending
            }
            Lookup::Exact(Binding::Action(action)) => {
                let count = self.count;
                self.reset();
                // `<count>G` folds the count into `GotoSection`; the count is
                // then consumed and not reported for further multiplication.
                match (action, count) {
                    (Action::GotoBottom, Some(n)) => MatchResult::Matched {
                        action: Action::GotoSection(n),
                        count: None,
                    },
                    (action, count) => MatchResult::Matched { action, count },
                }
            }
            Lookup::Prefix => MatchResult::Pending,
            Lookup::None => {
                self.reset();
                MatchResult::NoMatch
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Action, Direction, Mode};

    fn km() -> Keymap {
        Keymap::default()
    }

    fn feed_str(m: &mut Matcher, km: &Keymap, s: &str) -> MatchResult {
        let mut last = MatchResult::NoMatch;
        for ch in s.chars() {
            last = m.feed(KeyPress::char(ch), km);
        }
        last
    }

    #[test]
    fn single_key_scrolls() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(
            m.feed(KeyPress::char('j'), &km),
            MatchResult::Matched {
                action: Action::Scroll(Direction::Down),
                count: None
            }
        );
    }

    #[test]
    fn count_prefix_is_generic() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(m.feed(KeyPress::char('5'), &km), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('j'), &km),
            MatchResult::Matched {
                action: Action::Scroll(Direction::Down),
                count: Some(5)
            }
        );
        // Matcher resets after firing.
        assert!(m.pending_indicator().is_empty());
    }

    #[test]
    fn multi_digit_count() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(feed_str(&mut m, &km, "12"), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('k'), &km),
            MatchResult::Matched {
                action: Action::Scroll(Direction::Up),
                count: Some(12)
            }
        );
    }

    #[test]
    fn gg_sequence() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(m.feed(KeyPress::char('g'), &km), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('g'), &km),
            MatchResult::Matched {
                action: Action::GotoTop,
                count: None
            }
        );
    }

    #[test]
    fn g_without_count_is_bottom() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(
            m.feed(KeyPress::char('G'), &km),
            MatchResult::Matched {
                action: Action::GotoBottom,
                count: None
            }
        );
    }

    #[test]
    fn count_g_is_goto_section() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(feed_str(&mut m, &km, "3"), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('G'), &km),
            MatchResult::Matched {
                action: Action::GotoSection(3),
                count: None
            }
        );
    }

    #[test]
    fn unknown_key_resets() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(feed_str(&mut m, &km, "5"), MatchResult::Pending);
        // `z` is unbound: reset, no leftover count.
        assert_eq!(m.feed(KeyPress::char('z'), &km), MatchResult::NoMatch);
        assert!(m.pending_indicator().is_empty());
        // Fresh `j` scrolls with no count.
        assert_eq!(
            m.feed(KeyPress::char('j'), &km),
            MatchResult::Matched {
                action: Action::Scroll(Direction::Down),
                count: None
            }
        );
    }

    #[test]
    fn partial_g_then_bad_key_resets() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(m.feed(KeyPress::char('g'), &km), MatchResult::Pending);
        assert_eq!(m.feed(KeyPress::char('x'), &km), MatchResult::NoMatch);
        assert!(m.pending_indicator().is_empty());
    }

    #[test]
    fn ctrl_r_vs_r_distinct() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(
            m.feed(KeyPress::char('r'), &km),
            MatchResult::Matched {
                action: Action::Reload,
                count: None
            }
        );
        assert_eq!(
            m.feed(KeyPress::new(Key::Char('r'), true, false), &km),
            MatchResult::Matched {
                action: Action::Recolor,
                count: None
            }
        );
    }

    #[test]
    fn leading_zero_is_not_a_count() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        // `0` is unbound and not a count start.
        assert_eq!(m.feed(KeyPress::char('0'), &km), MatchResult::NoMatch);
    }

    #[test]
    fn zero_extends_existing_count() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(m.feed(KeyPress::char('1'), &km), MatchResult::Pending);
        assert_eq!(m.feed(KeyPress::char('0'), &km), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('j'), &km),
            MatchResult::Matched {
                action: Action::Scroll(Direction::Down),
                count: Some(10)
            }
        );
    }

    #[test]
    fn shift_folds_into_char() {
        // Shift+j (char already uppercased by the shell) equals a literal `J`.
        assert_eq!(
            KeyPress::new(Key::Char('J'), false, true),
            KeyPress::char('J')
        );
    }

    #[test]
    fn tab_toggles_toc() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(
            m.feed(KeyPress::new(Key::Tab, false, false), &km),
            MatchResult::Matched {
                action: Action::ToggleToc,
                count: None
            }
        );
    }

    #[test]
    fn quickmark_set_captures_next_char() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        // `m` arms the capture (pending), `a` completes it.
        assert_eq!(m.feed(KeyPress::char('m'), &km), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('a'), &km),
            MatchResult::Matched {
                action: Action::QuickmarkSet('a'),
                count: None
            }
        );
        assert!(m.pending_indicator().is_empty());
    }

    #[test]
    fn quickmark_jump_captures_next_char() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(m.feed(KeyPress::char('\''), &km), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('Z'), &km),
            MatchResult::Matched {
                action: Action::QuickmarkJump('Z'),
                count: None
            }
        );
    }

    #[test]
    fn quickmark_char_is_case_and_symbol_preserving() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        m.feed(KeyPress::char('m'), &km);
        // Uppercase and digit registers must survive verbatim.
        assert_eq!(
            m.feed(KeyPress::char('5'), &km),
            MatchResult::Matched {
                action: Action::QuickmarkSet('5'),
                count: None
            }
        );
    }

    #[test]
    fn escape_aborts_pending_capture() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(m.feed(KeyPress::char('m'), &km), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::new(Key::Escape, false, false), &km),
            MatchResult::NoMatch
        );
        // Capture cleared: a fresh `j` scrolls normally.
        assert_eq!(
            m.feed(KeyPress::char('j'), &km),
            MatchResult::Matched {
                action: Action::Scroll(Direction::Down),
                count: None
            }
        );
    }

    #[test]
    fn count_before_quickmark_is_dropped_not_leaked() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        // `3m` then `a`: the count does not attach to the quickmark and does
        // not leak into the following command.
        assert_eq!(feed_str(&mut m, &km, "3"), MatchResult::Pending);
        assert_eq!(m.feed(KeyPress::char('m'), &km), MatchResult::Pending);
        assert_eq!(
            m.feed(KeyPress::char('a'), &km),
            MatchResult::Matched {
                action: Action::QuickmarkSet('a'),
                count: None
            }
        );
    }

    #[test]
    fn follow_link_and_jumplist_defaults() {
        let km = km();
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(
            m.feed(KeyPress::char('f'), &km),
            MatchResult::Matched {
                action: Action::FollowLink,
                count: None
            }
        );
        assert_eq!(
            m.feed(KeyPress::char('F'), &km),
            MatchResult::Matched {
                action: Action::ShowLinkTarget,
                count: None
            }
        );
        assert_eq!(
            m.feed(KeyPress::new(Key::Char('o'), true, false), &km),
            MatchResult::Matched {
                action: Action::JumpBackward,
                count: None
            }
        );
        assert_eq!(
            m.feed(KeyPress::new(Key::Char('i'), true, false), &km),
            MatchResult::Matched {
                action: Action::JumpForward,
                count: None
            }
        );
    }

    #[test]
    fn toc_mode_navigation_keys() {
        let km = km();
        let mut m = Matcher::new(Mode::Toc);
        assert_eq!(
            m.feed(KeyPress::char('j'), &km),
            MatchResult::Matched {
                action: Action::TocNext,
                count: None
            }
        );
        assert_eq!(
            m.feed(KeyPress::char('k'), &km),
            MatchResult::Matched {
                action: Action::TocPrevious,
                count: None
            }
        );
        assert_eq!(
            m.feed(KeyPress::char('l'), &km),
            MatchResult::Matched {
                action: Action::TocExpand,
                count: None
            }
        );
        assert_eq!(
            m.feed(KeyPress::char('h'), &km),
            MatchResult::Matched {
                action: Action::TocCollapse,
                count: None
            }
        );
        assert_eq!(
            m.feed(KeyPress::new(Key::Enter, false, false), &km),
            MatchResult::Matched {
                action: Action::TocSelect,
                count: None
            }
        );
        assert_eq!(
            m.feed(KeyPress::new(Key::Tab, false, false), &km),
            MatchResult::Matched {
                action: Action::ToggleToc,
                count: None
            }
        );
    }

    #[test]
    fn toc_j_differs_from_normal_j() {
        // Same key, different mode → different action (mode-scoped tables).
        let km = km();
        let mut normal = Matcher::new(Mode::Normal);
        let mut toc = Matcher::new(Mode::Toc);
        assert_eq!(
            normal.feed(KeyPress::char('j'), &km),
            MatchResult::Matched {
                action: Action::Scroll(Direction::Down),
                count: None
            }
        );
        assert_eq!(
            toc.feed(KeyPress::char('j'), &km),
            MatchResult::Matched {
                action: Action::TocNext,
                count: None
            }
        );
    }

    #[test]
    fn override_replaces_default() {
        let mut km = km();
        km.bind(
            Mode::Normal,
            KeySequence::single(KeyPress::char('j')),
            Action::Quit,
        );
        let mut m = Matcher::new(Mode::Normal);
        assert_eq!(
            m.feed(KeyPress::char('j'), &km),
            MatchResult::Matched {
                action: Action::Quit,
                count: None
            }
        );
    }
}
