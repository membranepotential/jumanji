# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline, modal vim/zathura keys, Obsidian dialect. Linux-first.

## Now

Shipped through **v1.8.0** (tag + GitHub release + AUR PKGBUILD pointed at it).

- **macOS port (issue #1)** — `docs/research/05-macos-port.md`; extraction
  first, tier-2 cfg-gated mac shell. Owner side now: mac scaffold branch.
- **perf guard** — `.github/workflows/{ci,bench}.yml` (Arch container) and
  `scripts/bench-compare.sh`. State: **e2e proven in CI (53/53)**; trail
  is a workflow artifact chain (no branch, no Pages). Next: confirm the first
  artifact-trail run.

## Done (recent)

- **wide blocks + merman 0.8 (2026-09-13, uncommitted)** — diagrams, fences and
  tables break out of the reading column to window width (DESIGN D5a.1); `s`
  toggles by flipping one `<html>` class, no re-render. merman 0.7 → 0.8.0-
  alpha.6 fixes a lexer parity bug that ate diagrams whose quoted `subgraph`
  title held a `;` or `]`; parse errors now read as `line:col`. 438 tests green;
  the e2e goes red without the breakout CSS. Perf vs v1.8.0: `mermaid` −47 %,
  `demo` −46 %, everything else at or under baseline (a lazily-built renderer
  removed a ~70 k-instruction fixed cost the bench caught).
- **diagram fit + zoom (2026-09-13, uncommitted)** — `a` cycles fit-to-width /
  intrinsic; Ctrl+wheel over a diagram scales that diagram, not the page
  (DESIGN D5a.2), routed shell-side off a cached hover flag because GTK takes
  the scroll before WebKit (D4). Stripping merman's inline `max-width` at the
  source got the stylesheet back to zero `!important`.
- **controller extraction (2026-09-02..12)** — session → `controller::session`,
  GTK shell → `shell/gtk/` (wiring only), fake toolkit + 51 display-free tests;
  DESIGN D2a/D13, CLAUDE, TESTING, README reconciled. 50/50 e2e, A/B flat.
- **e2e restore-gate hole closed (2026-08-12)** — growing fixture plus
  `reveal_scroll_y` / `reveal_failsafe`; tests go red without the gate.
- **Perf pass (2026-08-12)** — criterion benches + headless startup timing via
  a lib/bin split; parallel syntect, mermaid renderer reuse, startup double
  load fixed, native-scroll statusbar updates.
- **v1.7.0** — session-scoped zoom (DESIGN D5a); document-switch flash gone.
- **v1.6.0** — opening position rides into the load (D12); `--background`.
- **v1.2 – v1.5** — breadcrumb, `:` completion, Obsidian dialect, Neovim sync.

## Next

1. **Mac shell** — Torsten's, on branch `mac-support`, after their spike;
   `05-macos-port.md` §5.3 is the guide. Owner side: review PRs, keep CI green.
2. Trim the e2e suite toward what needs a real engine (unit tests now cover
   the flows in ms).
3. Delete `.flash-investigation/` (gitignored) — bug closed, released, and the
   suite now defends the fix.

## Open questions

- Editor pairing on macOS (D7 rides on D-Bus): a unix-socket transport, or
  a stated gap? `05-macos-port.md` §6. The other §6 decisions are made.
