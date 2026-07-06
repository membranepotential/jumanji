//! Live reload. Editors rename-replace files on save, so we watch the parent
//! *directory* (not the inode), debounce, filter to our file, and marshal the
//! result onto the GTK main loop.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use gtk::glib;
use notify::{EventKind, RecursiveMode};
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

        // Only events touching our specific file matter; everything else in the
        // directory (sibling saves, editor swap files) is filtered out.
        let target_name = file.file_name().map(|n| n.to_owned());
        Self::start_impl(
            &parent,
            move |event| {
                let hits = event
                    .paths
                    .iter()
                    .any(|p| *p == file || p.file_name().map(|n| n.to_owned()) == target_name);
                if !hits {
                    return None;
                }
                Some(if file.exists() {
                    FileEvent::Changed
                } else {
                    FileEvent::Removed
                })
            },
            on_event,
        )
    }

    /// Start watching a whole directory (non-recursive). Any content-mutating
    /// event inside it fires [`FileEvent::Changed`] — used for the user-CSS
    /// themes directory, where any `.css` add/edit/remove should re-render.
    pub fn start_dir<F>(dir: &Path, on_event: F) -> anyhow::Result<Self>
    where
        F: Fn(FileEvent) + 'static,
    {
        Self::start_impl(dir, |_event| Some(FileEvent::Changed), on_event)
    }

    /// Watch `watch_path` (non-recursive), debounce, and marshal each debounced
    /// batch onto the GTK main loop. `classify` maps a raw debounced event to an
    /// optional [`FileEvent`]; `None` drops it. Only content-mutating event
    /// kinds reach `classify` (`Access` events fire on every read — including our
    /// own reload's — and would otherwise feed a self-sustaining reload loop).
    fn start_impl<C, F>(watch_path: &Path, classify: C, on_event: F) -> anyhow::Result<Self>
    where
        C: Fn(&notify::Event) -> Option<FileEvent> + 'static,
        F: Fn(FileEvent) + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let mut debouncer =
            new_debouncer(Duration::from_millis(150), None, tx).context("create file debouncer")?;
        debouncer
            .watch(watch_path, RecursiveMode::NonRecursive)
            .with_context(|| format!("watch {}", watch_path.display()))?;

        // Poll the channel from the main loop; the debouncer already coalesces,
        // so a coarse tick adds no perceptible latency.
        let poll = glib::timeout_add_local(Duration::from_millis(120), move || {
            let mut fired: Option<FileEvent> = None;
            while let Ok(result) = rx.try_recv() {
                if let Ok(events) = result {
                    for event in events {
                        if !matches!(
                            event.kind,
                            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                        ) {
                            continue;
                        }
                        if let Some(ev) = classify(&event) {
                            // A `Removed` wins over a `Changed` in the same batch.
                            fired = Some(match (fired, ev) {
                                (Some(FileEvent::Removed), _) | (_, FileEvent::Removed) => {
                                    FileEvent::Removed
                                }
                                _ => FileEvent::Changed,
                            });
                        }
                    }
                }
            }
            if let Some(ev) = fired {
                on_event(ev);
            }
            glib::ControlFlow::Continue
        });

        Ok(Self {
            _debouncer: debouncer,
            poll: Some(poll),
        })
    }
}
