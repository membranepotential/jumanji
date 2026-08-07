# STATUS

Canonical dashboard for jumanji. Keep it true; keep it under 60 lines.

## Goal

A zathura-inspired markdown reader: Rust + GTK4 + system WebKitGTK 6, 100%-Rust
content pipeline (no JavaScript), modal vim/zathura keybindings, Obsidian
dialect support. Linux-first (Arch, X11/i3wm).

## Now

Shipped **v1.6.0** (tag + GitHub release + AUR PKGBUILD pointed at it).

No workstream in flight — both flash-diagnosis rounds were stopped by the user.
Their output is saved in `.flash-investigation/` (gitignored), `HANDOFF.md` as
the entry point: what was disproven, the untested hypothesis, the next
experiment.

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

1. **Flash on document switch is NOT fixed.** User-confirmed shape: the new
   document is painted **at scroll 0**, then scrolls to the linked/stored
   position — not a white frame, not the old document persisting. Happens on
   **both** link-follows and jumplist `Ctrl-o`/`Ctrl-i`, so it's a property of
   the document-switch path in general.

   **Ruled out by trace evidence** (`.flash-investigation/traces/`): the position
   is consumed *before* the load (app.rs:543-544) and is armed correctly on
   switches (`open=Some("offset:45764")`); and there is exactly **one** load per
   switch — the vault-rescan double load is startup-only (`equal=true` on every
   switch, app.rs:500 short-circuits).

   **Untested lead:** `setTimeout(reveal, 400)` at `src/shell/view.rs:290` is
   unconditional, while `apply()`'s `scrollTo` clamps against the not-yet-final
   document height. Layout slower than 400ms ⇒ body revealed at ~0, position
   lands after. No fixture yet exceeds 400ms (~25-190ms observed), so this is
   unproven — the next round needs JS-side reveal instrumentation and a
   positive control. See `.flash-investigation/HANDOFF.md`.

## Open questions

- Is the two-frame background flash during a restore (the hide gate's known
  cost, noted as "next" in DEVLOG 2026-08-07) the same thing the user is seeing
  on document switch, or a separate defect? Answering this scopes item 1.
