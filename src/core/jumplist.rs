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
use std::path::{Path, PathBuf};

/// Maximum retained entries (vim's default `'jumplist'` size).
const CAPACITY: usize = 100;

/// Separator between breadcrumb segments.
const SEP: &str = " > ";
/// Stands in for the segments dropped off the left of a breadcrumb.
const ELLIPSIS: &str = "…";

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

    /// The documents on the path to `current`, oldest first and ending at
    /// `current` — the route that got us here (`index.md > topic.md > note.md`).
    ///
    /// Only the entries *behind* the cursor count, so walking back with
    /// `Ctrl-o` shortens the trail and `Ctrl-i` re-extends it. Consecutive jumps
    /// within one document collapse to a single segment: a breadcrumb answers
    /// "which files", not "how many jumps".
    pub fn trail<'a>(&'a self, current: Option<&'a Path>) -> Vec<Option<&'a Path>> {
        let behind = self.entries[..self.pos.min(self.entries.len())]
            .iter()
            .map(|loc| loc.doc.as_deref());
        let mut out: Vec<Option<&Path>> = Vec::new();
        for doc in behind.chain(std::iter::once(current)) {
            if out.last() != Some(&doc) {
                out.push(doc);
            }
        }
        out
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

/// Render a [`trail`](Jumplist::trail) of display names as `a > b > c`, fitted
/// to `max_cols` monospace columns.
///
/// Overflow is cut from the **left** — whole segments are dropped oldest-first
/// and replaced by a leading `…`, so the current document (the last segment) is
/// always visible. A last segment too long for `max_cols` on its own is left
/// over-long rather than mangled; the statusbar label ellipsizes the remainder.
pub fn breadcrumb(segments: &[String], max_cols: usize) -> String {
    let Some((last, older)) = segments.split_last() else {
        return String::new();
    };
    let sep = SEP.chars().count();
    // What a leading `… > ` costs, reserved while anything is still to the left.
    let marker = ELLIPSIS.chars().count() + sep;

    let mut cols = last.chars().count();
    let mut kept = 0;
    for (i, seg) in older.iter().enumerate().rev() {
        let more_left = i > 0;
        let width = cols + sep + seg.chars().count() + if more_left { marker } else { 0 };
        if width > max_cols {
            break;
        }
        cols = width - if more_left { marker } else { 0 };
        kept += 1;
    }

    let dropped = older.len() - kept;
    let mut out = String::new();
    if dropped > 0 {
        out.push_str(ELLIPSIS);
        out.push_str(SEP);
    }
    for seg in &older[dropped..] {
        out.push_str(seg);
        out.push_str(SEP);
    }
    out.push_str(last);
    out
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

    /// The three-document reading route `a.md → b.md → c.md`, as the shell
    /// records it: each link follow pushes the departure, then the file changes.
    fn route() -> (Jumplist, PathBuf, PathBuf, PathBuf) {
        let (a, b, c) = (
            PathBuf::from("/v/a.md"),
            PathBuf::from("/v/b.md"),
            PathBuf::from("/v/c.md"),
        );
        let mut j = Jumplist::new();
        j.push(Location {
            doc: Some(a.clone()),
            scroll_y: 100.0,
        });
        j.push(Location {
            doc: Some(b.clone()),
            scroll_y: 10.0,
        });
        (j, a, b, c)
    }

    #[test]
    fn trail_is_the_route_to_the_current_document() {
        let (j, a, b, c) = route();
        assert_eq!(
            j.trail(Some(&c)),
            vec![Some(a.as_path()), Some(b.as_path()), Some(c.as_path())]
        );
    }

    #[test]
    fn trail_collapses_consecutive_jumps_within_one_document() {
        let a = PathBuf::from("/v/a.md");
        let mut j = Jumplist::new();
        // Section jumps inside one document: many entries, one breadcrumb segment.
        j.push(Location {
            doc: Some(a.clone()),
            scroll_y: 0.0,
        });
        j.push(Location {
            doc: Some(a.clone()),
            scroll_y: 400.0,
        });
        assert_eq!(j.trail(Some(&a)), vec![Some(a.as_path())]);
    }

    #[test]
    fn trail_shrinks_walking_back_and_regrows_forward() {
        let (mut j, a, b, c) = route();
        // Ctrl-o from c.md lands on b.md: the trail is now just a.md > b.md.
        j.back(Location {
            doc: Some(c.clone()),
            scroll_y: 0.0,
        });
        assert_eq!(
            j.trail(Some(&b)),
            vec![Some(a.as_path()), Some(b.as_path())]
        );
        // Ctrl-i returns to c.md and the full route with it.
        j.forward();
        assert_eq!(
            j.trail(Some(&c)),
            vec![Some(a.as_path()), Some(b.as_path()), Some(c.as_path())]
        );
    }

    #[test]
    fn trail_of_a_fresh_jumplist_is_just_the_current_document() {
        let j = Jumplist::new();
        let a = PathBuf::from("/v/a.md");
        assert_eq!(j.trail(Some(&a)), vec![Some(a.as_path())]);
        // The stdin stream has no path, and still occupies a segment.
        assert_eq!(j.trail(None), vec![None]);
    }

    /// `["index.md", "topic.md", "note.md"]` — the doc-comment example.
    fn segments() -> Vec<String> {
        ["index.md", "topic.md", "note.md"]
            .map(String::from)
            .to_vec()
    }

    #[test]
    fn breadcrumb_joins_segments_when_it_fits() {
        assert_eq!(breadcrumb(&segments(), 80), "index.md > topic.md > note.md");
        assert_eq!(breadcrumb(&[], 80), "");
    }

    #[test]
    fn breadcrumb_drops_oldest_segments_from_the_left() {
        // One column short of the full trail: the oldest segment goes first.
        assert_eq!(breadcrumb(&segments(), 28), "… > topic.md > note.md");
        assert_eq!(breadcrumb(&segments(), 21), "… > note.md");
    }

    #[test]
    fn breadcrumb_never_drops_the_current_document() {
        // Narrower than even the current filename: it survives anyway (the label
        // ellipsizes), because losing it would leave the reader with nothing.
        assert_eq!(breadcrumb(&segments(), 3), "… > note.md");
        assert_eq!(breadcrumb(&["note.md".to_string()], 3), "note.md");
    }

    #[test]
    fn breadcrumb_fits_its_budget_whenever_the_last_segment_does() {
        let segs = segments();
        // From the narrowest budget that holds `… > note.md` upward.
        for cols in 11..40 {
            let out = breadcrumb(&segs, cols);
            assert!(
                out.chars().count() <= cols,
                "{cols} cols overflowed with {out:?}"
            );
            assert!(out.ends_with("note.md"), "{cols} cols lost the current doc");
        }
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
