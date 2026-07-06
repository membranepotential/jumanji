//! Vim-style jumplist for scroll positions (`Ctrl-o` / `Ctrl-i`).
//!
//! Pure and GTK-free. Positions are opaque scroll offsets (`f64`); the shell
//! decides what a "jump" is (section moves, anchor follows, quickmark jumps).
//!
//! Semantics mirror vim: [`push`](Jumplist::push) records the position *before*
//! a jump and discards any forward history; [`back`](Jumplist::back) walks
//! toward older entries, saving the live position on the first step so
//! [`forward`](Jumplist::forward) can return to it.
//!
// Wired into `Ctrl-o`/`Ctrl-i` by the parallel M2 shell-integration work; the
// pure list and its tests land first, hence the allow.
#![allow(dead_code)]

/// Maximum retained entries (vim's default `'jumplist'` size).
const CAPACITY: usize = 100;

#[derive(Debug, Clone, Default)]
pub struct Jumplist {
    /// Recorded positions, oldest first. When navigating, the newest entry may
    /// be the live position saved by the first `back`.
    entries: Vec<f64>,
    /// Index of the entry we are currently "at". `pos == entries.len()` means
    /// we are at the live (unrecorded) position past the newest entry.
    pos: usize,
}

impl Jumplist {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `y` as the position before a jump, discarding any forward tail
    /// (a new jump invalidates entries we had stepped back past).
    pub fn push(&mut self, y: f64) {
        self.entries.truncate(self.pos.min(self.entries.len()));
        self.entries.push(y);
        self.enforce_cap();
        self.pos = self.entries.len();
    }

    /// Step back toward older positions (`Ctrl-o`). On the first step from the
    /// live position, `current` is appended so a later [`forward`] can return
    /// to it. Returns the position to scroll to, or `None` at the oldest entry.
    pub fn back(&mut self, current: f64) -> Option<f64> {
        if self.entries.is_empty() {
            return None;
        }
        if self.pos >= self.entries.len() {
            // At the live position: save it, then land on the newest jump.
            let target = self.entries.len() - 1;
            self.entries.push(current);
            self.pos = target;
            self.enforce_cap();
            return self.entries.get(self.pos).copied();
        }
        if self.pos == 0 {
            return None;
        }
        self.pos -= 1;
        self.entries.get(self.pos).copied()
    }

    /// Step forward toward newer positions (`Ctrl-i`). Returns the position to
    /// scroll to, or `None` when already at the newest.
    pub fn forward(&mut self) -> Option<f64> {
        if self.pos + 1 < self.entries.len() {
            self.pos += 1;
            self.entries.get(self.pos).copied()
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

    #[test]
    fn empty_jumplist_navigates_to_nothing() {
        let mut j = Jumplist::new();
        assert_eq!(j.back(5.0), None);
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn back_then_forward_round_trips_including_live_position() {
        let mut j = Jumplist::new();
        j.push(10.0);
        j.push(20.0);
        j.push(30.0);
        // Live position is 40; walk all the way back.
        assert_eq!(j.back(40.0), Some(30.0));
        assert_eq!(j.back(40.0), Some(20.0));
        assert_eq!(j.back(40.0), Some(10.0));
        assert_eq!(j.back(40.0), None);
        // Forward returns through the entries and finally the saved live pos.
        assert_eq!(j.forward(), Some(20.0));
        assert_eq!(j.forward(), Some(30.0));
        assert_eq!(j.forward(), Some(40.0));
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn push_after_back_truncates_forward_tail() {
        let mut j = Jumplist::new();
        j.push(10.0);
        j.push(20.0);
        j.push(30.0);
        assert_eq!(j.back(40.0), Some(30.0));
        assert_eq!(j.back(40.0), Some(20.0));
        // A new jump from the middle drops the forward history (30, live 40).
        j.push(25.0);
        assert_eq!(j.forward(), None);
        // Back now walks the rebuilt list: newest recorded is 25.
        assert_eq!(j.back(99.0), Some(25.0));
        assert_eq!(j.back(99.0), Some(10.0));
        assert_eq!(j.back(99.0), None);
    }

    #[test]
    fn capacity_is_bounded_and_drops_oldest() {
        let mut j = Jumplist::new();
        for i in 0..150 {
            j.push(i as f64);
        }
        // Newest recorded position survives; oldest are evicted.
        assert_eq!(j.back(999.0), Some(149.0));
        let mut steps = 1;
        while j.back(999.0).is_some() {
            steps += 1;
        }
        // Never more than CAPACITY reachable entries.
        assert!(steps <= CAPACITY, "reachable entries: {steps}");
    }

    #[test]
    fn forward_without_prior_back_is_none() {
        let mut j = Jumplist::new();
        j.push(1.0);
        assert_eq!(j.forward(), None);
    }
}
