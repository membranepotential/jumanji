# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline, modal vim/zathura keys, Obsidian dialect. Linux-first.

## Now

Shipped through **v1.10.0** (the document graph, pointer-anchored zoom).

- **macOS port (issue #1)** — [05-macos-port.md](docs/research/05-macos-port.md);
  tier-2 cfg-gated mac shell. Owner side now: mac scaffold branch.
- **perf guard** — `.github/workflows/{ci,bench}.yml` + `scripts/bench-compare.sh`;
  e2e green in CI, trail is a workflow-artifact chain. Next: confirm the trail.

## Done (recent)

- **selection colour (2026-09-21, 686b357..6f2de40)** — zathura's yellow-green
  by default, `highlight-color` option, selection no longer spans the window.
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
- **docs split into feature folders (2026-09-19)** — [docs/README.md](docs/README.md)
  indexes every decision; DESIGN.md is an anchor stub.
- **position held across resize, toggles and zoom (2026-09-13..18)** — the
  D5a.0 anchor for `s`, `a`, fullscreen; `probe_text` e2e observables.
- **v1.9.0** — wide blocks (D5a.1), diagram fit + per-diagram zoom (D5a.2),
  merman 0.8 (−46 % on the mermaid bench).
- **controller extraction (2026-09-02..12)** — toolkit-agnostic controller,
  GTK shell as wiring, fake toolkit + display-free tests.
- **v1.2 – v1.8** — breadcrumb, `:` completion, Obsidian dialect, Neovim sync,
  opening position (D12), session zoom (D5a), perf pass.

## Next

1. **Mac shell** — Torsten's, on branch `mac-support`, after their spike;
   `05-macos-port.md` §5.3 is the guide. Owner side: review PRs, keep CI green.
2. Trim the e2e suite toward what needs a real engine (unit tests now cover
   the flows in ms).
3. Delete `.flash-investigation/` (gitignored).

## Open questions

- Editor pairing on macOS (D7 rides on D-Bus): a unix-socket transport, or
  a stated gap? `05-macos-port.md` §6. The other §6 decisions are made.
