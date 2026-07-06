# CLAUDE.md

jumanji — a zathura-inspired markdown reader. Rust, GTK4 + system WebKitGTK 6,
100%-Rust content pipeline (no JavaScript). Linux-first (Arch, X11/i3wm).

## Read first

- `docs/DESIGN.md` — the architecture decision record. **Binding.** Deviations
  from it require updating it in the same change, with reasoning.
- `docs/DEVLOG.md` — running log. Append an entry (newest first) for every
  substantial change: what, why, what's next.
- `docs/research/` — the research the design rests on; cite it, don't re-argue
  it without new evidence.

## Build & run

```sh
cargo build                 # needs system gtk4 + webkitgtk-6.0
cargo test                  # core unit tests + headless e2e (tests/e2e.rs)
cargo test --test e2e       # just the e2e suite (real Xvfb + WebKit + D-Bus)
cargo clippy -- -D warnings
cargo run -- demo/demo.md
```

The e2e suite drives the real app under a virtual X server and asserts via the
D-Bus interface; it needs `xorg-server-xvfb`, `xdotool`, and `dbus`
(`pacman -S xorg-server-xvfb xdotool dbus`) and **skips cleanly** (passes as a
no-op) when they're absent. See `docs/TESTING.md`.

## Architecture (enforced boundaries)

- `src/core/` — **pure, no GTK imports, unit-tested.** Markdown → HTML pipeline
  (comrak AST transform; syntect highlighting; merman mermaid → inline SVG),
  TOC extraction, config parsing, keymap lookup (`mode × count × key-seq →
  Action`). Everything here must be testable without a display.
- `src/shell/` — imperative GTK layer. Window, webkit6 WebView + `app://` URI
  scheme, capture-phase `EventControllerKey`, statusbar/inputbar, notify-based
  live reload. As thin as possible; logic lives in core.
- The core must never depend on the shell. New features start with types in
  core.

## Conventions

- Fully typed: model states with enums/ADTs (e.g. `Action`, `Mode`,
  `KeySequence`), no stringly-typed dispatch. Illegal states unrepresentable.
- Functional core / imperative shell — keep the boundary honest.
- No accidental complexity: no wrapper that adds nothing, no premature
  generalization, no config option with one caller.
- Rendering failures degrade gracefully (a broken mermaid fence renders as a
  highlighted code block + error note, never a crash or blank page).
- Zathura semantics are the spec for UX questions: check
  `docs/research/02-zathura.md` before inventing behavior.
- Keep count-prefix handling generic in the dispatcher — never per-binding.
- No network access anywhere. CSP locks the webview; nothing else may do I/O
  beyond the local filesystem.

## Committing

- Small, focused commits with conventional-commit style messages
  (`feat:`, `fix:`, `docs:`, `refactor:`).
- Before committing: `cargo test && cargo clippy -- -D warnings && cargo fmt`.
- Update `docs/DEVLOG.md` alongside non-trivial changes.
