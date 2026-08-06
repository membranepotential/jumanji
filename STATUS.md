# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline (no JavaScript), modal vim/zathura keybindings, Obsidian
dialect support. Linux-first (Arch, X11/i3wm).

## Now

Shipped **v1.6.0** (tag + GitHub release + AUR PKGBUILD pointed at it). Two
workstreams in flight on the open bugs below:

- **flash-diagnosis** — re-diagnose the document-switch flash (Next 1) from a
  fresh frame capture of a link-follow. Read-only; returns a root cause + fix
  proposal. State: running. Next: hand its proposal to an implementer.
- **zoom-sticky** — make zoom a session-level setting that carries across
  navigation (Next 2). Owns `src/shell/app.rs`. State: running. Next: review,
  then commit.

Sequencing: both bugs live in `src/shell/app.rs`, so the flash *fix* only starts
after zoom-sticky lands.

## Done (recent)

- **v1.6.0** — reading position rides into the load as `data-jmnj-open`
  (`InitialPosition` = `Top | Offset | Anchor | SourceLine`, DESIGN D12); text
  zoom inlined the same way; `background = true` / `--background`. Frame-capture
  harness + `first_frame_scroll_y` e2e observable.
- **v1.5.0** — jumplist breadcrumb in the statusbar, paginated `:` completion.
- **v1.4.0 / v1.3.0** — Obsidian dialect: wikilinks, vault resolution, callouts,
  embeds, vault rooting, frontmatter, side buttons.
- **v1.2.x** — Neovim two-way sync plugin, 960px content column, NON_UNIQUE
  GApplication (one process per window).

## Next

1. **Flash on document switch is NOT fixed.** Despite the v1.6.0 work, switching
   files still flashes visibly. The D12 mechanism (arm position → inline
   `data-jmnj-open` → rAF user-script → `html.jmnj-restoring body { visibility:
   hidden }` + failsafe reveal) either isn't covering the real path or the
   hide-gate reveal itself is what's seen. Needs re-diagnosis from a fresh frame
   capture of a *link follow* / `Ctrl-o` between two real files — the existing
   harness proved the startup/reload case, not necessarily this one. Do not
   assume the previous root cause was the whole story.
2. **Follow a link → keep the current zoom.** Today `load_document`
   (`src/shell/app.rs:1802-1814`) restores the *target file's* saved zoom, or
   resets to 100% when the file has never been opened. Reading at 130% and
   clicking a wikilink therefore drops back to 100%. Zoom should behave as a
   session-level view setting that carries across navigation, with the saved
   per-file value only used when there's no live session zoom to inherit.

## Open questions

- ~~Should per-file zoom in `history` survive once zoom becomes sticky?~~
  **Decided 2026-08-07:** yes, it survives — but it is the *default on open*,
  consulted only when there is no live session zoom to inherit (cold start of a
  window). Once the session has a zoom, it wins on every document switch. Follows
  `docs/research/02-zathura.md:162-166` ("separate default-on-open from current
  live setting"); no `history.toml` format change.
- Is the two-frame background flash during a restore (the hide gate's known
  cost, noted as "next" in DEVLOG 2026-08-07) the same thing the user is seeing
  on document switch, or a separate defect? Answering this scopes item 1.
