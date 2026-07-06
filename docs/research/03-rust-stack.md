# Research: Rust building blocks (webview vs native, mermaid, parsing)

*Web research conducted 2026-07-06. Versions verified against crates.io and
upstream repos on that date. Target: Arch Linux, X11/i3wm, GTK4 4.22,
webkitgtk-6.0 2.52.4.*

## A) Webview-based rendering

### wry standalone — not viable on this stack today

- wry 0.55.1 (2026-05-04), actively maintained
  ([crates.io](https://crates.io/crates/wry)).
- **Linux backend is still GTK3**: depends on `gtk 0.18` (GTK3 bindings) +
  `webkit2gtk 2.0.2` — the legacy webkit2gtk-4.1/GTK3 API. GTK4/webkit6
  migration tracked in [wry#1474](https://github.com/tauri-apps/wry/issues/1474)
  (open since Jan 2025, unmerged as of 2026-07-06).
- Standalone embedding without tao/winit works
  (`WebViewBuilderExtUnix::build_gtk`) — but the container must be GTK3, so wry
  cannot sit inside a gtk4-rs app. Using it means shipping a second, legacy
  GTK3+WebKit stack.

### Tauri proper — overkill

tauri 2.11.5 (2026-07-01). Adds IPC bridge, capability/permission system,
plugin system, bundler — value for JS-frontend apps; a single-window local
reader needs none of it. Sits on wry → inherits the GTK3 problem.

### Direct gtk4-rs + webkit6 — the winner in category A

- **`webkit6` 0.6.1** (2026-03-11), maintained in GNOME World:
  [webkit6-rs](https://gitlab.gnome.org/World/Rust/webkit6-rs). Wraps WebKitGTK
  **API 6.0** = GTK4 + libsoup3 — exactly Arch's `webkitgtk-6.0` 2.52.4
  ([API versions explained](https://blogs.gnome.org/mcatanzaro/2025/04/28/webkitgtk-api-versions/)).
  **gtk4** crate 0.11.4 (2026-06-29), supports GTK 4.22.
- **Keyboard interception: architecturally guaranteed.** GTK4 dispatches every
  key event capture-phase from the window down before target/bubble
  ([GTK4 input handling](https://docs.gtk.org/gtk4/input-handling.html)). An
  `EventControllerKey` with `PropagationPhase::Capture` on the toplevel runs
  **before** the WebView; return `Propagation::Stop` to consume. Caveat: only
  consume bound keys; consider a passthrough mode for form fields.
- **Custom URI scheme**: `WebContext::register_uri_scheme("app", …)` →
  `URISchemeRequest`/`URISchemeResponse`. Stable API since 2.36.
- **Assets/CSP:** embed assets with `rust-embed`, serve via `app://`; CSP
  `default-src 'self' app:` makes network egress impossible.
- **Footprint/cold-start — honest picture:** no credible published benchmark
  for a minimal WebKitGTK6 window; circulating "30–50 MB" numbers are
  unsourced. One primary data point
  ([tauri#5889](https://github.com/tauri-apps/tauri/issues/5889)) measured
  ~581 MB RSS but USS/PSS ~125/185 MB (RSS mostly shared-library accounting);
  WebKitGTK spawns a separate WebProcess with known long-run growth.
  Expectation: **~100–200 MB effective, warm start well under a second, not
  zathura-light** — benchmark a hello-world webkit6 window early.

## B) Native (non-browser) rendering

- **inlyne**: comrak → HTML subset → custom wgpu layout. Tables yes; mermaid
  no. Good architecture reference, nothing for diagrams.
- **Widget kits**: `egui_commonmark` ~0.22 has GFM tables/tasks/footnotes/
  strikethrough + syntect. iced's first-party markdown widget is more basic.
  Slint has no rich-text/markdown widget
  ([slint#9560](https://github.com/slint-ui/slint/issues/9560)).
- **Text layout**: cosmic-text (battle-tested, powers COSMIC); parley maturing
  (Bevy discussing migration).
- **The mermaid-without-a-browser story flipped in 2026:**
  - **`merman`** ([github.com/Latias94/merman](https://github.com/Latias94/merman))
    — pure-Rust reimplementation of Mermaid.js: native parser, Rust ports of
    Dagre/fCoSE layout engines, headless SVG renderer. No JS engine. Stable
    0.7.0 (June 2026); 0.8.0-alpha.2 on crates.io 2026-06-23. Parity target
    Mermaid@11.15.0 with golden snapshot tests, 23+ diagram types, and
    `render_svg_resvg_safe_sync()` which strips `<foreignObject>` for resvg
    compatibility. **Zed adopted it**
    ([zed#57967](https://github.com/zed-industries/zed/pull/57967)). Pre-1.0:
    parity gaps possible — degrade gracefully.
  - **`mermaid-rs-renderer` 0.3.0** (2026-07-03): second, earlier-stage
    pure-Rust renderer (used by CleverCloud's mdr). Backup/watch.
  - **Everything else is bad**: `mermaid-rs` crate wraps headless Chrome
    (dead); mmdc needs Puppeteer + ~150–200 MB Chromium; QuickJS/boa + resvg is
    a dead end (mermaid.js needs a layout-capable DOM — `getBBox()`, see
    [mermaid#559](https://github.com/mermaid-js/mermaid/issues/559) — and resvg
    explicitly does not render foreignObject, confirmed in the maintainers'
    [non-browser renderer discussion](https://github.com/orgs/mermaid-js/discussions/7085));
    kroki/mermaid.ink require network — disqualified.
- **Category B verdict**: native is now *possible* (egui_commonmark + merman)
  but you hand-roll document polish (table edge cases, inline HTML, images)
  that a webview gives free, and merman parity gaps become your rendering bugs
  with no fallback.

## C) Markdown parsing & syntax highlighting

- **comrak 0.53.0** (2026-07-02, monthly cadence) — **recommended**. Full GFM +
  footnotes; renders HTML directly; mutable arena AST makes intercepting
  ```` ```mermaid ```` fences a first-class parse → mutate → format workflow;
  built-in syntect plugin adapter (`SyntaxHighlighterAdapter`). Production use:
  crates.io, docs.rs.
- **pulldown-cmark 0.13.4** (2026-05-20) — flat event iterator; intercepting
  code blocks means hand-buffering event spans. Choose only for minimal deps.
- **markdown-rs 1.0.0** (2025-04-23) — stable but dormant (>1 yr no release).
- **Highlighting: syntect 5.3.0** (2025-09-27) — Sublime syntaxes + themes,
  mature, comrak-integrated; add **two-face 0.5.1** (2025-12-25, bat's extended
  syntax/theme set) for TOML/TypeScript/Dockerfile coverage. tree-sitter
  highlighting: DIY, uneven per-language quality, no theme format. JS
  highlighters (Shiki/highlight.js) would reintroduce a JS pipeline — rejected.
  **Do it in Rust with syntect** — one pipeline, works for future export/TUI
  paths.

## D) Supporting pieces

- **File watching**: notify 8.2.0 stable (9.0.0-rc.4 in flight) — inotify on
  Linux; pair with **notify-debouncer-full 0.7.0** for editor-save storms
  (editors rename-replace on save: watch the parent directory, debounce
  ~100 ms).
- **Config**: serde + toml 1.1.2. Skip figment (dormant, layered providers
  unneeded). Keybinding remaps as a TOML table; clap for CLI.
- **girara-like statusbar/inputbar for GTK4**: no crate exists (searched
  crates.io/lib.rs/GitHub). Hand-build: `GtkBox` with WebView + status
  `GtkLabel` + `GtkEntry` inputbar revealed on `:` — ~200 lines of gtk4-rs.

## E) girara as the app framework — dead end, recently made moot

- **girara no longer exists as a UI library.** Commit `0e6a327` "Remove Gtk
  specific parts" (2026-02-01) stripped all GTK code; girara 2026.02.04
  (current Arch package) is ~2,350 lines of glib-only utilities. The GTK UI
  moved into zathura's tree (`girara-gtk/`), was GTK4-ported (commit
  `871a82c4`, 2026-06-17), and shipped in zathura 2026.07.02 — compiled as an
  internal static library, headers not installed. Nothing to link against.
  ([pwmt/girara](https://github.com/pwmt/girara),
  [pwmt/zathura](https://github.com/pwmt/zathura); git.pwmt.org offline,
  GitHub canonical.)
- **No Rust bindings exist**, and girara never shipped GObject Introspection,
  so `gir` can't generate them; hand-FFI would target ~5,700 lines of
  callback-heavy GTK3-era C from the last standalone release (0.4.5,
  2024-12-09) — abandoned upstream.
- GTK3 girara would also chain us to **webkit2gtk-4.1** — packaged but
  explicitly legacy ([soup2 deprecation](https://webkitgtk.org/2025/10/07/webkitgtk-soup2-deprecation.html),
  [Arch EOL todo](https://archlinux.org/todo/webkit2gtk-deprecation/)).
- **pwmt/jumanji**: "no longer developed", last commit 2016-02-04, WebKit1 —
  architecturally interesting, zero reusable code.
- The needed subset (session shell, inputbar, statusbar, mode/count dispatch,
  config) is ~2,500–3,000 lines of C in zathura's `girara-gtk/`. Reimplementing
  idiomatically in Rust is decisively cheaper than any FFI route. Zathura
  itself chose "absorb and port" over keeping the library — follow its lead;
  use `girara-gtk/` (zlib) as the design reference.

## Recommendation

**gtk4-rs 0.11 + webkit6 0.6 (system webkitgtk-6.0) · comrak 0.53 +
syntect 5.3/two-face · merman for mermaid · hand-built girara-style shell ·
notify 8.2 · serde+toml.**

- **WebKitGTK for layout, Rust for everything else** — tables/HTML/images/
  typography are what a browser engine does perfectly and native stacks make
  you hand-roll. webkit6 is the only released Rust path onto the system's
  modern WebKit.
- **No JS pipeline at all** — comrak/syntect/merman run before the webview
  sees content; the webview is a dumb static renderer. CSP locks out network.
  Optional mmdc fallback stays a config option, not a dependency.
- **Zathura-feel guaranteed by capture-phase key handling**; scroll/zoom via
  webkit6 APIs. Cold-start risk is real: measure early; the pure core is 100%
  reusable over an egui front end if WebKit disappoints (the escape hatch that
  makes the bet safe).

Component boundaries: see [DESIGN.md](../DESIGN.md).
