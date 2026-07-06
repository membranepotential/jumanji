# Development log

Newest entries first. Each entry: what happened, what was decided, what's next.

## 2026-07-06 — v0.1.0: `+`/`-` back to geometric zoom, anchored text zoom

User feedback after trying the two-axis zoom: `+`/`-` should do geometric
zoom (zathura muscle memory), and text zoom made the viewport jump — reflow
moves content, so a fixed pixel offset lands somewhere else in the document.

- Defaults swapped: `+`/`-` → geometric (`zoom in`/`zoom out`); text zoom is
  `Ctrl+Shift+wheel` / config-only.
- Text zoom now anchors the reading position: capture the element at the top
  of the viewport (probing a few points down the content column's center,
  skipping body/main containers), apply the font-size change, then
  `scrollIntoView` the captured element. No-op at the very top.
- Tagged **v0.1.0**; release build installed to `~/.local/bin/jumanji`,
  registered as the `xdg-open` handler for `text/markdown`.

## 2026-07-06 — Feature batch: two-axis zoom, copy-on-select, fonts, dark hardening

Five-feature batch. **54 core + 10 e2e = 64 tests, clippy `-D warnings` clean,
fmt clean.** Manually smoke-tested under Xvfb (copy-on-select, Ctrl+wheel and
Ctrl+Shift+wheel zoom, dark-first-paint).

- **Diagram-zoom diagnosis (done first, empirically).** Question: does webkit
  `zoom_level` scale mermaid SVGs? merman emits `<svg width="100%"
  style="max-width:<intrinsic>px" viewBox=…>`. Measured a diagram's
  `getBoundingClientRect` in a real WebView at zoom 1.0 vs 2.0 against the actual
  stylesheet: the svg's **layout** px stays constant (111.9) across zoom while
  the CSS viewport halves — i.e. it occupies 35 % → 70 % of the viewport from
  1.0× → 2.0×. **Conclusion: geometric zoom already scales diagrams correctly**,
  because `zoom-text-only` defaults off (full-page zoom scales the px unit
  itself, inline `max-width:<px>` included). No CSS fix needed — and a
  `max-width:100% !important` override was tested and *rejected* (it stretches
  small diagrams to fill and breaks proportional scaling).
- **Two-axis zoom.** `Action` split: `ZoomIn`/`ZoomOut` = geometric (webkit
  `zoom_level`), new `TextZoomIn`/`TextZoomOut` = the `--font-size` CSS var on
  `<html>` (reflow, no geometry change, clamped 8 px…3× base), `ZoomReset` resets
  both. Defaults: `+`/`-` → text zoom, `=` → reset both; geometric has no default
  key (config `zoom in`/`zoom out` — zathura spelling — and `Ctrl`+wheel).
  `Ctrl`+wheel = geometric, `Ctrl`+`Shift`+wheel = text; plain wheel scrolls
  untouched. Statusbar shows `150%/120%T` when either axis ≠ 100 %. `GetState`
  gains `text_zoom`.
- **Copy-on-select (zathura parity).** `WebView` now built with a
  `UserContentManager`; an injected user-script posts the debounced (~200 ms),
  non-empty selection to a `selection` message handler; Rust writes it to the
  primary or clipboard selection per new `selection-clipboard` option (default
  `primary`). Empty selections post nothing.
- **Font config.** New `font-body`/`font-mono`/`font-size` options thread into
  the pipeline and emit CSS-escaped, quoted `--font-*` overrides in the generated
  `:root{…}` block (the stylesheet already consumed these vars). `font-size` is
  the text-zoom base.
- **Reload/dark hardening (flicker follow-up).** `View` tracks desired dark state
  internally; `load_document` pre-applies `class="dark"` on `<html>` so reloads
  and `default-recolor = true` starts paint dark from the first frame, and the
  WebView's native background color switches with the theme (white / `#1a1a1a`)
  so unpainted regions never flash.

**Deviation from brief:** the Ctrl+wheel controller is attached to the
**toplevel window** (capture phase), not the WebView. A capture-phase controller
on the WebView never receives the scroll events (verified: 0 events on the
webview, fires on the window) — the window+capture placement is the same
architectural guarantee the key controller already relies on (DESIGN D4).

Copy-on-select is not e2e'd (a real selection drag under Xvfb is flaky); the
clipboard-target parsing is unit-tested and the write path is a thin
`Clipboard::set_text`. Wheel zoom is likewise verified manually, not in the
suite (`xdotool click 4/5` only delivers scroll pointer-based, never via
`--window`).

**Next:** M2 — TOC mode, `:` commands, link hints.

## 2026-07-06 — Flicker root-caused: self-sustaining reload loop

User-reported "flicker after scrolling" reproduced headlessly (Xvfb + 30 fps
ffmpeg frame capture + per-frame brightness analysis): a sustained ~4 Hz
light/dark oscillation. Root cause: `watch.rs` filtered file events by *path*
only, so `Access` (read) events counted as changes — and the reload handler
itself reads the file, so **one external read of the document started an
infinite reload loop** (read → Access event → debounce 150 ms → poll 120 ms →
reload → read → …). Every cycle reset scroll and repainted; in dark mode each
fresh load painted light before recolor re-applied, hence the strobe.

Fix: filter to content-mutating event kinds (`Create`/`Modify`/`Remove`).
Verified with the same frame harness under deliberate external reads:
150 frames, brightness stddev 0.003, zero flashes (buggy build: 0.374
oscillation amplitude). Remaining hardening queued: pre-apply the dark class
at load (no light flash on *legitimate* reloads) and set the WebView native
background to the theme color.

Testing method note: GUI verification now runs on Xvfb — never on the live
X session (the earlier live-session testing caused visible window flicker on
the desktop).

## 2026-07-06 — D-Bus state interface + headless e2e harness

Added a per-instance D-Bus service and a real-app e2e test suite. **53/53 tests
(46 core + 7 e2e), clippy `-D warnings` clean, e2e green across repeated runs
(~9.5 s/run under Xvfb).**

- `shell/dbus.rs`: zathura-style per-instance service — owns
  `org.membranepotential.jumanji.PID-<pid>` on the **session** bus, object
  `/org/membranepotential/jumanji`, interface `org.membranepotential.jumanji`.
  Built on `gtk::gio` (`bus_own_name` + `register_object` with introspection
  XML) — **no new deps, no zbus**. Two methods:
  - `GetState() -> (s)` — JSON snapshot (`file`, `scroll_y`, `scroll_percent`,
    `dark`, `zoom`, `mode`, `section`, `toc_len`, `loaded`). Scroll figures are
    queried live from the webview; the reply is completed from the async JS
    callback (`DBusMethodInvocation` finished later) so the main loop never
    blocks.
  - `ExecuteAction(s, u)` — parses via `core::config::parse_action` and runs the
    same `execute()` path the keyboard uses; unknown action → D-Bus error.
  The module is pure transport: the app injects two closures, so `dbus.rs` never
  sees `Shell` and `app.rs` never sees a `Variant`. This is deliberately the M3
  editor-sync (D7) foundation, not test-only. Name-acquisition failure (no
  session bus) logs to stderr and the reader still runs.
- `app.rs`: added a `loaded` flag (set on the first `LoadEvent::Finished`) so
  clients can wait for a driveable window — keys/actions before load are
  no-ops. Wired `serve_dbus`; `_dbus` owner id kept for process lifetime.
- `tests/e2e.rs`: spins up a private `Xvfb` + private `dbus-daemon` per test,
  launches the real binary on `demo/demo.md`, waits for `loaded`, injects real
  GTK keys via `xdotool`, asserts on `GetState`. RAII teardown even on panic;
  serialized behind a mutex (concurrent WebKit instances are flaky); skips
  cleanly if `Xvfb`/`xdotool`/`dbus-daemon` are absent. Covers j/k, counts,
  gg/G, Ctrl-r, +/=, J/K, and the pure-D-Bus ExecuteAction path.
- **Two Xvfb gotchas found and documented** (`docs/TESTING.md`): with no window
  manager, `xdotool key --window` is dropped unless the window is first given X
  input focus (`windowfocus --sync`; `windowactivate` needs EWMH); and Shift
  must be the explicit `shift+g` form, not the bare `G` keysym. A prior debug
  session's *stale overlapping app processes* — not a code bug — were what made
  section state look like it reset; a clean run is deterministic.

**Next:** M2 — TOC mode, `:` commands, link hints; and the reverse D7 direction
(modifier-click → `$EDITOR +line`).

## 2026-07-06 — M1 MVP: it renders, it scrolls, it recolors

Core pipeline and GTK shell implemented (in parallel, by two agents with
disjoint file ownership) and integrated on the first build: **46/46 tests,
clippy `-D warnings` clean.**

- `core/`: comrak (GFM + footnotes + header IDs) → AST passes (mermaid via
  merman → inline SVG, syntect classed highlighting, table wrapping) → complete
  HTML document with embedded CSS. TOC anchors are byte-identical to emitted
  ids (comrak's own `Anchorizer`). Light theme InspiredGitHub, dark Base16
  Ocean Dark under `html.dark`.
- `shell/`: capture-phase `EventControllerKey` → pure `Matcher`
  (counts, `gg`, `<N>G`), actions executed via webkit6 (`scrollBy` JS, zoom
  level, FindController search, `classList.toggle('dark')` recolor), girara
  bar (filename · pending keys · percent), notify-based live reload with
  scroll restore.
- Gotcha found at integration: merman's `render` cargo feature is required
  for `merman::render` to exist (default features are empty).
- Verified live on X11 (screenshots): typography, tables, syntect fences,
  both mermaid diagrams, footnotes, `j`/`G`/`Ctrl-r` all behave; statusbar
  percent honest (queries actual `scrollY`).
- Known gaps: keys are silent no-ops until the initial load finishes
  (sub-second); section tracking (`J`/`K`) steps the TOC list rather than
  following the viewport; ToC mode and `:` commands are M2.

**Next:** headless e2e testing (Xvfb + D-Bus state interface — doubles as the
M3 editor-sync foundation).

## 2026-07-06 — Project inception

**Research.** Three parallel research passes (full write-ups in
[research/](research/)):

1. [Landscape survey](research/01-landscape.md) — no "zathura for markdown"
   exists; inlyne is closest but keyboard-less and mermaid-less; the "missing
   5%" (mermaid, math, callouts) is why users switch tools.
2. [Zathura architecture & UX](research/02-zathura.md) — girara's inputbar/
   statusbar/dispatch design, the zathurarc idioms, the full keybinding table,
   and the SyncTeX editor-pairing pattern.
3. [Rust stack evaluation](research/03-rust-stack.md) — webview vs native,
   the 2026 pure-Rust mermaid breakthrough (merman), parser/highlighter
   comparison.

**Girara-as-framework considered and rejected** (user suggestion, investigated
seriously): girara's GTK parts were stripped upstream in Feb 2026 and absorbed
into zathura as an internal static lib — no installable headers, no Rust
bindings, no introspection. We reimplement the small girara subset in Rust
instead, with zathura's `girara-gtk/` as design reference.

**Architecture decided** (full record in [DESIGN.md](DESIGN.md)):

- Rust · gtk4-rs + system WebKitGTK 6 (webview for typesetting only)
- 100%-Rust content pipeline: comrak (GFM) + syntect/two-face (highlighting)
  + merman (pure-Rust mermaid → inline SVG). **No JavaScript pipeline.**
- Capture-phase GTK key controller → girara-style mode/count dispatch
- TOML config with zathura idioms; notify-based live reload
- License: MIT. Repo: github.com/membranepotential/jumanji

**Next:** cargo scaffold, then parallel implementation of core pipeline and
GTK shell (Opus subagents), integration, first running MVP.
