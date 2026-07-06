//! Quickmarks: a per-session `char → position` register (zathura `m`/`'`).
//!
//! Pure and GTK-free. Positions are opaque to the core; the shell captures and
//! restores them. Quickmarks are volatile (not persisted); window-state
//! persistence lives in [`super::history`].
//!
// Wired into `m`/`'` by the parallel M2 shell-integration work; the pure
// register set and its tests land first, hence the allow.
#![allow(dead_code)]

use std::collections::HashMap;

/// A saved reading position: vertical scroll plus the geometric zoom level it
/// was captured at, so a jump can restore the same visual framing.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Position {
    pub scroll_y: f64,
    pub zoom: f64,
}

/// The quickmark register set. Any character is a valid register (`ma`, `m1`,
/// `mA` are distinct), matching zathura/vim.
#[derive(Debug, Clone, Default)]
pub struct Marks {
    map: HashMap<char, Position>,
}

impl Marks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store (or overwrite) the position for register `c`.
    pub fn set(&mut self, c: char, p: Position) {
        self.map.insert(c, p);
    }

    /// Retrieve the position for register `c`, if set.
    pub fn get(&self, c: char) -> Option<Position> {
        self.map.get(&c).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(y: f64) -> Position {
        Position {
            scroll_y: y,
            zoom: 1.0,
        }
    }

    #[test]
    fn set_then_get_returns_position() {
        let mut m = Marks::new();
        m.set('a', pos(120.0));
        assert_eq!(m.get('a'), Some(pos(120.0)));
    }

    #[test]
    fn missing_register_is_none() {
        let m = Marks::new();
        assert_eq!(m.get('z'), None);
    }

    #[test]
    fn set_overwrites_existing_register() {
        let mut m = Marks::new();
        m.set('a', pos(10.0));
        m.set('a', pos(99.0));
        assert_eq!(m.get('a'), Some(pos(99.0)));
    }

    #[test]
    fn registers_are_case_sensitive_and_distinct() {
        let mut m = Marks::new();
        m.set('a', pos(1.0));
        m.set('A', pos(2.0));
        m.set('1', pos(3.0));
        assert_eq!(m.get('a'), Some(pos(1.0)));
        assert_eq!(m.get('A'), Some(pos(2.0)));
        assert_eq!(m.get('1'), Some(pos(3.0)));
    }

    #[test]
    fn position_carries_zoom() {
        let mut m = Marks::new();
        m.set(
            'q',
            Position {
                scroll_y: 50.0,
                zoom: 1.5,
            },
        );
        assert_eq!(m.get('q').unwrap().zoom, 1.5);
    }
}
