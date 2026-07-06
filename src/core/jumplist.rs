//! Vim-style jumplist for reading positions (`Ctrl-o` / `Ctrl-i`).
//!
//! Pure and GTK-free. Each entry is a [`Location`]: which document, plus an
//! opaque scroll offset (`f64`) within it. The shell decides what a "jump" is
//! (section moves, anchor follows, quickmark jumps, **and opening another
//! document via a link**) and how to restore a location.
//!
//! Because a location carries its document, the list spans files: following a
//! link records the departure, so `Ctrl-o` walks back into the previous
//! document at the position you left it. A `doc` of `None` is the live stdin
//! stream, which has no reopenable identity — the shell treats a `None` target
//! as "cannot return".
//!
//! Semantics mirror vim: [`push`](Jumplist::push) records the position *before*
//! a jump and discards any forward history; [`back`](Jumplist::back) walks
//! toward older entries, saving the live position on the first step so
//! [`forward`](Jumplist::forward) can return to it.
use std::path::PathBuf;

/// Maximum retained entries (vim's default `'jumplist'` size).
const CAPACITY: usize = 100;

/// A reading position: which document, and the scroll offset within it.
///
/// `doc == Some(path)` is a file (reopenable); `doc == None` is the live stdin
/// stream. The scroll offset is opaque to the core — only the shell interprets
/// it.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    /// The document this position belongs to, or `None` for the stdin stream.
    pub doc: Option<PathBuf>,
    /// Opaque scroll offset within the document.
    pub scroll_y: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Jumplist {
    /// Recorded locations, oldest first. When navigating, the newest entry may
    /// be the live position saved by the first `back`.
    entries: Vec<Location>,
    /// Index of the entry we are currently "at". `pos == entries.len()` means
    /// we are at the live (unrecorded) position past the newest entry.
    pos: usize,
}

impl Jumplist {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `loc` as the position before a jump, discarding any forward tail
    /// (a new jump invalidates entries we had stepped back past).
    pub fn push(&mut self, loc: Location) {
        self.entries.truncate(self.pos.min(self.entries.len()));
        self.entries.push(loc);
        self.enforce_cap();
        self.pos = self.entries.len();
    }

    /// Step back toward older positions (`Ctrl-o`). On the first step from the
    /// live position, `current` is appended so a later [`forward`] can return
    /// to it. Returns the location to restore, or `None` at the oldest entry.
    pub fn back(&mut self, current: Location) -> Option<Location> {
        if self.entries.is_empty() {
            return None;
        }
        if self.pos >= self.entries.len() {
            // At the live position: save it, then land on the newest jump.
            let target = self.entries.len() - 1;
            self.entries.push(current);
            self.pos = target;
            self.enforce_cap();
            return self.entries.get(self.pos).cloned();
        }
        if self.pos == 0 {
            return None;
        }
        self.pos -= 1;
        self.entries.get(self.pos).cloned()
    }

    /// Step forward toward newer positions (`Ctrl-i`). Returns the location to
    /// restore, or `None` when already at the newest.
    pub fn forward(&mut self) -> Option<Location> {
        if self.pos + 1 < self.entries.len() {
            self.pos += 1;
            self.entries.get(self.pos).cloned()
        } else {
            None
        }
    }

    /// Trim to [`CAPACITY`] by dropping the oldest entries, keeping `pos`
    /// pointing at the same logical entry.
    fn enforce_cap(&mut self) {
        if self.entries.len() > CAPACITY {
            let over = self.entries.len() - CAPACITY;
            self.entries.drain(0..over);
            self.pos = self.pos.saturating_sub(over);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A same-document location at scroll offset `y` (the common test case).
    fn at(y: f64) -> Location {
        Location {
            doc: None,
            scroll_y: y,
        }
    }

    #[test]
    fn empty_jumplist_navigates_to_nothing() {
        let mut j = Jumplist::new();
        assert_eq!(j.back(at(5.0)), None);
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn back_then_forward_round_trips_including_live_position() {
        let mut j = Jumplist::new();
        j.push(at(10.0));
        j.push(at(20.0));
        j.push(at(30.0));
        // Live position is 40; walk all the way back.
        assert_eq!(j.back(at(40.0)), Some(at(30.0)));
        assert_eq!(j.back(at(40.0)), Some(at(20.0)));
        assert_eq!(j.back(at(40.0)), Some(at(10.0)));
        assert_eq!(j.back(at(40.0)), None);
        // Forward returns through the entries and finally the saved live pos.
        assert_eq!(j.forward(), Some(at(20.0)));
        assert_eq!(j.forward(), Some(at(30.0)));
        assert_eq!(j.forward(), Some(at(40.0)));
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn push_after_back_truncates_forward_tail() {
        let mut j = Jumplist::new();
        j.push(at(10.0));
        j.push(at(20.0));
        j.push(at(30.0));
        assert_eq!(j.back(at(40.0)), Some(at(30.0)));
        assert_eq!(j.back(at(40.0)), Some(at(20.0)));
        // A new jump from the middle drops the forward history (30, live 40).
        j.push(at(25.0));
        assert_eq!(j.forward(), None);
        // Back now walks the rebuilt list: newest recorded is 25.
        assert_eq!(j.back(at(99.0)), Some(at(25.0)));
        assert_eq!(j.back(at(99.0)), Some(at(10.0)));
        assert_eq!(j.back(at(99.0)), None);
    }

    #[test]
    fn capacity_is_bounded_and_drops_oldest() {
        let mut j = Jumplist::new();
        for i in 0..150 {
            j.push(at(i as f64));
        }
        // Newest recorded position survives; oldest are evicted.
        assert_eq!(j.back(at(999.0)), Some(at(149.0)));
        let mut steps = 1;
        while j.back(at(999.0)).is_some() {
            steps += 1;
        }
        // Never more than CAPACITY reachable entries.
        assert!(steps <= CAPACITY, "reachable entries: {steps}");
    }

    #[test]
    fn forward_without_prior_back_is_none() {
        let mut j = Jumplist::new();
        j.push(at(1.0));
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn crosses_documents_carrying_the_source_file() {
        let a = |y| Location {
            doc: Some(PathBuf::from("/a.md")),
            scroll_y: y,
        };
        let b = |y| Location {
            doc: Some(PathBuf::from("/b.md")),
            scroll_y: y,
        };
        let mut j = Jumplist::new();
        // Read a.md at 100, follow a link to b.md (records the departure).
        j.push(a(100.0));
        // From b.md at 5, Ctrl-o returns to a.md at exactly where we left it.
        assert_eq!(j.back(b(5.0)), Some(a(100.0)));
        // Ctrl-i goes forward to the saved b.md position.
        assert_eq!(j.forward(), Some(b(5.0)));
    }
}
