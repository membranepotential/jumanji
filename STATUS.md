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

- **position held across window resize (2026-09-18)** — fullscreen
  / re-tile no longer moves the reader; `resize_anchor_js` (D5a.0), e2e red
  without it.
- **wide blocks + merman 0.8 (v1.9.0, 2026-09-13)** — diagrams/fences/tables
  break out to window width (D5a.1), `s` toggles. merman 0.8 fixes a lexer
  parity bug that ate diagrams with `;`/`]` in a quoted `subgraph` title;
  errors read as `line:col`. Perf vs v1.8.0: `mermaid` −47 %, `demo` −46 %,
  rest at or under baseline (a lazily-built renderer removed a ~70 k fixed
  cost the bench caught).
- **reading position held across toggles (2026-09-13)** — v1.9.0's `s` and `a`
  moved the document under the reader: a class flip does not re-render but does
  change block *heights*, so content after them shifts. Both now use D5a's
  anchor (DESIGN D5a.0); `GetState` gained `probe_text`/`probe_top` and two e2e
  tests that go red without it. Also fixed the v1.9.0 CI e2e failure (fixture
  diagram too narrow to overflow a 1x container's box).
- **diagram fit + zoom (v1.9.0)** — `a` cycles fit-to-width/intrinsic;
  Ctrl+wheel scales one diagram, not the page (D5a.2), routed shell-side off a
  cached hover flag (GTK takes the scroll before WebKit, D4).
- **controller extraction (2026-09-02..12)** — session → `controller::session`,
  GTK shell → `shell/gtk/` (wiring only), fake toolkit + 51 display-free tests;
  DESIGN D2a/D13, CLAUDE, TESTING, README reconciled. 50/50 e2e, A/B flat.
- **e2e restore-gate + perf pass (2026-08-12)** — `reveal_scroll_y` /
  `reveal_failsafe` go red without the gate; criterion benches, parallel
  syntect, startup double load fixed.
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
