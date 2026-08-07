# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline (no JavaScript), modal vim/zathura keybindings, Obsidian
dialect support. Linux-first (Arch, X11/i3wm).

## Now

Shipped **v1.6.0** (tag + GitHub release + AUR PKGBUILD pointed at it).

Both open bugs are fixed and confirmed; working tree clean on `main`, unreleased.
No workstream in flight. Next move is a release, or the startup double load.

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

1. **Release** the two fixes (zoom stickiness + document-switch flash). Both are
   user-visible; minor bump per the release checklist in CLAUDE.md (tag **and**
   `gh release`, then repoint the AUR PKGBUILD).
2. **Startup double load** — real but separate: at launch the vault index goes
   empty → populated, so `rescan_vault` fires a second full `render_and_load`
   after the first already finished. Wasted parse/layout/paint on every start.
3. **The e2e suite cannot see this class of bug.** Every fixture loads too fast
   to catch a document mid-growth, so the flash never reproduced headlessly and
   a green suite proved only "no regression". A fixture whose height grows after
   `readyState === 'complete'` would turn the restore gate into something tests
   can actually defend. Rig for it: `.flash-investigation/` (gitignored).

## Open questions

- Is the two-frame background flash during a restore (the hide gate's known
  cost, noted as "next" in DEVLOG 2026-08-07) the same thing the user is seeing
  on document switch, or a separate defect? Answering this scopes item 1.
