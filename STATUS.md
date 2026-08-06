# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline (no JavaScript), modal vim/zathura keybindings, Obsidian
dialect support. Linux-first (Arch, X11/i3wm).

## Now

Shipped **v1.6.0** (tag + GitHub release + AUR PKGBUILD pointed at it). Two
workstreams in flight on the open bugs below:

- **flash-diag2** — re-diagnose the document-switch flash (Next 1). Round 1 was
  stopped for a false negative (60fps video can't prove absence); its rig +
  handoff live in `.flash-investigation/` (gitignored). Round 2 measures
  `first_frame_scroll_y` instead of filming. State: running. Next: hand its
  proposal to an implementer.

## Done (recent)

- **v1.6.0** — reading position rides into the load as `data-jmnj-open`
  (`InitialPosition` = `Top | Offset | Anchor | SourceLine`, DESIGN D12); text
  zoom inlined the same way; `background = true` / `--background`. Frame-capture
  harness + `first_frame_scroll_y` e2e observable.
- **v1.5.0 / v1.4.0 / v1.3.0** — jumplist breadcrumb, paginated `:` completion;
  Obsidian dialect (wikilinks, vault resolution, callouts, embeds, frontmatter).
- **v1.2.x** — Neovim two-way sync plugin, 960px content column, NON_UNIQUE
  GApplication (one process per window).

## Next

1. **Flash on document switch is NOT fixed.** User-confirmed shape: the new
   document is painted **at scroll 0**, then scrolls to the linked/stored
   position — not a white frame, not the old document persisting. Happens on
   **both** link-follows and jumplist `Ctrl-o`/`Ctrl-i`, so it's a property of
   the document-switch path in general.

   Verified: `pending_position` *is* consumed before the load
   (`do_render_and_load`, `src/shell/app.rs:543-544`), so D12 does cover this
   path — "position applied after paint" is not the explanation.

   Leading hypothesis: a **second full load** per switch. `load_document` calls
   `rescan_vault` + `do_render_and_load`; when the rescan lands and the index
   differs (`app.rs:500`), it fires `render_and_load` again — and by then the
   finished first load has reset `pending_position` to `Top`, so
   `preserve_scroll` is true and an async scroll round-trip precedes a second
   parse/layout/paint. Would also explain why a small static fixture never
   reproduces it (identical index → early return → no second load).

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
