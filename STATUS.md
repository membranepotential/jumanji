# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline, modal vim/zathura keys, Obsidian dialect. Linux-first.

## Now

Shipped through **v1.13.0** (`--no-focus`).

- **merman alpha.6 → 0.9.0** — not started — next: tests, output, bench vs v1.13.0.
- **macOS port (issue #1)** — [05-macos-port.md](docs/research/05-macos-port.md);
  tier-2 cfg-gated mac shell. Owner side now: mac scaffold branch.
- **perf guard** — `.github/workflows/{ci,bench}.yml` + `scripts/bench-compare.sh`;
  e2e green in CI, trail is a workflow-artifact chain. Next: confirm the trail.
- **diagram text measurer** — merman's only measurer uses fixed per-character
  widths, 10–30 % short of real fonts (DEVLOG 2026-09-28). Decided: bundle an
  open font + its width table, a `TextMeasurer` over it, diagram CSS on it.
  State: not started.

## Done (recent)

- **v1.13.0 (2026-10-19)** — `--no-focus`: `_NET_WM_USER_TIME` = 0 on realize,
  `set_visible` instead of `present()`; for the jumanji skill's wrapper.
- **v1.12.1 (2026-09-28)** — classDiagram labels overflow instead of clipping
  (CSS `overflow: visible`); seasoned dependency patch updates.
- **selection + search colours (2026-09-21, 686b357..d8f2557)** — zathura's
  `highlight-color`/`highlight-active-color`; search is controller JS painted
  with CSS highlights (no FindController); selection no longer spans the window.
  Open: SVG `<text>` matches are found but not painted.
- **completion, TOC flash, theme (2026-09-21, 6ce0ace..b47bbf1)** — multi-word
  Tab completion + Shift-Tab; TOC is an overlay (no stale graph frame);
  `resources/theme.example.css` (the built-in colours).
- **v1.10.0 (2026-09-19)** — the document graph; after it: zoom clamp, peek
  drawn as scene nodes, arrow keys + Tab, view line, mermaid docs test.
- **pre-release review fixes (2026-09-19, 31f895f, 327269f)** — Astra review
  of v1.9.1..HEAD: reading anchor holds a screen point (bursts, resize, wide
  blocks, jumps); graph counts, races, edges, symlinks, Esc layering.
- **pointer zoom (2026-09-19, 82594ca)** — document + diagram Ctrl+wheel
  anchor at the pointer the page tracks itself (the shell's px were off by
  WebKit's own scale); shell motion wiring removed; e2e red without it.
- **document graph v2 (2026-09-19, e4aa0b2)** — [interaction model](docs/graph/interaction.md):
  one node, one handle, `Space` folds, peek, route-first framing;
  [docs/graph/](docs/graph/README.md) with screenshots + demo tree.

## Next

1. **Mac shell** — Torsten's, on branch `mac-support`, after their spike;
   `05-macos-port.md` §5.3 is the guide. Owner side: review PRs, keep CI green.
2. Trim the e2e suite toward what needs a real engine (unit tests now cover
   the flows in ms).
3. Delete `.flash-investigation/` (gitignored).

## Open questions

- Editor pairing on macOS (D7 rides on D-Bus): a unix-socket transport, or
  a stated gap? `05-macos-port.md` §6. The other §6 decisions are made.
