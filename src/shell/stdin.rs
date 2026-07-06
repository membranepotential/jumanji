//! Stdin streaming. A background thread reads standard input into a growing
//! byte buffer and posts "content changed" ticks to the GTK main loop, which
//! coalesces a burst of chunks into one re-render — the same batch-then-poll
//! shape the live-reload watcher uses (see `watch.rs`).
//!
//! EOF simply stops the updates: the thread sends a final tick (so the last
//! bytes are rendered) and exits; there is no error state. Reading `jumanji -`
//! from an already-closed stdin (`echo x | jumanji -`) therefore renders once
//! and settles.

use std::io::Read;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::glib;

/// Main-loop poll cadence, matching `watch.rs`'s poll tick: the debouncer there
/// coalesces to ~150 ms, this coarse poll to ~120 ms. Either way a physical
/// burst of chunks collapses into a single re-render.
const POLL: Duration = Duration::from_millis(120);

/// Owns the growing buffer and the main-loop poll source; dropping it removes
/// the poll (and, once the reader's next `send` fails, ends the reader thread).
pub struct StdinReader {
    buffer: Arc<Mutex<Vec<u8>>>,
    poll: Option<glib::SourceId>,
}

impl Drop for StdinReader {
    fn drop(&mut self) {
        if let Some(id) = self.poll.take() {
            id.remove();
        }
    }
}

impl StdinReader {
    /// Spawn the reader thread and install the main-loop poll. `on_change` runs
    /// on the GTK main thread whenever a debounced batch of new bytes (or EOF)
    /// has arrived; the caller re-renders from [`buffer`](Self::buffer).
    pub fn start<F>(on_change: F) -> Self
    where
        F: Fn() + 'static,
    {
        let buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel::<()>();

        let thread_buf = buffer.clone();
        std::thread::Builder::new()
            .name("jumanji-stdin".into())
            .spawn(move || {
                let mut stdin = std::io::stdin().lock();
                let mut chunk = [0u8; 8192];
                loop {
                    match stdin.read(&mut chunk) {
                        // EOF: one final tick so the last render sees all bytes.
                        Ok(0) => {
                            let _ = tx.send(());
                            break;
                        }
                        Ok(n) => {
                            if let Ok(mut b) = thread_buf.lock() {
                                b.extend_from_slice(&chunk[..n]);
                            }
                            // A send error means the poll source was dropped (the
                            // shell switched away from stdin) — stop reading.
                            if tx.send(()).is_err() {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = tx.send(());
                            break;
                        }
                    }
                }
            })
            .expect("spawn stdin reader thread");

        // Drain every tick that arrived since the last poll and re-render once —
        // the coalescing step. The channel stays empty (and this is a cheap
        // no-op) after EOF, mirroring how `watch.rs` keeps its poll alive for the
        // window's lifetime rather than tearing it down.
        let poll = glib::timeout_add_local(POLL, move || {
            let mut any = false;
            while rx.try_recv().is_ok() {
                any = true;
            }
            if any {
                on_change();
            }
            glib::ControlFlow::Continue
        });

        Self {
            buffer,
            poll: Some(poll),
        }
    }

    /// The shared buffer the reader appends to; the shell snapshots and
    /// lossily-decodes it (a chunk boundary may split a multibyte char, which
    /// self-corrects on the next chunk) at render time.
    pub fn buffer(&self) -> Arc<Mutex<Vec<u8>>> {
        self.buffer.clone()
    }
}
