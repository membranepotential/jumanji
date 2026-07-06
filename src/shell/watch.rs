//! Live reload. Editors rename-replace files on save, so we watch the parent
//! *directory* (not the inode), debounce, filter to our file, and marshal the
//! result onto the GTK main loop.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use gtk::glib;
use notify::RecursiveMode;
use notify_debouncer_full::{Debouncer, RecommendedCache, new_debouncer};

/// What happened to the watched file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEvent {
    /// The file was created or modified — re-render.
    Changed,
    /// The file disappeared — keep the last render, note it.
    Removed,
}

/// Owns the debouncer and the main-loop poll source; dropping it stops both.
pub struct Watch {
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    poll: Option<glib::SourceId>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(id) = self.poll.take() {
            id.remove();
        }
    }
}

impl Watch {
    /// Start watching `file`'s parent directory. `on_event` runs on the GTK
    /// main thread whenever a debounced change touches `file`.
    pub fn start<F>(file: &Path, on_event: F) -> anyhow::Result<Self>
    where
        F: Fn(FileEvent) + 'static,
    {
        let file = file.to_path_buf();
        let parent = file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let (tx, rx) = mpsc::channel();
        let mut debouncer =
            new_debouncer(Duration::from_millis(150), None, tx).context("create file debouncer")?;
        debouncer
            .watch(&parent, RecursiveMode::NonRecursive)
            .with_context(|| format!("watch {}", parent.display()))?;

        // Poll the channel from the main loop; the debouncer already coalesces,
        // so a coarse tick adds no perceptible latency.
        let target_name = file.file_name().map(|n| n.to_owned());
        let poll = glib::timeout_add_local(Duration::from_millis(120), move || {
            let mut touched = false;
            while let Ok(result) = rx.try_recv() {
                if let Ok(events) = result {
                    for event in events {
                        let hits = event.paths.iter().any(|p| {
                            *p == file || p.file_name().map(|n| n.to_owned()) == target_name
                        });
                        touched |= hits;
                    }
                }
            }
            if touched {
                let event = if file.exists() {
                    FileEvent::Changed
                } else {
                    FileEvent::Removed
                };
                on_event(event);
            }
            glib::ControlFlow::Continue
        });

        Ok(Self {
            _debouncer: debouncer,
            poll: Some(poll),
        })
    }
}
