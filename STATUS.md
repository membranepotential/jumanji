# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline (no JavaScript), modal vim/zathura keybindings, Obsidian
dialect support. Linux-first (Arch, X11/i3wm).

## Now

Shipped through **v1.7.0** (tag + GitHub release + AUR PKGBUILD pointed at it).

No workstream in flight. The perf pass and the e2e restore-gate fixture are
landed and unreleased on `main` — the next release ships both (minor bump:
benches + perf are feature-ish).

## Done (recent)

- **e2e restore-gate hole closed (2026-08-12)** — a temp-dir fixture that keeps
  growing ~200 ms after `readyState === 'complete'` (CSS `height` animation; no
  JS, CSP-legal) plus `reveal_scroll_y` / `reveal_failsafe`, the offset the body
  was *unhidden* at and why. Two tests, one per caller (live reload, `Ctrl-o`
  back into a deep offset); both self-check the fixture, and both go red at
  ~34% of target when `STABLE_FRAMES` is weakened to the pre-v1.7.0 behaviour.
- **Perf pass (2026-08-12)** — criterion benches (`benches/pipeline.rs`) +
  headless startup timing (`scripts/bench-startup.sh`) via a lib/bin split;
  parallel syntect highlighting (code_heavy 44.7→8.3 ms), mermaid renderer
  reuse, the startup double load fixed (initial render deferred ≤250 ms until
  the vault index lands), native-scroll statusbar updates with zero JS evals.
- **v1.7.0** — zoom is a session setting that carries across navigation
  (history's per-file value demoted to *default on open*, DESIGN D5a); the
  document-switch flash is gone — the restore gate now concedes only once
  `scrollHeight` holds steady, not one frame after `readyState === 'complete'`.
  User-confirmed on the repro vault.
- **v1.6.0** — reading position rides into the load as `data-jmnj-open`
  (`InitialPosition` = `Top | Offset | Anchor | SourceLine`, DESIGN D12); text
  zoom inlined the same way; `background = true` / `--background`. Frame-capture
  harness + `first_frame_scroll_y` e2e observable.
- **Build speed (2026-08-07)** — mold linker via `.cargo/config.toml` + dev
  `debug = "line-tables-only"`: incremental rebuild 5.3s → 1.0s. `mold` is now
  an AUR makedepends.
- **v1.5.0 / v1.4.0 / v1.3.0 / v1.2.x** — jumplist breadcrumb, paginated `:`
  completion; Obsidian dialect (wikilinks, vault resolution, callouts, embeds,
  frontmatter); Neovim sync plugin, 960px column, NON_UNIQUE GApplication.

## Next

1. **Cut the next minor release** — perf pass + the restore-gate e2e are on
   `main` unreleased (tag + GitHub release + AUR PKGBUILD, per CLAUDE.md).
2. Delete `.flash-investigation/` (gitignored) once released — bug closed, and
   the suite now defends the fix.

## Open questions

- ~~Was the restore's two-frame background flash the same thing seen on document
  switch?~~ **Answered 2026-08-07: separate.** The reported flash was the restore
  loop conceding early, revealing at a `scrollTo`-clamped offset (fixed v1.7.0);
  the hide gate's own two-frame `--bg` cost is smaller and stays as designed.
