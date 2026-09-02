# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline, modal vim/zathura keys, Obsidian dialect. Linux-first.

## Now

Shipped through **v1.8.0** (tag + GitHub release + AUR PKGBUILD pointed at it).

- **macOS port (issue #1)** — evaluated 2026-09-02 in
  `docs/research/05-macos-port.md`: feasible, sensible under conditions
  (extraction first, tier-2 cfg-gated mac shell, parity gaps named). Owner-side
  work is three items, in order: CI workflows → controller extraction →
  mac scaffold branch. Decided 2026-09-02: extraction first, traits + generic
  `Session`, Arch container for CI, `cfg(target_os)` for the mac shell.
- **controller-extraction** (on `main`, main checkout) — stage the §5.2
  plan: scripts seam → traits → session move → fake toolkit tests → docs.
  State: **stages 1–2 landed** (`21fae85`, `7393903`; reviewed, 50/50 e2e).
  A/B vs v1.8.0 running. Next: stage 3 (session move) once the A/B is clean.
- **perf guard** — `.github/workflows/{ci,bench}.yml` (Arch container; fmt/
  clippy/unit, e2e, criterion + startup trail on `gh-pages`, alerts on PRs)
  and `scripts/bench-compare.sh` (local A/B of a ref vs. the tree). State:
  **e2e proven in CI (50/50 in an Arch container)**; trail carried as a
  workflow artifact chain (no branch, no Pages — the Pages experiment is
  torn down). `bench-compare.sh` interleaves both halves; local A/B vs
  v1.8.0 flat at startup. Next: confirm the first artifact-trail run.

## Done (recent)

- **e2e restore-gate hole closed (2026-08-12)** — growing fixture plus
  `reveal_scroll_y` / `reveal_failsafe`; tests go red without the gate.
- **Perf pass (2026-08-12)** — criterion benches + headless startup timing via
  a lib/bin split; parallel syntect, mermaid renderer reuse, startup double
  load fixed, native-scroll statusbar updates.
- **v1.7.0** — session-scoped zoom (DESIGN D5a); document-switch flash gone
  (restore gate waits for a steady `scrollHeight`). User-confirmed.
- **v1.6.0** — opening position rides into the load (D12); `--background`.
- **v1.2 – v1.5** — breadcrumb, `:` completion, Obsidian dialect, Neovim sync.

## Next

1. **CI** — `.github/workflows/ci.yml`: check (fmt/clippy/unit), e2e (Xvfb,
   Arch container recommended), bench (criterion + startup, artifact + job
   summary, no gate). Plan in `05-macos-port.md` §5.1.
2. **Controller extraction** — `src/controller/` (session, `Viewport`/`Chrome`/
   `Scheduler` traits, shared user scripts via `__jmnj_post`); GTK shell becomes
   the adapter. Staged, each step green on the 50 e2e. §5.2.
3. **Mac scaffold** — branch `mac-support` (the contributor's branch; all
   else lands on `main`), after 2 and after the spike. §5.3.
4. Delete `.flash-investigation/` (gitignored) — bug closed, released, and the
   suite now defends the fix.

## Open questions

- Editor pairing on macOS (D7 rides on D-Bus): a unix-socket transport, or
  a stated gap? `05-macos-port.md` §6. The other §6 decisions are made.
