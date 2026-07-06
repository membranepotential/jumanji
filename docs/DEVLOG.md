# Development log

Newest entries first. Each entry: what happened, what was decided, what's next.

## 2026-07-06 — intrinsic diagram size, cursor-anchor fix, leading-edge zoom, one demo

Four related fixes, driven by user feedback that big diagrams were unreadably
small at zoom 1 and that Ctrl+wheel didn't zoom towards the cursor near the
bottom of the page:

- **Diagrams render at intrinsic (natural) size.** The old model shrank every
  diagram to fit the reading column, so a large flowchart was tiny at zoom 1 and
  zooming to read it blew the prose up. New model: `core::diagram` parses the
  intrinsic pixel width from merman's SVG root (`max-width:<N>px`) and emits
  `<div class="mermaid" style="--dw:<N>px">`; `.mermaid svg` is now
  `width: var(--dw, auto)`. A diagram wider than the column renders full-size and
  overflows into its own `.mermaid` scroll box, never the page. Because WebKit's
  native geometric zoom multiplies CSS px → device px, device size = intrinsic ×
  zoom *for free* — so the `--zoom` custom property and the old
  `min(100vw × --zoom − chrome, …)` formula are **gone** (`--diagram-chrome`
  deleted with them). Parse-failure degrades to a plain wrapper (svg `auto`).
  Text-zoom invariance is now by construction (diagrams don't read `--font-size`).
  Sample intrinsic widths: flowchart-LR 1200 px, gantt 1184 px, timeline 1390 px,
  sequence 650 px.
- **Cursor-anchored zoom bug fixed.** `flush_wheel_zoom` set `Shell.zoom` to the
  new level *before* calling `cursor_anchor`, but the capture JS runs while the
  page is still laid out at the OLD zoom; `cursor_anchor` divides the pointer by
  the zoom to get CSS px, so it used the wrong divisor and the error grew with
  distance from the origin — hence "doesn't zoom towards the cursor" near the
  bottom. Now the anchor is captured from the pre-change zoom. New e2e drives a
  real Ctrl+wheel with the pointer ~80 % down and asserts the reflow direction +
  no page h-scroll.
- **Leading-edge wheel-zoom coalescing.** Every Ctrl+wheel tick used to wait the
  full 40 ms before anything happened. Now the first tick of a burst applies
  immediately and the 40 ms timer only batches ticks that arrive during the
  trailing window (no-op if none do); no tick is ever lost. Combined with the
  `--zoom` removal — which drops a root-level custom-property write (a full
  document style invalidation) from *every* zoom step — this is the zoom-latency
  win. (WebKit reflow remains the measured floor; no other speculative perf.)
- **Native zoom survives reload**, so the load-finished handler no longer
  re-applies geometric zoom (only text zoom's `--font-size`, which *is* lost on
  reload). Verified: `zoom_level` is a WebView property, and the live-reload e2e
  now zooms in a narrow window and asserts the CSS viewport stays collapsed
  across the reload.
- **One demo.** `docs/demo.md` (the richer 11-diagram rendering showcase) is now
  the single `demo/demo.md`, with the run command + image path fixed, the
  zathura driving hint expanded (Tab/TOC, f/hints, q), and the old demo's
  unknown-language-fence coverage folded in. The duplicate `Notes` headings
  (anchor disambiguation) are preserved. Old `docs/demo.md` deleted.

e2e: `narrow_viewport_zoom_reflows_without_page_overflow` gains an assertion that
at zoom 1 the diagram width exceeds the narrow viewport (overflow inside its
box); device-growth assertion unchanged. New `ctrl_wheel_zoom_anchors_with_cursor_near_bottom`.
108 unit + e2e.

Next: user feel-test on real GPU (intrinsic diagrams, cursor anchor, leading-edge
snappiness).

## 2026-07-06 — v0.2.1: zoom mechanics reworked — reflow, cursor anchor, coalescing

User feedback on v0.2.0: scrolling fixed, but zoom felt janky, should zoom
towards the cursor, and should *reflow* the text — no horizontal page scroll
after zooming. That reverses the reflow-free design from earlier today, so
D5a was rewritten rather than patched (the original requirements it served —
diagrams must really zoom, position must not drift — still hold and are now
met by construction instead of by rigidity):

- **Text reflows**: the column re-fits the viewport again; the page never
  scrolls horizontally (e2e-asserted: `doc_scroll_width ≤ viewport_width` at
  every zoom × width combination). A pre-existing overflow leak surfaced by
  the new assertion — an unbreakable long-URL link — fixed with
  `overflow-wrap: break-word` on the column.
- **Diagrams keep zooming**: `.mermaid svg` pins its CSS width to the zoom-1
  value (`min(100vw × --zoom − chrome, --content-width − chrome)`; chrome in
  `rem`, invariant under text zoom since `rem` tracks the fixed root font,
  not `--font-size`). Device width scales ∝ zoom — verified empirically over
  zoom {0.7, 1, 1.5, 2} × window {500, 800, 1200}; overflow scrolls inside
  the `.mermaid` box. `.mermaid` padding normalised 1.1em → 1.25rem (0.4 px).
- **Anchored zoom, one mechanism**: capture the element under a probe point +
  its viewport offset, apply the zoom, scroll it back. `Ctrl`+wheel probes
  the cursor (motion-controller-tracked; GTK→CSS coords divide by zoom);
  keys/D-Bus/text zoom probe the viewport top. Race-free by sequencing the
  native `set_zoom_level` inside the capture eval's completion callback.
  `Shell.zoom` is now the source of truth (native level lands async).
- **De-janked**: wheel ticks coalesce into one anchored application per
  ~40 ms (10-tick burst: 10 reflows → 1, settle ~5.5× faster under Xvfb;
  absolute feel needs the user's GPU).

e2e: `narrow_viewport_zoom_does_not_reflow` rewritten as
`narrow_viewport_zoom_reflows_without_page_overflow` (the old invariant is
intentionally dead); new tests for position anchoring (±8 % band), real
synthetic Ctrl+wheel at the pointer (XTEST button 4/5 works under Xvfb), and
burst coalescing. 105 unit + 21 e2e.

Next: user feel-test on real GPU (cursor anchor, jank, 40 ms window).

## 2026-07-06 — v0.2.0: performance investigation — measure, don't guess

User: "startup could be faster, scrolling feels janky." A dedicated
measurement pass (release build, headless Xvfb, medians over runs, ffmpeg
frame capture for what the user actually sees) instead of speculative fixes:

- **Startup ≈ 950–1050 ms is a WebKitGTK floor, not app code.** The Rust
  pipeline renders the full demo (11 mermaid diagrams) in **20 ms**; WebKit
  web-process spawn (253 ms) + one-time engine warmup/layout (436 ms)
  dominate, with a blank window from ~290 ms until content pops. Every
  surgical fix was tested and disproven (pre-warm, load-before-present —
  250 ms *worse*, hwaccel toggles, a11y off, font trims: all ±0). This is
  DESIGN.md D2's documented risk materializing. Real levers are
  architectural and deferred: a daemon/window-reuse mode over the existing
  D-Bus seam (a warm process loads in 35 ms), or D2's egui escape hatch.
- **Scroll jank = smooth scrolling × SVG compositing.** Headless scrolling
  is mechanically flawless (zero lost distance over 60-key storms; the
  per-key status JS round-trip is 0.28 ms — debouncing would be pointless).
  But `enable_smooth_scrolling(true)` makes WebKit animate every wheel tick
  (~100 ms, 4× the composited frames for the same distance). **Applied:**
  smooth scrolling off + explicit `behavior:'instant'` in the scroll JS —
  zathura semantics, and a repeated `j` can never restart an in-flight
  animation. Feel needs confirming on the real GPU (Xvfb has none), along
  with two user-side experiments: `WEBKIT_DISABLE_DMABUF_RENDERER=1` and
  touchpad inertia through the capture-phase scroll controller.
- **Rendering needs no work in release.** Syntax set/theme already
  OnceLock-cached; live reload 57–78 ms end-to-end. Debug builds were
  10–20× slower in the pipeline: **applied** `[profile.dev.package."*"]
  opt-level = 2` (deps optimized once, the crate itself stays fast to
  rebuild). Disproven-and-not-built: status-bar debounce, mermaid
  content-hash cache, syntax-set pre-warm thread.

## 2026-07-06 — v0.2.0: milestone M2 — TOC, hints, command line, marks, persistence

M2 complete, split core-first then shell, both halves by Opus subagents
against a pinned contract; 105 unit + 18 e2e tests, clippy/fmt clean.

Core (pure, unit-tested): `command.rs` (`:` parse + completion; path
completion delegated to the shell via `Completions::Path`), `jumplist.rs`
(vim semantics, cap 100), `marks.rs` (char registers, position+zoom),
`history.rs` (per-file state, TOML array-of-tables for lossless LRU
round-trips, cap 500), keymap char-argument bindings (`m`/`'` capture the
next key as a typed argument — no stringly dispatch), `Mode::Toc` default
table, runtime `Options::set` returning a typed `SetEffect`
(Rerender/Recolor/None), comrak GFM alerts (verified against vendored
source: `extension.alerts`, `.markdown-alert-<kind>` classes) styled via
`--alert-*` accents + `color-mix` tints, and `extra_css` emission for user
themes.

Shell: TOC mode as a `gtk::Stack` page (collapsible outline tree, dispatcher
driven, counts work); `f`/`F` link hints (overlay JS labels visible links,
posts `label\thref` back; shell-local hint input state, *not* a keymap
mode); navigation policy that denies everything except our programmatic
loads — fragment clicks scroll in-place (jumplist push), local `.md` links
open in-window and re-point the watcher, anything else goes to the system
handler (the reader itself stays offline); `:` command line with typed
prompts, Tab completion incl. real filesystem completion; quickmarks +
jumplist wired through async scroll queries; per-file scroll/zoom/text-zoom
persistence at `$XDG_DATA_HOME/jumanji/history.toml` (restored on open,
flushed synchronously on close — two RefCell-reentrancy bugs found by the
new e2e tests and fixed); user CSS themes hot-swapped by a second directory
watcher; `GetState.mode` now truthful (normal/toc/hint/command/search).
The e2e harness gained private `XDG_DATA_HOME` isolation and a
clean-quit + relaunch flow to prove persistence.

Deliberately skipped: window-geometry persistence (core history stores
per-file entries only; a second format wasn't worth it), mouse-click e2e for
fragments (hint-follow exercises the identical routing deterministically).

Next: M3 — editor sync (D-Bus forward/reverse), external fence renderers,
math, stdin streaming; decide on the AUR publish.

## 2026-07-06 — Arch/AUR packaging

Added real Arch Linux packaging so jumanji installs and upgrades like any
other system package instead of living in `~/.local/bin`.

- `resources/jumanji.desktop`: standard desktop entry (`Exec=jumanji %f`,
  resolved via `$PATH` — not a hardcoded install path — since the system
  package puts the binary on `$PATH`), registers as a `text/markdown` /
  `text/x-markdown` handler.
- `resources/config.example.toml`: every `[options]` key documented at its
  built-in default, cross-checked against `RawOptions`' serde renames in
  `src/core/config.rs` rather than guessed from the README, plus a couple of
  commented `[keys.normal]` remap examples.
- `packaging/aur/PKGBUILD` + generated `.SRCINFO`: sources the GitHub release
  tarball for the tagged version, `cargo fetch --locked` in `prepare()`,
  `cargo build --frozen --release` in `build()`, installs the binary, desktop
  file, example config, LICENSE, and README.
- **Real build hazard found and fixed**: a clean `makepkg -f` reliably failed
  to link — `undefined symbol: onig_new` and friends from `onig_sys` (pulled
  in by syntect). Root cause: Arch's default `CFLAGS` carries `-flto=auto`,
  and `onig_sys` compiles bundled oniguruma C sources via the `cc` crate
  using that `$CFLAGS`; the resulting objects are GCC-only LTO bitcode that
  rustc's default `lld` linker can't resolve. Fixed with
  `CFLAGS+=' -ffat-lto-objects -w'` in `build()` — same fix the official
  `bat` package uses for the identical syntect+onig combination. A first
  build succeeding only after a second incremental rebuild would have been
  an easy trap (looks like a flaky race; it's actually deterministic and
  CFLAGS-dependent) — worth remembering for any future dependency that
  vendors C sources.
- Verified end-to-end: `makepkg -f` in a scratch copy produces
  `jumanji-0.1.1-1-x86_64.pkg.tar.zst` containing `/usr/bin/jumanji`
  (dynamically linked, stripped, executable), the desktop file, example
  config, and LICENSE. Not installed (no `pacman -U`) and not pushed to the
  AUR — the PKGBUILD currently points at the already-tagged `v0.1.1` GitHub
  release tarball, which predates `resources/` and `packaging/`, so
  packaging from the public tarball won't have the desktop file/config until
  a new tag (e.g. `v0.1.2`) is cut that includes them.

Next: cut a release that includes `resources/` and `packaging/`, then
publish to the AUR.

## 2026-07-06 — reflow-free geometric zoom (narrow-viewport diagram bug)

User-reported: mermaid diagrams didn't zoom when the window was narrower than
`page-width` (they did on wide windows), and zooming on a narrow viewport
drifted the reading position. One root cause: the column was sized
`max-width: var(--content-width)`, so on a narrow window its width tracked the
viewport — and webkit zoom *shrinks* the CSS viewport, so the column reflowed
to re-fit and `max-width: 100%` re-fit the SVGs. Net visual change: zero.

Fix (one rule): the shell mirrors `zoom_level` into a `--zoom` custom property
(re-applied after each load, like `--font-size`), and the stylesheet sizes the
column `width: min(var(--content-width), calc(100% * var(--zoom, 1)))`. The
layout width in CSS px is now invariant under zoom → no reflow → position
stays put, and content overflows the window edge instead of re-fitting
(pan with `h`/`l`) — zathura semantics. DESIGN.md D5a updated.

Testability: `GetState` now reports `content_width` (the column's layout width,
CSS px); a new e2e test narrows the window to 500 px, zooms ×1.5, and asserts
width invariance + real device-px growth. Asserts poll (the `--zoom` set is an
async JS eval) and the baseline waits for two stable readings after resize.

Next: M2 features, startup/scroll performance.

## 2026-07-06 — v0.1.1: config format bug — `[options]` was silently ignored

Found while verifying the installed release: `default-recolor = true` had no
effect. The parser expected option keys at the TOML **top level**, while the
README/DESIGN (and the user's config) put them in an `[options]` table — and
serde silently ignored the unknown `options` field, so the whole table was
swallowed with no error. The "fonts worked" evidence was a red herring: the
stylesheet defaults happen to match the configured fonts.

- Implementation now matches the documented format: `[options]` +
  `[keys.<mode>]`.
- `deny_unknown_fields` on every raw config struct: a misplaced or misspelled
  key now errors loudly (still non-fatal — stderr + defaults). Regression test
  asserts the old top-level format errors.
- e2e isolation hole closed: the harness inherited `$HOME`, so tests read the
  developer's real `~/.config/jumanji/config.toml`; the app child now gets a
  private empty `XDG_CONFIG_HOME`.

Released as **v0.1.1** (v0.1.0 shipped with the bug).

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
