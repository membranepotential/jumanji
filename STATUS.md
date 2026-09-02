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
  State: **stage 1 landing** (scripts + `__jmnj_post`), sequential implementer
  agents, e2e green per stage. Next: stage 2 (traits + GTK impls).

## Done (recent)

- **e2e restore-gate hole closed (2026-08-12)** — a growing fixture (CSS
  `height` animation, CSP-legal) plus `reveal_scroll_y` / `reveal_failsafe`;
  two tests that go red when `STABLE_FRAMES` is weakened to pre-v1.7.0.
- **Perf pass (2026-08-12)** — criterion benches + headless startup timing via
  a lib/bin split; parallel syntect, mermaid renderer reuse, startup double
  load fixed, native-scroll statusbar updates.
- **v1.7.0** — zoom is a session setting that carries across navigation
  (history's per-file value demoted to *default on open*, DESIGN D5a); the
  document-switch flash is gone — the restore gate now concedes only once
  `scrollHeight` holds steady, not one frame after `readyState === 'complete'`.
  User-confirmed on the repro vault.
- **v1.6.0** — reading position rides into the load as `data-jmnj-open`
  (DESIGN D12); text zoom inlined the same way; `--background`.
- **v1.2 – v1.5** — jumplist breadcrumb, `:` completion, Obsidian dialect,
  Neovim sync plugin, 960px column, NON_UNIQUE GApplication.

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

- The §6 decisions in `docs/research/05-macos-port.md`: traits vs type
  alias, `cfg(target_os)` vs feature, Arch container vs Ubuntu for CI, and
  whether editor pairing gets a macOS transport at all.
