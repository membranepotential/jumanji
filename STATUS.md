# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline, modal vim/zathura keys, Obsidian dialect. Linux-first.

## Now

Shipped through **v1.14.0** (merman 0.9.1, `diagram-look`, worktree vaults).

- **macOS port (issue #1)** — [05-macos-port.md](docs/research/05-macos-port.md);
  tier-2 cfg-gated mac shell. Owner side now: mac scaffold branch.
- **perf guard** — `.github/workflows/{ci,bench}.yml` + `scripts/bench-compare.sh`;
  e2e green in CI, trail is a workflow-artifact chain. Next: confirm the trail.
- **diagram text measurer** — merman measures fixed per-char widths, 10–30 %
  short (DEVLOG 2026-09-28). Decided: bundled font + width table + a
  `TextMeasurer`. State: not started.

## Done (recent)

- **publishing (2026-10-28, 4119c73..1fe538b)** — `jumanji-publish` + password
  Worker ([D15](docs/publish/README.md)); thesis wiki live on workers.dev.
- **v1.14.0 (2026-10-20)** — merman 0.9.1 without ELK; `diagram-look` classic
  (default, Mermaid 11 colours + sizing) | neo; a `.git` file roots a vault.
  Bench vs v1.13.0: mermaid +6 %, demo +7 % (merman 0.9; accepted).
- **v1.13.0 (2026-10-19)** — `--no-focus`: `_NET_WM_USER_TIME` = 0 on realize,
  `set_visible` instead of `present()`; for the jumanji skill's wrapper.
- **v1.12.1 (2026-09-28)** — classDiagram labels overflow instead of clipping
  (CSS `overflow: visible`); seasoned dependency patch updates.
- **completion, TOC flash, theme (2026-09-21, 6ce0ace..b47bbf1)** — multi-word
  Tab completion + Shift-Tab; TOC is an overlay (no stale graph frame);
  `resources/theme.example.css` (the built-in colours).
- **v1.10.0 (2026-09-19)** — the document graph; after it: zoom clamp, peek
  drawn as scene nodes, arrow keys + Tab, view line, mermaid docs test.
- **document graph v2 (2026-09-19, e4aa0b2)** — [interaction model](docs/graph/interaction.md):
  one node, one handle, `Space` folds, peek, route-first framing;
  [docs/graph/](docs/graph/README.md) with screenshots + demo tree.

## Next

1. **Mac shell** — Torsten's, on branch `mac-support`, after their spike;
   `05-macos-port.md` §5.3 is the guide. Owner side: review PRs, keep CI green.
2. Trim the e2e suite toward what needs a real engine (unit tests now cover
   the flows in ms).
3. Delete `.flash-investigation/` (gitignored).
4. Report upstream to merman: per-mindmap config clone (cose-bilkent vs elk).
5. Search: SVG `<text>` matches are found but not painted.

## Open questions

- Editor pairing on macOS (D7 rides on D-Bus): a unix-socket transport, or
  a stated gap? `05-macos-port.md` §6. The other §6 decisions are made.
