//! Per-file window-state persistence (scroll offset, zoom, text zoom).
//!
//! Pure and GTK-free: the shell does the I/O (reading/writing
//! `~/.local/share/jumanji/history.toml`); this module only parses, stores, and
//! serializes. Loading is lenient — an unreadable or malformed file yields an
//! empty history rather than failing startup.
//!
//! Entries are stored as an ordered array of tables so LRU order round-trips
//! through TOML (a map would lose ordering). Capacity is bounded to keep the
//! file from growing without limit.
//!
// Read/written at `~/.local/share/jumanji/history.toml` by the parallel M2
// shell-integration work; the pure store and its tests land first, hence the
// allow.
#![allow(dead_code)]

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Most recently used paths retained; older entries are evicted on write.
const CAPACITY: usize = 500;

/// The persisted state for one file.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FileState {
    pub scroll_y: f64,
    pub zoom: f64,
    pub text_zoom: f64,
}

/// LRU-ordered per-file window states, keyed by (stringified) path.
#[derive(Debug, Clone, Default)]
pub struct History {
    /// Oldest first, newest last (the LRU eviction order).
    entries: Vec<Entry>,
}

#[derive(Debug, Clone)]
struct Entry {
    path: String,
    state: FileState,
}

impl History {
    /// Parse history from TOML text. Lenient: any parse error yields an empty
    /// history so a corrupt file never blocks startup. The newest [`CAPACITY`]
    /// entries are kept, with duplicate paths collapsed to their last value.
    pub fn load(toml_str: &str) -> Self {
        let raw: RawHistory = toml::from_str(toml_str).unwrap_or_default();
        let mut history = History::default();
        for e in raw.entry {
            history.record_str(
                e.path,
                FileState {
                    scroll_y: e.scroll_y,
                    zoom: e.zoom,
                    text_zoom: e.text_zoom,
                },
            );
        }
        history
    }

    /// Serialize to TOML, preserving LRU order. Round-trips with [`load`].
    pub fn to_toml(&self) -> String {
        let raw = RawHistory {
            entry: self
                .entries
                .iter()
                .map(|e| RawEntry {
                    path: e.path.clone(),
                    scroll_y: e.state.scroll_y,
                    zoom: e.state.zoom,
                    text_zoom: e.state.text_zoom,
                })
                .collect(),
        };
        // Serializing a plain array-of-tables cannot fail.
        toml::to_string(&raw).unwrap_or_default()
    }

    /// The stored state for `path`, if any.
    pub fn get(&self, path: &Path) -> Option<FileState> {
        let key = key_of(path);
        self.entries.iter().find(|e| e.path == key).map(|e| e.state)
    }

    /// Record (or update) the state for `path`, marking it most-recently-used
    /// and evicting the oldest entry if capacity is exceeded.
    pub fn record(&mut self, path: &Path, st: FileState) {
        self.record_str(key_of(path), st);
    }

    fn record_str(&mut self, key: String, st: FileState) {
        self.entries.retain(|e| e.path != key);
        self.entries.push(Entry {
            path: key,
            state: st,
        });
        if self.entries.len() > CAPACITY {
            let over = self.entries.len() - CAPACITY;
            self.entries.drain(0..over);
        }
    }
}

fn key_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RawHistory {
    #[serde(default)]
    entry: Vec<RawEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawEntry {
    path: String,
    scroll_y: f64,
    zoom: f64,
    text_zoom: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn st(y: f64) -> FileState {
        FileState {
            scroll_y: y,
            zoom: 1.0,
            text_zoom: 1.0,
        }
    }

    #[test]
    fn record_then_get() {
        let mut h = History::default();
        let p = PathBuf::from("/home/u/a.md");
        h.record(&p, st(120.0));
        assert_eq!(h.get(&p), Some(st(120.0)));
        assert_eq!(h.get(Path::new("/home/u/other.md")), None);
    }

    #[test]
    fn record_updates_in_place() {
        let mut h = History::default();
        let p = PathBuf::from("/home/u/a.md");
        h.record(&p, st(10.0));
        h.record(&p, st(50.0));
        assert_eq!(h.get(&p), Some(st(50.0)));
    }

    #[test]
    fn round_trips_through_toml() {
        let mut h = History::default();
        h.record(
            Path::new("/home/u/a.md"),
            FileState {
                scroll_y: 100.5,
                zoom: 1.25,
                text_zoom: 0.5,
            },
        );
        h.record(Path::new("/home/u/b.md"), st(0.0));
        let toml = h.to_toml();
        let loaded = History::load(&toml);
        assert_eq!(
            loaded.get(Path::new("/home/u/a.md")),
            h.get(Path::new("/home/u/a.md"))
        );
        assert_eq!(
            loaded.get(Path::new("/home/u/b.md")),
            h.get(Path::new("/home/u/b.md"))
        );
        // Order (and thus a second round-trip) is stable.
        assert_eq!(loaded.to_toml(), toml);
    }

    #[test]
    fn load_is_lenient_on_garbage() {
        assert!(
            History::load("this is not valid toml {{{")
                .get(Path::new("x"))
                .is_none()
        );
        assert!(History::load("").get(Path::new("x")).is_none());
    }

    #[test]
    fn paths_with_special_chars_round_trip() {
        // Dots, spaces, brackets — a raw TOML key would choke; array-of-tables
        // stores the path as a quoted string value instead.
        let mut h = History::default();
        let weird = Path::new("/home/u/my notes [draft].2024.md");
        h.record(weird, st(42.0));
        let loaded = History::load(&h.to_toml());
        assert_eq!(loaded.get(weird), Some(st(42.0)));
    }

    #[test]
    fn capacity_is_bounded_evicting_oldest() {
        let mut h = History::default();
        for i in 0..600 {
            h.record(&PathBuf::from(format!("/f/{i}.md")), st(i as f64));
        }
        // Oldest evicted, newest retained.
        assert_eq!(h.get(Path::new("/f/0.md")), None);
        assert_eq!(h.get(Path::new("/f/599.md")), Some(st(599.0)));
        // Exactly CAPACITY survive (100..599 inclusive).
        assert_eq!(h.get(Path::new("/f/100.md")), Some(st(100.0)));
        assert_eq!(h.get(Path::new("/f/99.md")), None);
    }

    #[test]
    fn re_recording_bumps_recency() {
        let mut h = History::default();
        for i in 0..CAPACITY {
            h.record(&PathBuf::from(format!("/f/{i}.md")), st(i as f64));
        }
        // Touch the oldest so it becomes newest, then overflow by one.
        h.record(Path::new("/f/0.md"), st(0.0));
        h.record(Path::new("/f/new.md"), st(1.0));
        // /f/1.md is now the oldest and gets evicted, not the bumped /f/0.md.
        assert_eq!(h.get(Path::new("/f/1.md")), None);
        assert_eq!(h.get(Path::new("/f/0.md")), Some(st(0.0)));
        assert_eq!(h.get(Path::new("/f/new.md")), Some(st(1.0)));
    }
}
