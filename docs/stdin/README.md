# stdin streaming

`some-tool | jumanji` renders markdown from a pipe and re-renders as more
arrives. The stream reuses the live-reload path, and every feature that needs a
file path degrades in a stated way.

Decisions on this page:

- [D9: stdin streaming (M3)](#d9-stdin-streaming-m3)

Code: [`src/core/source.rs`](../../src/core/source.rs),
[`src/controller/stdin.rs`](../../src/controller/stdin.rs).

## D9: stdin streaming (M3)

The last M3 item: `some-tool | jumanji` renders markdown from a pipe and
progressively re-renders as more arrives. The design reuses the live-reload
machinery wholesale rather than inventing a parallel path.

- **CLI surface.** `jumanji -` reads stdin explicitly; a bare `jumanji` with a
  piped (non-terminal) stdin and no file argument does the same (detected with
  `std::io::IsTerminal`). A bare `jumanji` at an interactive prompt has nothing
  to read and errors with a clap usage message. The file/stdin classification is
  a pure, unit-tested core type (`core::source::Source::resolve(file, is_tty)`);
  the isatty read (shell I/O) is injected, so the matrix stays testable without a
  terminal.
- **Reader — a shell thread, not core.** `shell::stdin::StdinReader` spawns a
  background thread that reads stdin into a growing `Vec<u8>` and posts ticks
  down an `mpsc` channel; a `glib::timeout_add_local` poll (120 ms, matching
  [`watch.rs`](../../src/controller/watch.rs)'s poll cadence) drains a burst of ticks and re-renders **once** —
  the same batch-then-poll coalescing the notify debouncer gives live reload. The
  render path is `render_and_load(preserve_scroll = true)`, identical to a file
  edit, so **scroll position is preserved across streaming re-renders** by the
  existing anchor/`pending_restore` mechanism. EOF is not an error: the thread
  sends one final tick (so the last bytes render) and exits; an
  already-closed stdin (`echo x | jumanji -`) renders once and settles. Content
  is decoded per render with `from_utf8_lossy`, so a chunk boundary splitting a
  multibyte char shows a transient replacement char that self-corrects on the
  next chunk. The thread/IO plumbing is shell only — the core stays pure.
- **What a stream degrades sensibly (each interaction that assumes a path).**
  - *live-reload watcher* — skipped; there is no file to watch (the stdin reader
    replaces it).
  - *per-file history* — skipped (zathura does not persist stdin documents
    either); a stream has no stable identity to key `history.toml` on.
  - *statusbar / `GetState` file* — the label is `stdin` (and so is the stream's
    breadcrumb segment); `GetState.file` reports `stdin` too, which keeps the
    D-Bus forward-search ([D7](../editor-sync/README.md#d7-editor-pairing--the-synctex-analogue-built), matches on that field) from ever mistaking a
    stream for a file.
  - *reverse editor sync (`%f`)* — suppressed with a statusbar notice (no file to
    open the editor at). `--forward` for a stream is rejected in the CLI up front
    (it targets a saved source line and can hand off to an instance holding that
    file — meaningless for a pipe).
  - *relative links/images* — resolved against the **current directory** (what a
    pipe user expects): the document base is a `<cwd>/stdin.md` sentinel, so
    document-relative `img/x.png` and `.md` links resolve under the CWD exactly as
    they would for a file there. The sentinel is never read or written.
  - TOC, math, mermaid, external fence renderers, search, and marks-in-session
    all operate on the rendered pipeline output, so they work on stream content
    unchanged.
- **Not built:** persisting a stream to disk, or an `:reload` that re-reads a
  (consumed) stdin — a pipe is single-shot by nature. Opening a real file from a
  stream (`:open`, a link click) ends the stream and switches to a normal file
  document (watcher, history, editor sync all resume).
- **Graceful degradation (binding):** a parser error (pulldown-latex emits an
  inline `<merror>`) or an unbalanced group/environment (which *panics* inside
  pulldown-latex's writer, contained by `catch_unwind`) degrades to the raw
  source shown as a code span (inline) or a small error box (display) with a
  note — never a crash, never a blank page. Mirrors [`diagram.rs`](../../src/core/diagram.rs).
