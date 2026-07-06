# jumanji — design & decision record

Date: 2026-07-06. Based on web research of the markdown-viewer landscape,
zathura's architecture, and the Rust rendering ecosystem (all claims verified
against primary sources at that date).

## Goal

A zathura-inspired markdown **reader** (not editor) for Linux: instant startup,
minimal chrome, vim keybindings with counts and modes, full GFM rendering,
highlighted code blocks, mermaid diagrams, extensible, offline-only.

## The gap we fill

No existing tool combines: native-feeling speed + dedicated reading UX + modal
keyboard control + math/mermaid/full-GFM + extensibility.

- **inlyne** (Rust, wgpu): fast and native, but no vim keys, no mermaid, no math,
  and its issue tracker is dominated by hand-rolled-layout bugs (CJK glyphs,
  wrapping, smooth scrolling, image handling). Lesson: don't hand-roll document
  layout.
- **Ferrite** (Rust, egui, Dec 2025): proved pure-Rust mermaid is possible, but
  immediate-mode redraw burns CPU ("fans spin up"), and reimplementing rendering
  engines is a large imperfect surface.
- **Marker** (GTK3 + WebKit preview): the only prior GUI tool with vim keys +
  mermaid + KaTeX — validated the "webview for fidelity" approach, then died of
  unmaintenance.
- **Terminal viewers** (glow, mdcat, md-tui, frogmouth): bound to the character
  grid; md-tui has the best keyboard model (link-select mode, search) — worth
  porting to a pixel surface.
- **grip**: killed by its GitHub-API/network dependency. Everything must render
  locally.

Recurring user demands across all trackers: the "missing 5%" (math, mermaid,
callouts, footnote anchors, fragment links) is *why* people switch tools; plus
stdin input, live reload, persisted window state, real theming.

## Decisions

### D1: Language — Rust

Memory-safe systems language with first-class GTK4 bindings, the best
markdown/highlighting/mermaid library ecosystem for this task, and a single
static binary at the end. (Considered: C — what zathura uses, but no safety and
weaker libraries for markdown/mermaid; Go/others — no serious GTK4 story.)

### D2: UI — gtk4-rs + system WebKitGTK 6, girara-style shell reimplemented

**Webview for layout, Rust for everything else.** GFM tables, inline HTML,
images, and typography are exactly what a browser engine does perfectly and
what native stacks make you hand-roll (see inlyne's issue tracker).

- `webkit6` crate 0.6.x (GNOME World) wraps the system `webkitgtk-6.0` — the
  modern GTK4 + libsoup3 API tier. No bundled engine.
- **wry/Tauri rejected:** wry's Linux backend is still GTK3 + webkit2gtk-4.1
  (the deprecated legacy tier; GTK4 migration unmerged as of 2026-07,
  wry#1474). Tauri additionally ships IPC/permissions/bundler machinery a
  single-window local reader doesn't need.
- **girara-as-framework rejected:** girara no longer exists as a UI library.
  Upstream stripped its GTK parts (commit `0e6a327`, 2026-02) and absorbed them
  into zathura's tree as an internal static lib, GTK4-ported 2026-06 with no
  installed headers. No Rust bindings, no GObject introspection to generate
  them. The subset a reader needs (inputbar, statusbar, mode/count keybinding
  dispatch, typed settings) is ~200 lines of gtk4-rs — we reimplement it, using
  zathura's `girara-gtk/` (zlib) as the design reference.
- **Native rendering (egui/iced/wgpu) rejected for v1** — but kept as a bounded
  escape hatch: the entire content pipeline (D3) is UI-independent, so if
  WebKit's footprint disappoints, an egui_commonmark front end can replace the
  shell without touching the core.

### D3: Content pipeline — 100% Rust, no JavaScript

Markdown → HTML happens entirely in Rust before the webview sees content. The
webview is a dumb, static renderer: no bundled mermaid.js/highlight.js, no
script execution needed for content, no async render races, and the same
pipeline can later feed an export path (PDF/HTML) or a different front end.

- **Parse: comrak 0.53** — full GFM (tables, task lists, strikethrough,
  autolinks) + footnotes; mutable arena AST makes intercepting fences a
  first-class parse → mutate → format workflow; built-in syntect adapter.
  (pulldown-cmark: flat event stream makes fence interception awkward;
  markdown-rs: dormant.)
- **Highlight: syntect 5.3 + two-face** (bat's extended syntax/theme set).
  Proven, themeable, no JS. (tree-sitter-highlight: DIY per-language quality,
  no theme format.)
- **Mermaid: merman 0.7** — pure-Rust reimplementation of Mermaid.js (native
  parser, Rust ports of Dagre/fCoSE layout, 23+ diagram types, golden-snapshot
  parity tests against Mermaid 11.15). Adopted by Zed for the same purpose.
  Pre-1.0: parity gaps are possible, so diagram rendering errors must degrade
  gracefully (show the fence as a highlighted code block + error note).
  - Rejected: mmdc (needs Puppeteer + ~200 MB Chromium), QuickJS/boa + resvg
    (mermaid.js needs a layout-capable DOM — `getBBox()` — and resvg can't
    render `foreignObject`), kroki/mermaid.ink (network).
- **Serving:** rendered HTML + embedded CSS/fonts via a custom `app://` URI
  scheme handler (`WebContext::register_uri_scheme`); CSP
  `default-src 'none'; style-src app:; img-src app: data: file:` as
  belt-and-suspenders. Local images resolve relative to the document.

### D4: Keybindings — GTK capture phase, zathura semantics

GTK4 dispatches key events capture-phase from the window down before the target
widget, so an `EventControllerKey` with `PropagationPhase::Capture` on the
toplevel handles vim keys *before* WebKit — architecturally guaranteed, no
focus fights. Dispatch is girara-style: `mode × count × key-sequence → Action`,
count-prefix handling done once in the dispatcher, never per-binding.
Scrolling/zoom drive the webview via `webkit6` APIs and small JS snippets
(`window.scrollBy`, anchor jumps); search uses WebKit's `FindController`.

### D5: Config — TOML, zathura idioms

Typed options + remappable keys, three concepts only (options, key maps, later
`include`). serde + toml; XDG paths. Every default keybinding remappable;
mode-scoped key tables (`[keys.normal]`, `[keys.toc]`).

Options surface (all optional; defaults in parentheses):

| Key | Type | Meaning |
|---|---|---|
| `scroll-step` | u32 (`60`) | pixels per `j`/`k`/`h`/`l` before count |
| `zoom-step` | f64 (`0.1`) | geometric zoom increment per step |
| `text-zoom-step` | f64 (`0.1`) | text zoom increment (fraction of base) per step |
| `page-width` | u32 (`720`) | content column width, px |
| `default-recolor` | bool (`false`) | start in dark mode |
| `font-body` | string (`""`) | prose font family; empty = stylesheet default serif stack |
| `font-mono` | string (`""`) | code font family; empty = stylesheet default mono stack |
| `font-size` | u32 (`18`) | base body font px; also the text-zoom 100% reference |
| `selection-clipboard` | `"primary"` \| `"clipboard"` (`primary`) | which clipboard copy-on-select writes to |

Font names are CSS-escaped and quoted before emission into the generated
`:root{…}` block (the stylesheet already consumes `--font-body`/`--font-mono`/
`--font-size`). Copy-on-select is zathura parity: a `UserContentManager` script
message handler + injected user-script post the current (debounced, non-empty)
selection to Rust, which writes it to the configured GDK clipboard.

### D5a: Two-axis zoom

Zoom has two independent axes, both count-multiplied and reset together by `=`:

- **Geometric** = webkit full-page `zoom_level` — scales *everything*, diagrams
  included (verified: `zoom-text-only` is off by default, so the px unit itself
  scales, and an inline `max-width:<px>` on a merman SVG scales with it). Bound
  to `+`/`-` (zathura muscle memory; config `zoom in` / `zoom out`) and
  `Ctrl`+wheel. Geometric zoom is **reflow-free**: the shell mirrors the level
  into a `--zoom` custom property and the stylesheet sizes the column as
  `min(--content-width, 100% × --zoom)`, so the layout width in CSS px is
  invariant under zoom. Consequences: the reading position never drifts when
  zooming, and on a viewport narrower than `page-width` the content grows past
  the window edge (pan with `h`/`l`, zathura-style) instead of re-fitting —
  which is what makes a full-width diagram actually get bigger. `GetState`
  exposes the layout width as `content_width` so tests can assert the
  invariant.
- **Text** = the `--font-size` CSS variable on `<html>` — reflows prose without
  touching layout geometry or diagram sizing; clamped to 8 px … 3× base. Bound
  to `Ctrl`+`Shift`+wheel (config `text zoom in` / `text zoom out`); no default
  key. Because reflow moves content, the element at the top of the viewport is
  captured before the change and scrolled back into view after — text zoom
  keeps the reading position anchored.

The statusbar shows `{geometric}%/{text}%T` on the right whenever either axis is
off 100%, and nothing when both are 100%. `GetState` exposes both as `zoom` and
`text_zoom`. The wheel controller lives on the **toplevel window** in capture
phase — the same architectural guarantee the key controller relies on (D4); a
controller attached to the WebView never receives the scroll events.

### D6: Extensibility — pipeline seams, not a plugin ABI

Zathura's C-ABI plugin system is overkill for one format. The extensibility
seams, in order of arrival:

1. **User CSS themes** — drop a `.css` in `~/.config/jumanji/themes/`;
   hot-swappable. (v1)
2. **External fence renderers** — config maps a fence language to a command
   producing SVG/HTML on stdout (`renderers.d2 = "d2 - -"`), the same seam
   merman occupies internally. Covers graphviz, d2, typst-math, … without any
   plugin API. (v2)
3. **Trait-based document backends** (the zathura seam: outline / render /
   links per section) if other formats (AsciiDoc, rST) ever land. (v3, maybe)

### D7: Editor pairing — the SyncTeX analogue (v2)

zathura's most distinctive feature maps 1:1 onto markdown: a `--forward <line>`
CLI flag + D-Bus method (`org.pwmt.jumanji…GotoLine`) so Neovim can point the
open reader at the heading under the cursor, and a modifier-click that shells
out to `$EDITOR +line file` for the reverse direction.

## Non-goals

- Editing. Ever. Pair with an editor instead (D7).
- Windows/macOS in v1 (the core pipeline is portable; the shell is GTK).
- Pixel-perfect mermaid.js parity (graceful degradation instead).
- Network access of any kind.

## Milestones

- **M1 (MVP):** open file/stdin → rendered GFM + syntect + merman; j/k/h/l,
  d/u, gg/G, counts; zoom +/-/=; `/` search n/N; statusbar; live reload
  (notify + debounce, scroll preserved); Ctrl-r recolor; config file with
  remapping; `q`/`Esc`.
- **M2:** Tab TOC mode (tree, zathura index keys); `f` link hints; `:` commands
  with completion; quickmarks `m`/`'`; jumplist Ctrl-o/Ctrl-i; window-state
  persistence; user CSS themes; fragment/anchor links; GFM alerts/callouts.
- **M3:** editor sync via D-Bus (D7); external fence renderers; math (KaTeX-
  equivalent, ideally typst-based, no JS); AUR package; stdin streaming.

## Keybinding spec (M1 + M2)

Adapted from zathura; "page" becomes "section" (heading-delimited).

| Key | Action | Milestone |
|---|---|---|
| `j`/`k`, `h`/`l` | scroll down/up/left/right (× count) | M1 |
| `d`/`u` | half-page down/up | M1 |
| `J`/`K` | next/previous section | M1 |
| `gg`/`G`, `<N>G` | top / bottom / section N | M1 |
| `+`/`-` | geometric zoom in/out (× count) | M1 |
| `=` | reset **both** zoom axes | M1 |
| `Ctrl`+wheel | geometric zoom in/out | M1 |
| `Ctrl`+`Shift`+wheel | text zoom in/out | M1 |
| `/`,`?`, `n`/`N` | search fwd/back, next/prev match | M1 |
| `Ctrl-r` | recolor (dark mode) | M1 |
| `r` | reload | M1 |
| `q`, `Esc` | quit, abort | M1 |
| `Tab` | TOC mode (`j`/`k`/`l`/`h`/`Enter`, zathura tree keys) | M2 |
| `f`/`F` | follow link / show target (hint overlay) | M2 |
| `m<x>`, `'<x>` | set / jump to quickmark | M2 |
| `Ctrl-o`/`Ctrl-i` | jumplist back/forward | M2 |
| `:` | command line (open, set, exec; tab completion) | M2 |

## Component boundaries

Functional core, imperative shell. The core is pure and GTK-free.

```
┌─ core (pure, no GTK, unit-tested) ─────────────────────────────┐
│ pipeline.rs   md text ──comrak AST──▶ transform ──▶ HTML doc   │
│               ├─ highlight.rs  syntect adapter (two-face)      │
│               └─ diagram.rs    ```mermaid → merman SVG inline  │
│ toc.rs        heading extraction → outline tree + anchors      │
│ config.rs     serde+toml: typed options, key tables            │
│ keymap.rs     mode × count × key-seq → Action (pure lookup)    │
└────────────────────────────────────────────────────────────────┘
┌─ shell (gtk4-rs + webkit6) ────────────────────────────────────┐
│ app.rs        window ─ EventControllerKey(Capture) → Action    │
│ view.rs       WebView; app:// scheme (HTML + embedded assets)  │
│ bar.rs        statusbar Label + inputbar Entry                 │
│ watch.rs      notify debouncer → re-render → reload w/ scroll  │
└────────────────────────────────────────────────────────────────┘
```

## Risks & mitigations

- **WebKitGTK footprint/cold-start** — *measured (2026-07, release build,
  target machine):* spawn → content ≈ **950–1050 ms**, of which the Rust
  pipeline is ~20 ms; the rest is WebKit web-process spawn (~250 ms) plus
  one-time engine warmup (~440 ms). Surgical fixes were tested and disproven
  (pre-warm, load-before-present, hwaccel/a11y toggles: ±0). A warm process
  re-loads in ~35 ms, so the honest levers are architectural, both deferred:
  a daemon/window-reuse mode over the D-Bus seam (D7), or the egui escape
  hatch (D2). Smooth scrolling is deliberately **off** (zathura-instant
  semantics; WebKit otherwise animates every wheel tick ~100 ms, 4× the
  composited frames on SVG-heavy pages).
- **merman parity gaps** — degrade to highlighted code block + error note;
  external-renderer seam (D6.2) as user-side fallback.
- **Editor save races** — editors rename-replace on save; watch the parent
  directory with notify-debouncer-full (~100 ms), not the file inode.
