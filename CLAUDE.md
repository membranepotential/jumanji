# CLAUDE.md

jumanji — a zathura-inspired markdown reader. Rust, GTK4 + system WebKitGTK 6,
100%-Rust content pipeline (no JavaScript). Linux-first (Arch, X11/i3wm).

## Read first

- @STATUS.md — the live project dashboard: what is in flight, what just
  landed, what is queued. Imported into every session by that `@`; keep it
  true (rules below).
- `docs/DESIGN.md` — the architecture decision record. **Binding.** Deviations
  from it require updating it in the same change, with reasoning.
- `docs/DEVLOG.md` — running log. Append an entry (newest first) for every
  substantial change: what, why, what's next.
- `docs/research/` — the research the design rests on; cite it, don't re-argue
  it without new evidence.

## Project dashboard (STATUS.md)

`STATUS.md` at the repo root is the shared dashboard: Goal / Now / Done /
Next / Open questions, hard cap 60 lines. Keep it true:

- Register every workstream in **Now** before starting it (one line: name —
  objective — state — next); move it to **Done** when it lands.
- Update Done/Next at phase boundaries and before reporting work complete.
  Prune rather than append; history lives in git and `docs/DEVLOG.md`.
- `/orient` (a project skill in `.claude/skills/orient/`) reconciles the file
  against git and prints a brief. Run it when you sit down and before you
  hand off.

## Build & run

```sh
cargo build                 # needs system gtk4 + webkitgtk-6.0
cargo test                  # core unit tests + headless e2e (tests/e2e.rs)
cargo test --test e2e       # just the e2e suite (real Xvfb + WebKit + D-Bus)
cargo clippy -- -D warnings
cargo run -- demo/demo.md
scripts/bench-compare.sh    # perf A/B: latest tag vs. the tree (see docs/TESTING.md)
```

CI (`.github/workflows/`): `ci.yml` runs fmt, clippy, unit tests and the e2e
suite in an Arch container on every push/PR; `bench.yml` runs the benches —
the pipeline instruction count gates (fails at 105 %), wall clock informs.
Results and the trail are workflow artifacts; `docs/TESTING.md` says how to
get them.

The e2e suite drives the real app under a virtual X server and asserts via the
D-Bus interface; it needs `xorg-server-xvfb`, `xdotool`, and `dbus`
(`pacman -S xorg-server-xvfb xdotool dbus`) and **skips cleanly** (passes as a
no-op) when they're absent. See `docs/TESTING.md`.

## Architecture (enforced boundaries)

Three layers (DESIGN D2a). Dependencies point one way: shell → controller →
core, never back.

- `src/core/` — **pure, no toolkit imports, unit-tested.** Markdown → HTML
  pipeline (comrak AST transform; syntect highlighting; merman mermaid →
  inline SVG), TOC extraction, config parsing, keymap lookup (`mode × count ×
  key-seq → Action`), the session models (jumplist, marks, history, vault).
  Everything here must be testable without a display.
- `src/controller/` — **the toolkit-agnostic imperative half, no `gtk` /
  `glib` / `gio` / `webkit6` / `javascriptcore` imports** (grep before you
  commit). `Controller<T: Toolkit>` owns the session state and every flow and
  drives the window only through the traits in `toolkit.rs` (`Viewport`,
  `Chrome`, `Host`). Viewport *behaviour* is JS the controller owns
  (`page.rs`, `scripts.rs`) run through `Viewport::eval`, so it is identical
  on every toolkit; the scripts post back via `window.__jmnj_post`, never
  `webkit.messageHandlers` directly. Unit-tested against a fake toolkit.
- `src/shell/gtk/` — the Linux shell: GTK4 widgets, the webkit6 view as
  `Viewport`, `GtkChrome`, `GlibHost`, GTK event adapters, D-Bus. Wiring
  only; no session logic. Native find, PRIMARY selection and D-Bus are
  GTK-only by design. A second shell is a sibling directory under
  `cfg(target_os = …)`, never a branch in the controller.
- New features start with types in core, then a controller flow, then (only
  if a native capability is needed) a trait method — which every shell and
  the fake must then implement.

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

## Releasing

Every version bump ships as a git tag **and** a matching GitHub release —
the tag alone is not a release. Steps:

1. Bump `version` in `Cargo.toml` (patch for fixes, minor for features);
   `cargo build` to update `Cargo.lock`. Commit as `chore: release vX.Y.Z`.
2. Tag `vX.Y.Z` and push the commit + tag.
3. **Create the GitHub release with notes**: `gh release create vX.Y.Z --title
   … --notes …`. Never skip this — a tag with no `gh release` is an incomplete
   release. Write real notes (what changed, user-facing), not a bare version.
4. Point the AUR `packaging/aur/PKGBUILD` at the new tag: set `pkgver` and
   replace `sha256sums` with the sha256 of the pushed tag tarball
   (`https://github.com/membranepotential/jumanji/archive/refs/tags/vX.Y.Z.tar.gz`).
   Commit as `chore: point PKGBUILD at vX.Y.Z`.
