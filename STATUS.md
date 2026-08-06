# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline (no JavaScript), modal vim/zathura keybindings, Obsidian
dialect support. Linux-first (Arch, X11/i3wm).

## Now

Shipped **v1.6.0** (tag + GitHub release + AUR PKGBUILD pointed at it); working
tree clean on `main`. No workstream in flight — next move is the two open bugs
below.

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

- Should per-file zoom in `history` survive at all once zoom becomes sticky
  across navigation — or does the session value always win, with history keeping
  only the scroll offset? (Decide before touching `load_document`; check
  `docs/research/02-zathura.md` for the zathura precedent.)
- Is the two-frame background flash during a restore (the hide gate's known
  cost, noted as "next" in DEVLOG 2026-08-07) the same thing the user is seeing
  on document switch, or a separate defect? Answering this scopes item 1.
