# Development log

Newest entries first. Each entry: what happened, what was decided, what's next.

## 2026-07-06 — M1 MVP: it renders, it scrolls, it recolors

Core pipeline and GTK shell implemented (in parallel, by two agents with
disjoint file ownership) and integrated on the first build: **46/46 tests,
clippy `-D warnings` clean.**

- `core/`: comrak (GFM + footnotes + header IDs) → AST passes (mermaid via
  merman → inline SVG, syntect classed highlighting, table wrapping) → complete
  HTML document with embedded CSS. TOC anchors are byte-identical to emitted
  ids (comrak's own `Anchorizer`). Light theme InspiredGitHub, dark Base16
  Ocean Dark under `html.dark`.
- `shell/`: capture-phase `EventControllerKey` → pure `Matcher`
  (counts, `gg`, `<N>G`), actions executed via webkit6 (`scrollBy` JS, zoom
  level, FindController search, `classList.toggle('dark')` recolor), girara
  bar (filename · pending keys · percent), notify-based live reload with
  scroll restore.
- Gotcha found at integration: merman's `render` cargo feature is required
  for `merman::render` to exist (default features are empty).
- Verified live on X11 (screenshots): typography, tables, syntect fences,
  both mermaid diagrams, footnotes, `j`/`G`/`Ctrl-r` all behave; statusbar
  percent honest (queries actual `scrollY`).
- Known gaps: keys are silent no-ops until the initial load finishes
  (sub-second); section tracking (`J`/`K`) steps the TOC list rather than
  following the viewport; ToC mode and `:` commands are M2.

**Next:** headless e2e testing (Xvfb + D-Bus state interface — doubles as the
M3 editor-sync foundation).

## 2026-07-06 — Project inception

**Research.** Three parallel research passes (full write-ups in
[research/](research/)):

1. [Landscape survey](research/01-landscape.md) — no "zathura for markdown"
   exists; inlyne is closest but keyboard-less and mermaid-less; the "missing
   5%" (mermaid, math, callouts) is why users switch tools.
2. [Zathura architecture & UX](research/02-zathura.md) — girara's inputbar/
   statusbar/dispatch design, the zathurarc idioms, the full keybinding table,
   and the SyncTeX editor-pairing pattern.
3. [Rust stack evaluation](research/03-rust-stack.md) — webview vs native,
   the 2026 pure-Rust mermaid breakthrough (merman), parser/highlighter
   comparison.

**Girara-as-framework considered and rejected** (user suggestion, investigated
seriously): girara's GTK parts were stripped upstream in Feb 2026 and absorbed
into zathura as an internal static lib — no installable headers, no Rust
bindings, no introspection. We reimplement the small girara subset in Rust
instead, with zathura's `girara-gtk/` as design reference.

**Architecture decided** (full record in [DESIGN.md](DESIGN.md)):

- Rust · gtk4-rs + system WebKitGTK 6 (webview for typesetting only)
- 100%-Rust content pipeline: comrak (GFM) + syntect/two-face (highlighting)
  + merman (pure-Rust mermaid → inline SVG). **No JavaScript pipeline.**
- Capture-phase GTK key controller → girara-style mode/count dispatch
- TOML config with zathura idioms; notify-based live reload
- License: MIT. Repo: github.com/membranepotential/jumanji

**Next:** cargo scaffold, then parallel implementation of core pipeline and
GTK shell (Opus subagents), integration, first running MVP.
