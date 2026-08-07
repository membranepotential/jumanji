# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline (no JavaScript), modal vim/zathura keybindings, Obsidian
dialect support. Linux-first (Arch, X11/i3wm).

## Now

Shipped **v1.6.0** (tag + GitHub release + AUR PKGBUILD pointed at it).

**Awaiting user verification of the flash fix** (`8559b49`). Diagnosed by
tracing an instrumented build against the user's own repro vault; investigation
artifacts in `.flash-investigation/` (gitignored, `HANDOFF.md` first). No agent
in flight.

## Done (recent)

- **v1.6.0** — reading position rides into the load as `data-jmnj-open`
  (`InitialPosition` = `Top | Offset | Anchor | SourceLine`, DESIGN D12); text
  zoom inlined the same way; `background = true` / `--background`. Frame-capture
  harness + `first_frame_scroll_y` e2e observable.
- **Build speed (2026-08-07)** — mold linker via `.cargo/config.toml` + dev
  `debug = "line-tables-only"`: incremental rebuild 5.3s → 1.0s. `mold` is now
  an AUR makedepends.
- **v1.5.0 / v1.4.0 / v1.3.0** — jumplist breadcrumb, paginated `:` completion;
  Obsidian dialect (wikilinks, vault resolution, callouts, embeds, frontmatter).
- **v1.2.x** — Neovim sync plugin, 960px column, NON_UNIQUE GApplication.

## Next

1. **Verify the flash fix on `board-reader/docs/index.md`** — the one machine
   that reliably shows the bug. `8559b49` makes the restore loop concede only
   after the document stops growing (3 unchanged `scrollHeight` frames) rather
   than one frame after `readyState === 'complete'`; conceding early revealed
   the body at a `scrollTo`-clamped near-top offset, and the post-load settle
   then jumped it down. Measured: forcing `apply` to fail flips the restore from
   `why=reached, reveal_y=24000` to `why=gaveup, reveal_y=0` — the reported
   shape. **Never reproduced headlessly**, so tests passing is not evidence the
   user-visible bug is gone. If it persists, the reproduction gap is the finding.
2. **Startup double load** — real but separate: at launch the vault index goes
   empty → populated, so `rescan_vault` fires a second full `render_and_load`
   after the first already finished. Wasted parse/layout/paint on every start.

## Open questions

- Is the two-frame background flash during a restore (the hide gate's known
  cost, noted as "next" in DEVLOG 2026-08-07) the same thing the user is seeing
  on document switch, or a separate defect? Answering this scopes item 1.
