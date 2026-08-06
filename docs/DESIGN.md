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
- **Serving:** the implementation went with a fully self-contained page instead
  of the `app://` scheme sketched here — CSS is inlined (`style-src
  'unsafe-inline'`), math fonts are base64 `data:` URIs (D8), and there is no URI
  scheme handler. Current CSP: `default-src 'none'; img-src file: data:;
  style-src 'unsafe-inline'; font-src data:`. Local images resolve relative to
  the document. (This supersedes the original `app://` plan; see D8.)

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
| `page-width` | u32 (`960`) | content column width, px |
| `default-recolor` | bool (`false`) | start in dark mode |
| `font-body` | string (`""`) | prose font family; empty = stylesheet default serif stack |
| `font-mono` | string (`""`) | code font family; empty = stylesheet default mono stack |
| `font-size` | u32 (`18`) | base body font px; also the text-zoom 100% reference |
| `selection-clipboard` | `"primary"` \| `"clipboard"` (`primary`) | which clipboard copy-on-select writes to |
| `background` | bool (`false`) | detach from the terminal at startup, so the prompt returns immediately; startup-only, and `--background`/`--foreground` override it |

Font names are CSS-escaped and quoted before emission into the generated
`:root{…}` block (the stylesheet already consumes `--font-body`/`--font-mono`/
`--font-size`). Copy-on-select is zathura parity: a `UserContentManager` script
message handler + injected user-script post the current non-empty selection to
Rust on **`mouseup`** — the end of a real pointer-selection gesture — which
writes it to the configured GDK clipboard. Keying off `mouseup` (not
`selectionchange`) is deliberate: WebKit's `FindController` sets the DOM
selection on the active match programmatically, and a `selectionchange` listener
would copy every search hit. For the same reason search must actively *protect*
the clipboard: WebKitGTK mirrors the find match into the X11 PRIMARY selection as
it selects it, so the `FindController::found-text` handler restores PRIMARY to
the user's last real selection (or clears it) after every `/`, `n`, `N` — the
match highlight stays, but a search never lands on the clipboard.

`background` detaches by **re-executing the binary**, not by forking: the process
is about to bring up GTK, WebKit and D-Bus, and `fork` in a soon-to-be-threaded
program is a well-known footgun, while a fresh process starts from a clean slate.
The child gets the original argv plus a trailing `--foreground` (the two flags
override each other last-one-wins, so that also neutralises an explicit
`--background`), its own process group, and null stdio — a detached process that
outlives its terminal must not write to a closed tty. The detach happens as the
last step of startup, after every diagnostic the user needs to see on the
terminal, and never for a stdin source, whose pipe we are still consuming.

### D5a: Two-axis zoom

Zoom has two independent axes, both count-multiplied and reset together by `=`:

- **Geometric** = webkit full-page `zoom_level` — scales *everything*, diagrams
  included (`zoom-text-only` is off by default, so the px unit itself scales).
  Bound to `+`/`-` (zathura muscle memory; config `zoom in` / `zoom out`) and
  `Ctrl`+wheel. Geometric zoom **reflows the text** (user-decided 2026-07,
  replacing the short-lived reflow-free design): the column re-fits the CSS
  viewport, so the page never scrolls horizontally at any zoom level — wide
  tables, code blocks and diagrams scroll inside their own `overflow-x` boxes
  instead. Three consequences are engineered rather than emergent:
  - **Diagrams render at intrinsic size and zoom by construction.** merman
    lays out each diagram at a natural pixel width (emitted as the SVG root's
    inline `max-width:<N>px`); the pipeline parses that value and pins it onto a
    per-diagram `--dw` custom property, and `.mermaid svg` sets
    `width: var(--dw)`. The CSS width is therefore the **intrinsic** width — a
    diagram bigger than the reading column renders full-size at zoom 1 and
    overflows into its own `.mermaid` scroll box (`overflow-x: auto`), never the
    page (the earlier fit-to-column shrinking made large diagrams unreadably
    small). Under WebKit's native geometric zoom — which multiplies CSS px →
    device px — the device size is simply `intrinsic × zoom`, with **no `--zoom`
    mirroring** needed. Text zoom rewrites only the body `--font-size`, so it
    leaves diagrams untouched by construction. If the width can't be parsed the
    pipeline omits `--dw` and the svg falls back to `auto`.
  - **The reading position is anchored, not accidental.** One anchor
    mechanism (capture `elementFromPoint` + viewport offset before the change,
    scroll it back after) is shared by both axes, parameterised by probe
    point: `Ctrl`+wheel anchors **at the cursor** (pointer tracked via a
    motion controller; GTK-logical → CSS px is `v / zoom`, evaluated at the
    **pre-change** zoom the page is still laid out at — using the post-change
    zoom misplaces the anchor, worst near the viewport bottom), keyboard/D-Bus
    zoom and text zoom anchor at the top of the viewport. Sequencing is
    race-free: capture-JS → (completion callback) native `set_zoom_level` →
    restore-JS. `Shell.zoom` is the source of truth (the native level lands
    async); the native level survives a document reload (a WebView property), so
    no re-apply is needed on load.
  - **Wheel zoom is coalesced, leading-edge** (~40 ms trailing window): the
    first tick of a burst applies immediately (a single tick feels instant), and
    any ticks arriving within the window after it are batched into one further
    anchored reflow — a burst becomes at most 2 applications instead of N, and
    no tick is ever lost (every tick adds a step; the flush drains all
    accumulated steps).

  `GetState` exposes `content_width` (reflows with zoom now), plus
  `viewport_width`, `doc_scroll_width` (the no-page-h-scroll invariant) and
  `diagram_width` (CSS px, now constant ≈ intrinsic under zoom; device size =
  × zoom) for tests.
- **Text** = the `--font-size` CSS variable on `<html>` — reflows prose without
  touching layout geometry or diagram sizing; clamped to 8 px … 3× base. Bound
  to `Ctrl`+`Shift`+wheel (config `text zoom in` / `text zoom out`); no default
  key. Because reflow moves content, the element at the top of the viewport is
  captured before the change and scrolled back into view after — text zoom
  keeps the reading position anchored.

**Both axes are session-scoped, not per-document.** Zoom is a live *view*
setting: once set it carries unchanged across every document switch — following
a wikilink, `:open`, `Ctrl-o`/`Ctrl-i` — including into files that have never
been opened. The per-file `zoom`/`text_zoom` in `history.toml` is the
*default on open*, read only at a window's **cold start** (the initial document,
`build_ui`), where there is no session zoom to inherit; `load_document` never
touches zoom. This is zathura's own split between "default on open" and "current
live setting" (`adjust-open` vs live zoom — `docs/research/02-zathura.md`).
Because the cold start is the sole reader, the distinction needs no extra state:
the `Shell.zoom` / `Shell.text_zoom` fields *are* the session value, seeded once
and thereafter owned by the session. Recording stays per-file and unchanged, so
reopening a note in a fresh window still lands at the zoom you last read it at.
Text zoom rides into the switch through the inlined `--font-size` (D12), so the
new document's first painted frame is already at the inherited size.

The statusbar shows `{geometric}%/{text}%T` on the right whenever either axis is
off 100%, and nothing when both are 100%. `GetState` exposes both as `zoom` and
`text_zoom`. The wheel controller lives on the **toplevel window** in capture
phase — the same architectural guarantee the key controller relies on (D4); a
controller attached to the WebView never receives the scroll events.

The scroll **percent** on the right updates live on any scroll. Keyboard scrolls
are Rust-driven and refresh the statusbar directly, but WebKit handles wheel /
touchpad / scrollbar scrolls itself and Rust never hears about them — so an
injected `scroll` listener (coalesced to one report per animation frame, and only
when the rounded percent changes) pings Rust to re-query and repaint the percent.

`Esc`/abort is the universal reset: besides leaving TOC/hint/input modes, it
clears any active search (highlight + `n`/`N` state) and any transient statusbar
notice, returning the chrome to its resting state.

The statusbar's **left** field is the jumplist breadcrumb — the route to the
document you are reading, not just its name: `index.md > topic.md > note.md`.
It is derived, never stored: `Jumplist::trail` walks the entries *behind* the
cursor and appends the live document, collapsing consecutive jumps within one
file, so following a link extends the trail and `Ctrl-o`/`Ctrl-i` shorten and
re-extend it. Overflow is cut on the **left** — whole segments are dropped
oldest-first behind a leading `…` (`core::jumplist::breadcrumb`, fitted to the
label's monospace column count), so the current filename is always visible; the
label also ellipsizes `Start` as a fallback between re-fits. `GetState` reports
the untruncated trail as `trail`, which is how the e2e suite asserts it.

Tab-completion on the `:` line **paginates** rather than truncating: candidates
are packed into pages that fit the bar (`core::command::completion_line`), the
echo shows the page holding the selection with `▸` on it, and repeated `Tab`
walks the pages in order — so every candidate is reachable, not just the first
few. The header is `[candidate/total] (page/pages)`; the page counter is
omitted when everything fits on one page.

### D6: Extensibility — pipeline seams, not a plugin ABI

Zathura's C-ABI plugin system is overkill for one format. The extensibility
seams, in order of arrival:

1. **User CSS themes** — drop a `.css` in `~/.config/jumanji/themes/`;
   hot-swappable. (v1)
2. **External fence renderers** — config maps a fence language to a command
   producing SVG/HTML on stdout (`renderers.d2 = "d2 - -"`), the same seam
   merman occupies internally. Covers graphviz, d2, typst-math, … without any
   plugin API. **(Built — decisions below.)**

   - **Placement — core, not shell.** The AST transform (`core::fence`) lives
     beside `diagram.rs`/`math.rs` and runs inside the pipeline as one more
     parse → mutate → format pass. It is the first thing in the core that spawns
     a subprocess, but that does not breach the functional-core boundary: the
     exec is local I/O with a `Result`-shaped outcome (no display, no GTK), so it
     stays unit-testable, and the transform is injectable — `transform_fences`
     takes the renderer table plus a `run` closure, so tests drive it with a
     fake while `pipeline::render` passes the real `fence::run_command`. The
     no-network rule is unaffected: subprocesses are local, and the page's CSP
     still blocks every egress from the rendered document.
   - **Contract — `sh -c` + stdin.** Each `[renderers]` entry is `language =
     "command"`; the command runs via `sh -c` with the fence body on **stdin**
     (no temp files, no `%f` substitution — kept minimal) and its **stdout**
     (SVG or HTML) replaces the fence. Language keys are normalised to lowercase
     and matched case-insensitively against the fence's first info token. Typed
     as a `BTreeMap<String,String>` on `Options`, parsed from a free `[renderers]`
     table (no `deny_unknown_fields` — any language key is valid). It is not a
     `:set` target (a table, wired once at render construction).
   - **Safety.** Hard **5 s** wall-clock timeout (child killed on expiry), **4
     MiB** stdout cap, stderr discarded. Any failure — spawn error, non-zero
     exit, timeout, over-cap, empty or non-UTF-8 output — degrades gracefully to
     the fence shown as a highlighted code block plus a styled error note,
     mirroring `diagram.rs` (reusing `.diagram-error`). Unlike `math.rs` no
     `catch_unwind` is needed: subprocess outcomes are `Result`-shaped, so there
     is no panic to contain (a crash is still structurally impossible).
   - **Output container — plain scroll box.** Output is wrapped in a
     `.rendered-fence` block that is *only* an `overflow-x: auto` scroller, so a
     wide SVG scrolls inside its own box and never the page (the same
     no-page-h-scroll invariant `.mermaid`/`.table-wrap`/`.math-scroll` keep).
     Unlike `.mermaid` there is **no intrinsic-width (`--dw`) parsing**: the
     output is arbitrary (SVG *or* HTML), so a plain scroll box is the honest
     primitive rather than over-fitting a width model to unknown markup.
   - **Trust & override.** jumanji runs whatever the user configures, exactly as
     zathura trusts its plugins — output is inlined verbatim (the CSP is the
     downstream guard). A configured `mermaid` renderer **overrides** the
     built-in merman path: `transform_fences` runs *before* `transform_mermaid`,
     so a consumed fence is no longer a `CodeBlock` when the built-in pass runs.
     Live reload re-runs the whole pipeline, so renderers re-execute for free.
3. **Trait-based document backends** (the zathura seam: outline / render /
   links per section) if other formats (AsciiDoc, rST) ever land. (v3, maybe)

### D7: Editor pairing — the SyncTeX analogue (built)

zathura's most distinctive feature maps 1:1 onto markdown. Both directions are
built; the surface is fixed below.

- **Forward (editor → reader).** A `--forward <LINE>` CLI flag plus a
  `GotoLine(line: u32)` method on the existing per-instance interface
  (`org.membranepotential.jumanji.PID-<pid>`, `src/shell/dbus.rs`). The per-PID
  model **requires** the GTK `Application` to run with `NON_UNIQUE` (set in
  `app::run`): every `jumanji <file>` is its own independent process (zathura
  semantics). Without it, GApplication's single-instance negotiation forwards a
  second launch's *activation* to the first process — reopening its own file in a
  duplicate window and colliding on the shared D-Bus object path — so distinct
  files could never be open at once. Semantics:
  scroll to the rendered element whose source line is the greatest at-or-before
  `LINE`, recording the departure on the jumplist first (a jump like any other).
  - **Second-instance routing (mirrors `--synctex-forward`):** `jumanji
    --forward N file.md` first tries to hand the jump to an instance that already
    has `file.md` open — it enumerates session-bus names under the
    `…jumanji.PID-` prefix, reads each's `GetState` `file` (reused, not a bespoke
    `GetFile`), and on the first canonical-path match calls `GotoLine(N)` and
    **exits 0 without opening a window**. No match ⇒ open normally and jump once
    the load finishes (`pending_forward`, applied in the load-finished handler,
    overriding the restored history scroll). All of this runs before any
    GTK/WebKit init, so the forwarding path needs no display.
- **Reverse (reader → editor).** A capture-phase Ctrl + primary-click user-script
  (`src/shell/view.rs`) walks up from the target to the nearest `[data-sourcepos]`
  ancestor and posts its source line over the `editorsync` script-message seam;
  the shell substitutes it into `editor-command` and spawns the editor detached
  (`gio::Subprocess`, which reaps via the main loop and never blocks the UI).
  Only Ctrl+click is intercepted (`preventDefault` + `stopPropagation`), so plain
  clicks, link routing, and text selection are untouched; every failure (bad
  line, unset `$EDITOR`, spawn error) is a statusbar notice, never a crash.
  - **`editor-command` (config option, typed).** Default `$EDITOR +%l %f`
    (zathura's synctex-placeholder style: `%l` = line, `%f` = file, `%%` = literal
    `%`). Parsed once at load into `core::editor::EditorCommand` — a typed argv
    template (`Vec` of tokens, each a sequence of literal / `%l` / `%f` segments),
    so substitution is a pure fold and a file path with spaces stays one argument
    (it fills a single `%f` token; the spawn is argv-based, never a shell). The
    shell expands a leading `$VAR` per token at spawn time (keeping env I/O out of
    the pure core). Config-only, like `[renderers]` — not a `:set` target.

- **Neovim plugin (`lua/jumanji/`, in-repo).** The editor half of D7 ships in
  this repo as a Neovim plugin at the root (`lua/jumanji/init.lua`), the fzf
  precedent: one clone serves both `cargo` and plugin managers, and the plugin
  version can never drift from the reader it drives. It adds no third surface —
  both directions ride the surfaces above:
  - *Forward:* `open()` pairs a buffer with a reader — it discovers an instance
    that has the file open exactly like the CLI does (session-bus name scan +
    `GetState` file match; the pid embedded in the bus name is the liveness
    handle) or spawns `jumanji --forward <line> <file>` detached. While that
    pid is alive, `CursorHold`/`BufWritePost` push the cursor line via
    `--forward`; when it dies, sync disarms silently (no reader resurrection
    from a stray autocmd).
  - *Reverse:* `editor-command = "nvim -l …/lua/jumanji/reverse.lua %l %f"`.
    The `-l` entry point is vimtex's inverse-search pattern: enumerate running
    instances via their default server sockets (`stdpath("run")/nvim.*`,
    skipping its own pid — even scripting mode owns a socket, and self-RPC
    deadlocks), RPC each, and the instance with the file loaded claims the
    jump (then raises its terminal via `$WINDOWID` + xdotool); no claimant ⇒
    open the file in the first reachable instance. This keeps plain `nvim`
    usable — no `--listen`, no `nvr`, no fixed socket path, any number of
    instances.

- **Source-line mapping (the SyncTeX line map).** comrak's `render.sourcepos`
  emits `data-sourcepos="startLine:col-endLine:col"` on every rendered element
  (block *and* inline), so most of the document is addressable natively with **no
  structural or CSS change** — the decisive advantage over wrapping blocks in
  marker divs (which would break the stylesheet's child/sibling selectors). The
  code-fence passes (mermaid, external fence, syntect highlight) replace their
  node with a raw `HtmlBlock`, which comrak emits verbatim *without* sourcepos —
  but those passes only swap `.value`, leaving the node's `.sourcepos` intact, so
  a single core pass (`pipeline::annotate_html_block_lines`) injects a matching
  `data-sourcepos` into each such wrapper's opening tag (synthetic table-wrap
  divs are marked line 0 and skipped). One uniform attribute across the page, so
  forward JS (`querySelectorAll('[data-sourcepos]')`, last start-line ≤ target)
  and reverse JS (walk up to nearest `[data-sourcepos]`) read the same thing.
  Document order makes start lines non-decreasing (pinned by a `core::pipeline`
  unit test), which is what forward search relies on.

### D8: Math — pulldown-latex → MathML Core, no JavaScript (M3)

LaTeX math is "the missing 5%" for a large slice of readers (notes, papers,
lecture material). The M3 target was "KaTeX-equivalent, no JS", and the pipeline
is 100% Rust (D3), so a JS math engine (KaTeX/MathJax) is out by construction.

- **Parse:** comrak's own math extension (`math_dollars` + `math_code`). `$x$`,
  `$$x$$`, and `` $`x`$ `` become inline `NodeValue::Math` nodes carrying the raw
  LaTeX — a first-class parse → mutate → format seam, identical in shape to the
  mermaid fence interception (D3). GitHub's dollar rules apply, so prose dollars
  ("costs $5 and $10") stay text (encoded in `core::math` tests as documentation).
- **Render:** **pulldown-latex 0.7.1** (crates.io, MIT) — a pure-Rust LaTeX →
  MathML Core renderer (~95% KaTeX coverage). `core::math` walks the AST and
  replaces each `Math` node with an inline raw-HTML `<math>` fragment (inline
  display style for `$…$`, block for `$$…$$`), mirroring `diagram.rs`.
  - **Rejected — typst:** pulling in a whole document compiler to typeset a
    fragment is a poor fit (huge dependency, its own markup/layout model, SVG or
    raster output rather than semantic MathML that recolors and reflows for free).
  - **Rejected — KaTeX/MathJax:** JavaScript in the content pipeline, which D3
    rules out (no bundled JS engine, no async render races, export-path hostile).
- **Display:** **WebKitGTK renders MathML Core natively** — no JS. Visual quality
  needs pulldown-latex's stylesheet plus the Latin Modern math fonts; both are
  vendored under `src/core/assets/math/` (`styles.css` + four WOFF2 files, ~0.5 MB,
  GUST Font License — see `font/LICENSE.fonts`).
- **Serving — base64 `data:` URIs, not `app://`.** There is no `app://` scheme
  in the code: D3's original plan gave way to a self-contained page (inlined CSS,
  `style-src 'unsafe-inline'`), and math stays consistent with that. `core::math`
  rewrites the stylesheet's `url('font/…woff2')` refs to base64 `data:` URIs at
  runtime (cached once), so the page fetches nothing. **CSP** gains exactly one
  token, `font-src data:` (harmless when a document has no math — nothing
  references a font). The math stylesheet is emitted only when the document
  actually contains math, so math-free pages carry none of its ~0.7 MB weight.
- **Recolor (Ctrl-r):** MathML inherits `color`, so equations recolor with the
  page for free. The one hardcoded colour in the vendored sheet — the negation
  slash's opaque-black gradient stop — is patched to `currentColor` so it stays
  visible in dark mode (marked `jumanji:` in `assets/math/styles.css`).
- **Deterministic fonts — no `local()`, unique family names (binding).** The
  vendored sheet must never consult system fonts: every `local()` source is
  removed and the embedded families are renamed to unshadowable names (`Latin
  Modern Math` → `Jumanji Math`, `LMRoman12` → `Jumanji Roman`). Why: CSS family
  names are shadowable, and Arch's `mathjax2` package registers "Latin Modern
  Math" for MathJax v2's split webfonts — MATH-table-less, huge-ascent subsets —
  which WebKit prefers over our woff2 via `local()`, then derives math layout
  constants from garbage metrics (superscripts flung line-heights above the base,
  fractions split across lines). Unique names + no `local()` keep the
  self-contained page's rendering identical across machines. Marked `jumanji:` in
  `assets/math/styles.css`; pinned by a `core::math` unit test (no `local(`,
  unique names present) and an e2e geometry probe (`msup_shift_ratio`).
- **No-page-h-scroll invariant (D5a):** display math is wrapped in a
  `.math-scroll` block (a `<span>` set to `display:block`, valid inside the
  enclosing `<p>`) so a wide matrix/alignment scrolls inside its own box, never
  the page — the same mechanism `.table-wrap` and `.mermaid` use.

### D9: stdin streaming (M3)

The last M3 item: `some-tool | jumanji` renders markdown from a pipe and
progressively re-renders as more arrives. The design reuses the live-reload
machinery wholesale rather than inventing a parallel path.

- **CLI surface.** `jumanji -` reads stdin explicitly; a bare `jumanji` with a
  piped (non-terminal) stdin and no file argument does the same (detected with
  `std::io::IsTerminal`). A bare `jumanji` at an interactive prompt has nothing
  to read and errors with a clap usage message. The file/stdin classification is
  a pure, unit-tested core type (`core::source::Source::resolve(file, is_tty)`);
  the isatty read (shell I/O) is injected, so the matrix stays testable without a
  terminal.
- **Reader — a shell thread, not core.** `shell::stdin::StdinReader` spawns a
  background thread that reads stdin into a growing `Vec<u8>` and posts ticks
  down an `mpsc` channel; a `glib::timeout_add_local` poll (120 ms, matching
  `watch.rs`'s poll cadence) drains a burst of ticks and re-renders **once** —
  the same batch-then-poll coalescing the notify debouncer gives live reload. The
  render path is `render_and_load(preserve_scroll = true)`, identical to a file
  edit, so **scroll position is preserved across streaming re-renders** by the
  existing anchor/`pending_restore` mechanism. EOF is not an error: the thread
  sends one final tick (so the last bytes render) and exits; an
  already-closed stdin (`echo x | jumanji -`) renders once and settles. Content
  is decoded per render with `from_utf8_lossy`, so a chunk boundary splitting a
  multibyte char shows a transient replacement char that self-corrects on the
  next chunk. The thread/IO plumbing is shell only — the core stays pure.
- **What a stream degrades sensibly (each interaction that assumes a path).**
  - *live-reload watcher* — skipped; there is no file to watch (the stdin reader
    replaces it).
  - *per-file history* — skipped (zathura does not persist stdin documents
    either); a stream has no stable identity to key `history.toml` on.
  - *statusbar / `GetState` file* — the label is `stdin` (and so is the stream's
    breadcrumb segment); `GetState.file` reports `stdin` too, which keeps the
    D-Bus forward-search (D7, matches on that field) from ever mistaking a
    stream for a file.
  - *reverse editor sync (`%f`)* — suppressed with a statusbar notice (no file to
    open the editor at). `--forward` for a stream is rejected in the CLI up front
    (it targets a saved source line and can hand off to an instance holding that
    file — meaningless for a pipe).
  - *relative links/images* — resolved against the **current directory** (what a
    pipe user expects): the document base is a `<cwd>/stdin.md` sentinel, so
    document-relative `img/x.png` and `.md` links resolve under the CWD exactly as
    they would for a file there. The sentinel is never read or written.
  - TOC, math, mermaid, external fence renderers, search, and marks-in-session
    all operate on the rendered pipeline output, so they work on stream content
    unchanged.
- **Not built:** persisting a stream to disk, or an `:reload` that re-reads a
  (consumed) stdin — a pipe is single-shot by nature. Opening a real file from a
  stream (`:open`, a link click) ends the stream and switches to a normal file
  document (watcher, history, editor sync all resume).
- **Graceful degradation (binding):** a parser error (pulldown-latex emits an
  inline `<merror>`) or an unbalanced group/environment (which *panics* inside
  pulldown-latex's writer, contained by `catch_unwind`) degrades to the raw
  source shown as a code span (inline) or a small error box (display) with a
  note — never a crash, never a blank page. Mirrors `diagram.rs`.

### D10: Cross-document jumplist navigation (post-1.0)

Following a `.md` link swaps the document in place (D3's single window; `:open`
does the same). Originally the jumplist (`Ctrl-o`/`Ctrl-i`, D4/M2) was reset on
every document switch, so there was no way back to the previous file — only a
per-file saved scroll position in `history.toml`. This closes that gap.

- **A jumplist entry is a `Location`, not a scroll offset.** `core::jumplist`
  now stores `{ doc: Option<PathBuf>, scroll_y: f64 }`: which document, and
  where in it. `doc == None` is the live stdin stream (no reopenable identity;
  the shell treats a `None` target as "cannot return"). The core stays pure —
  `PathBuf` is std, and the offset remains opaque to it. The push/back/forward
  algorithm is unchanged; only the payload widened.
- **The list spans documents; it is no longer reset on a switch.** Opening a
  file (link or `:open`) records the departure `Location` on the jumplist first
  (via `open_file`), then loads the new document as the live position. `Ctrl-o`
  walks back — scrolling in place when the target names the current document,
  else reopening its file at the recorded offset (`load_document`, split out of
  the old `open_file` so jump navigation reuses the load path without its own
  jumplist bookkeeping). Quickmarks/marks stay **per-document** (still reset on
  switch); only the jumplist crosses.
- **`Backspace` is a second default binding for jumplist-back** — the
  discoverable "go back" key after following a link — aliasing the `jump
  backward` action, so it is remappable via `[keys.normal]` like any binding.
- **The mouse's back/forward side buttons (8/9) are bound to the same two
  actions.** Every browser makes those buttons mean "back through what I was
  reading", and after following a wikilink that is exactly the jumplist — so
  they dispatch `JumpBackward`/`JumpForward` rather than getting a history of
  their own, and a thumb click and `Ctrl-o` cannot disagree. A capture-phase
  `GestureClick` on the toplevel with `set_button(0)`, for both the D5a reason
  (a controller on the WebView never sees these) and because WebKit would
  otherwise walk its *own* session history — which is not the history the reader
  navigates, since jumanji loads each document itself. GDK names constants only
  for the primary three buttons; 8/9 are the X11/evdev numbers, spelled out at
  the binding.
- Rejected — a *separate* document back-stack for `Backspace` distinct from the
  scroll jumplist: two overlapping histories is accidental complexity (Tar Pit).
  One document-aware jumplist serves both keys; vim's jumplist already carries a
  buffer per entry, so this is the idiomatic shape.

### D11: Obsidian dialect & vault resolution (post-1.0; implemented)

Obsidian is the largest population of markdown jumanji renders badly today:
frontmatter shows as garbage, `[[wikilinks]]` as literal text, 22 of the 27
callout spellings as `[!question]`, and `%%comments%%`/`^block-ids` leak into
the prose. `docs/research/04-obsidian.md` is the ground truth for the dialect
and for comrak 0.53's behaviour; where these decisions differ from its §6
proposal, these win.

- **Always on, and zero new config options.** The dialect is not gated on
  detecting anything: one document must render the same wherever it sits, and a
  plain folder of notes (Zettelkasten, Foam, Dendron, a docs tree) is exactly
  the case a gate would break. Markers pick the vault *root* (the `core::vault`
  bullet below); they never decide whether the dialect is on, and a tree with no
  marker at all still gets every construct. The cost is
  that a literal `[[x]]` outside a notes collection renders as inert
  unresolved-link text; inline code
  and fences are untouched (comrak parses those first), so the realistic blast
  radius is unfenced shell `[[ -z "$x" ]]` in prose. Accepted. *Rejected:* a
  `obsidian = true/false` option — it is the "config option with one caller"
  the conventions forbid, and it makes rendering depend on invisible state.
- **Four constructs are comrak flags, not code.** Turn on
  `extension.highlight` (`==x==` → `<mark>`, exactly Obsidian's semantics),
  `extension.inline_footnotes` (`^[…]`, folded into the existing footnote
  numbering — better than the parenthetical degradation the research assumed),
  `parse.relaxed_tasklist_matching` (`- [-]`/`- [?]` stop rendering as literal
  text) and `extension.front_matter_delimiter = Some("---")` (YAML properties
  stop rendering as a setext heading + thematic-break mess). `extension.alerts`
  is turned **off** — the callout pass below subsumes it. Enabling front matter
  changes one non-Obsidian case too: a document opening with a thematic break
  and containing a later `---` line loses that span. Obsidian and every static
  site generator have the same behaviour; accepted.
- **Frontmatter is hidden by default, and showing it is a rendering, not a
  dump.** Hidden is the default because a note should open as prose rather than
  as a block of machine metadata — and it is also the *free* path: comrak parses
  frontmatter into a node its formatter already skips, so hiding costs nothing
  and showing is one AST pass swapping that node for raw HTML (keeping its
  source position, so D7 reverse click still lands on it). `:frontmatter`
  toggles it live and `show-frontmatter` sets the startup state. When shown it
  is a `<dl>` properties table, not the YAML source: the reason to ask for
  frontmatter is to read the values, and Obsidian shows properties as a table
  for the same reason. `core::frontmatter` parses it with the same deliberately
  shallow rules as `parse_aliases` below — one level of `key: value`, both list
  spellings, and *verbatim* text for anything with structure it does not model
  (nested maps, `|` scalars), degrading to the whole block verbatim when nothing
  parses. It never reshapes what it did not understand. *Rejected:* a YAML crate
  (a dependency to render metadata nobody asked to see), and a `<pre>` of the
  source (which is what the toggle exists to improve on).
- **`core::vault` — one `Vault`, rooted by marker walk-up from the opened
  document, pinned at launch, index rescanned off-thread per document load.**
  *(Supersedes this section's two earlier rationales: `.obsidian/` discovery
  with a second `Loose` mode, then the process CWD.)* `vault::root_for(doc)`
  walks up from the document's directory and takes **the nearest ancestor
  holding `.obsidian/`, else the nearest holding `.git/`, else the document's
  own directory.** Each marker is searched over the whole ancestor chain before
  the next is tried, so an explicit vault marker outranks an incidental repo
  marker no matter which sits closer.
  - **Why markers, having once rejected them.** The objection to `.obsidian/`
    was that it made rendering depend on a competitor's *private directory* and
    gave one document two meanings depending on invisible state. The first half
    does not survive scrutiny: a marker directory in the user's own tree is a
    fact about how those notes are organised, not a handshake with a program —
    jumanji reads it, it does not require Obsidian to exist. The second half was
    real but was aimed at the wrong thing: two *resolution modes* were the
    problem, and there is still only one. What replaced it — the CWD — turned
    out to be worse in practice. It made resolution depend on state that is not
    in the tree at all and is invisible in the launcher, desktop entry, or file
    manager that most opens actually come through: the same file rendered
    differently depending on which directory the user's shell happened to be in.
    `.git/` as the second marker covers the marker-less notes tree, which is the
    common case for anyone keeping notes in a repo.
  - **Pinned, not recomputed.** The root is resolved once, from the document
    jumanji was launched with, and held for the process. Recomputing it per load
    would let following a wikilink into a subfolder silently narrow the vault
    under the reader — you opened a collection, not a directory. Only the
    *index* is rebuilt per load, and because the root is pinned the index
    outlives any one document: a switch only rebinds which note "this one" is.
    `scan` walks `root` into a case-folded map of *filename* → path (plus full
    relative paths) and a map of frontmatter `aliases` → path. Resolution
    follows Obsidian: vault-wide by name, **root outranks a sibling folder**,
    the source path is only a tiebreaker, matching is case-insensitive, aliases
    participate. It is a table lookup, never a path join — so `[[../secrets]]`
    and `[[/etc/passwd]]` are simply not keys, and a wikilink cannot address
    anything outside the root.
  - **The index covers the vault, not the tree it sits in.** Two filters, both
    aimed at the `.git/` fallback rooting at a source repo. **Ignore files are
    obeyed** — `.gitignore`, `.ignore`, `.git/info/exclude`, the global
    gitignore, and the same files in parent directories, via the `ignore` crate
    (ripgrep's; gitignore's negation, `**`, and precedence rules are not worth
    reimplementing). `require_git` is off, so a `.gitignore` counts in a
    marker-less or `.obsidian/`-rooted vault too: the file is the user saying
    "this is not my content", and whether git happens to be watching is beside
    the point. **Only Obsidian's accepted formats are indexed** (research §2 —
    notes, images, audio/video, PDF, `.canvas`, `.base`); nothing else can be
    named by a `[[…]]`, so indexing it buys nothing and costs an alias read
    each. `AssetKind::classify` returns `Option`, which makes that list the one
    place the formats are written down. *Rejected:* a hardcoded
    `target/`/`node_modules/` blocklist — the heuristic guess about which
    directories "don't count" that the previous revision of this bullet rightly
    refused. An ignore file is not a guess; it is already in the tree, and the
    user wrote it.
  - **Accepted consequences.** A document opened from outside the pinned root
    resolves only against that root, so a bare `[[x]]` in it may come out
    `Unresolved` — correct, not a bug: it is not part of the collection you
    opened. It still renders, and its same-file `[[#Heading]]` / `[[#^id]]`
    references still work, because those never consult the index. stdin keeps
    D9's `<cwd>/stdin.md` sentinel, so a pipe roots at the CWD as before. The
    `.git/` fallback can still root at a large repo, but the filters above mean
    the *index* is the size of the notes in it, not of the checkout.
  - **Off the main loop.** The walk is the one piece of per-load work whose cost
    is set by the tree rather than by the document, so it is the one piece that
    must not run on the UI thread: a vault behind a slow mount would otherwise
    stall every `:open` for as long as the filesystem took to answer. The shell
    runs `scan` + `VaultIndex::build` on `gio::spawn_blocking` and swaps the
    result in when it lands; that split is exactly what `VaultIndex::build`
    taking scanned entries was already for. Landing is quiet — the index is
    compared against the one in hand (`PartialEq`) and an identical one, which
    is the overwhelmingly common case, re-renders nothing; a changed one
    re-renders only if the document contains a `[[` at all. Renders never wait:
    launch starts with an empty index and the scan overlaps window creation and
    WebKit startup, so it lands well before the first load finishes. Because the
    only blocking constructor left is test-only, `Vault::rooted` is `#[cfg(test)]`
    — the type system now enforces that the reader cannot block on a walk.
    `GetState` reports `vault_files`, which is how the e2e suite waits for a
    scan to land instead of sleeping, and the first thing to look at when a
    `[[…]]` will not resolve: it says whether the vault jumanji found is the one
    you meant.
  - **Freshness:** rescanned on every document load and on `r`, reused by every
    live-reload re-render (editing a note cannot rename another one). No
    vault-wide watcher, no cache layer. Measured on this repo (dev build, warm
    cache): the previous revision walked 22 966 files in ~23 ms and spent ~126 ms
    building the tables — the ~150 ms this bullet used to quote, and mostly the
    *build*, not the walk. With ignore files and the format allowlist it is 16
    files, ~1.4 ms total, and off-thread besides. The depth (32) and file
    (50 000) caps stay: they now bound a pathological tree rather than an
    ordinary repo.
  - `aliases` is read by a ~40-line targeted parser, not a YAML crate: one key
    in three documented shapes (`aliases: x`, `[a, b]`, a `- ` block), and a
    malformed value degrades to "no aliases". The walk is local filesystem I/O
    inside the core on the `core::fence` (D6.2) precedent — `Result`-shaped, no
    display, injectable, so `VaultIndex::build` takes scanned entries and
    resolution is unit-tested without a fixture tree.
- **Wikilinks: comrak's parser, our resolution.** `wikilinks_title_after_pipe`
  is the only correct switch (Obsidian is url-first). An AST pass over
  `NodeValue::WikiLink` **percent-decodes** `NodeWikiLink.url` (comrak stores it
  `clean_url`-encoded, so `#^id` arrives as `#%5Eid`), parses it into `WikiRef`,
  resolves, and rewrites the node. Heading fragments are translated to slugs
  with the *same* `comrak::Anchorizer` `core::toc` uses — no target document is
  read, because the naive slug is also the right answer for Obsidian's
  "duplicate headings resolve to the first" rule (the first occurrence is the
  one that gets the unsuffixed slug). It is wrong only when two *differently
  spelled* headings slugify identically; noted, not chased.
  - **Emitted as three nodes, not one:** `HtmlInline("<a …>")`, a `Text` node
    holding the label, `HtmlInline("</a>")`. Folding the label into the raw HTML
    would hide it from `comrak::html::collect_text`, which both `toc::extract`
    and comrak's own heading-id renderer use — a heading containing a wikilink
    would silently get a truncated anchor. The `Text` node keeps TOC, emitted
    id, and Obsidian's own slug in agreement.
  - **Label:** the alias when given; otherwise the note name with fragment
    components joined by ` > ` (`Note > Heading`), Obsidian's display, not
    comrak's raw-target default. Aliases are rendered as plain text — Obsidian
    does not parse markdown inside link text.
  - **Unresolved links carry no `href`:** `<a class="internal-link
    is-unresolved" data-href="<raw>" title="unresolved: <raw>">label</a>`. No
    href means not clickable, not focusable, and invisible to the `f` hint
    overlay (which selects `a[href]`) — dead by construction rather than by a
    guard in the router, and never note creation (jumanji is a reader). The
    native tooltip is the feedback channel. *Rejected:* a `jumanji-unresolved:`
    pseudo-scheme routed to a statusbar notice — it invents a URI scheme and
    fills the hint list with dead entries.
- **Embeds `![[…]]` are an inline-`Text` pass, because comrak never sees them.**
  `!` consumes the following `[` as an image bracket, so the wikilink parser
  never fires and the construct survives as literal text nodes. The pass joins
  each block's consecutive `Text` siblings, scans that run, and splices the
  match — fence- and inline-code-safe by construction (those are `CodeBlock` /
  `Code` nodes, never `Text`). *Rejected:* a pre-pass over the source, which
  would have to re-implement fence and code-span lexing to know where not to
  look. The same scanner also carries `%%comments%%` and `^block-ids` (below):
  one scanner, a small `enum Construct`, separate handlers — the text-run
  splicing is the tricky part and is written once.
  - Image targets → `<img class="internal-embed" src="file://…">` with the
    `|W`/`|WxH` dimensions as attributes, capped by `max-width:100%` so a huge
    declared width scrolls nothing (D5a). The CSP already permits `img-src
    file:`.
  - **Note/heading/block transclusion is deferred** — it needs recursive
    rendering with cycle detection — and degrades to an honest **link-card**:
    `<a class="internal-embed embed-card" data-embed="note" …>` showing the
    target's display name. A real link, so click, `f` hints and the jumplist all
    work, and it visibly says "a door, not the content". PDF/audio/video/canvas
    embeds get the same card (`data-embed="pdf"`, …) and open via the system
    handler; `#page=`/`#height=` are parsed and ignored.
- **Callouts: one pass, all 27 spellings, `<details>` for folding.** A pass
  over `BlockQuote` nodes matches `[!type]` + optional `+`/`-` + optional title
  on the first line, drops that line, and wraps the quote the way `wrap_tables`
  does (`HtmlBlock` siblings, but carrying the blockquote's source line so D7
  reverse click still lands). A fold marker emits `<details class="callout"
  …><summary class="callout-title">`, open for `+`, closed for `-`; **no marker
  emits a plain `<div>`** — Obsidian shows no disclosure affordance there, and
  `<details open>` would invent one. Folding therefore costs no JavaScript (D3).
  Emission is `data-callout="<literal lowercased type>"` (Obsidian's own, so
  themes keying off it work) *plus* `class="callout callout-<canonical>"`,
  keeping the 27 → 13 alias table in typed Rust rather than a CSS selector list.
  An unknown type keeps its literal `data-callout` and falls back to
  `callout-note`, matching Obsidian. Nesting falls out of the recursive pass.
  Per-type icons are deferred; the existing `.markdown-alert` colour language is
  reused.
- **Comments and block ids.** Inline `%%…%%` is deleted by the text-run scanner;
  the block form (`%%` alone on a line) is matched between *sibling* blocks and
  the span removed — a comment region straddling a list boundary is not handled,
  which is the honest limit of an AST-level treatment. A trailing `^block-id` is
  stripped from display and replaced by `<span class="block-anchor"
  id="^37066d"></span>` inserted **before** the owning block (a standalone `^id`
  paragraph attaches to its preceding sibling), so `[[Note#^37066d]]` has
  somewhere to land. The `^` is kept in the id: heading slugs never contain one,
  so the two anchor namespaces cannot collide. The shell must **percent-decode
  the fragment** before `getElementById` — WebKit hands back `%5E`.
- **Pass order** (extends D3/D6.2): strip comments → callouts →
  `fence` → `mermaid` → `highlight` → `math` → embeds → wikilinks → block ids →
  task markers → `wrap_tables` → `annotate_html_block_lines` → `toc::extract` →
  format. Comments go first so a commented-out fence never runs a renderer.
  Every new pass either preserves the node's `.sourcepos` (block passes, which
  `annotate_html_block_lines` then picks up) or works inside a block whose own
  `data-sourcepos` is untouched (inline passes) — D7 holds. Inline sourcepos is
  lost on the rewritten inlines themselves; reverse click walks up to the
  enclosing block, so this is invisible.
- **Routing (D10, extended).** `pipeline::render` takes the `Vault` as a third
  argument (`render(md, opts, vault)`) — it is per-document state, not
  config-derived render options. A resolved wikilink is an ordinary `file://`
  link, so `open_uri` → `open_file` handles it with the jumplist push D10
  already gives `.md` links, and `f` hints pick it up with no change. One real
  gap this **supersedes**: `open_uri` today drops the fragment of a
  *cross-document* link (`other.md#section` opens the file and ignores the
  anchor) — it only honours a fragment when the base is the current document.
  D11 adds a `pending_anchor: Option<String>` applied in the load-finished
  handler exactly as `pending_forward` is, overriding the restored history
  scroll. This fixes plain markdown fragment links too.

Types, sketched (illegal states unrepresentable; "unresolved" is a variant, not
a dangling href):

```rust
// core::obsidian — a parsed, percent-decoded `[[…]]` / `![[…]]`.
pub struct WikiRef { note: Option<String>,   // None => same file, `[[#H]]`
                     fragment: Option<Fragment>,
                     pipe: Option<String>,   // alias | embed dimensions
                     kind: RefKind }         // Link | Embed
pub enum Fragment { Heading(Vec<String>), Block(BlockId) }  // `#A#B` | `#^id`

// core::vault — resolution.
pub enum Vault { Loose { dir: PathBuf }, Indexed(VaultIndex) }
pub enum Target { Note { path: PathBuf, anchor: Option<String> },  // slugified
                  Asset { path: PathBuf, kind: AssetKind },  // Image|Pdf|Av|…
                  Unresolved }
impl Vault { pub fn resolve(&self, r: &WikiRef, source: &Path) -> Target }
```

**Amendment (settled during implementation).** Three points the sketch above
left underspecified:

- **`Vault` binds the document path**, so `render(md, opts, vault)` stays
  three-argument. The sketch's `Vault::resolve(&self, r, source)` and a
  three-argument `render` cannot both hold — `render` would need a fourth
  argument for the source. Since the `Vault` is already per-document state,
  it carries the source: `struct Vault { source: PathBuf, index: VaultIndex }`.
  With the root fixed at the CWD there is exactly one state, so there is no
  `VaultKind` — a variant nothing constructs is a Tar Pit invitation.
  `VaultIndex::resolve(&self, r, source)` keeps the sketch's signature at the
  layer that must be unit-testable without a fixture tree; `Vault::resolve(&self,
  r)` delegates to it, including the `note: None` same-file case, so that rule
  lives in one place. `Vault::rooted` takes the root as an argument rather than
  reading `current_dir()` itself — the core stays free of ambient state, and a
  test can root a vault anywhere.
- **Callout titles are plain text**, and only the parts of them comrak
  reports as text survive. `> [!tip] **Bold** title` titles as `Bold title`
  (emphasis markers dropped, text kept), but a construct comrak turns into a
  node with no text of its own — raw inline HTML, and a wikilink, which this
  pass runs before — is dropped **entirely, text and all**: `> [!note] a <c> b`
  titles as `a  b`. Obsidian does parse markdown in a title; matching it would
  mean re-parsing the title fragment as inlines and re-hosting them inside a
  raw-HTML wrapper, which the `HtmlBlock` sandwich shape cannot express.
  Accepted limitation — a callout title is a label, not prose.
- **Non-`x` task markers.** `relaxed_tasklist_matching` makes comrak *parse*
  `- [-]`/`- [?]`, but `render_task_item` emits `checked` for any non-empty
  symbol — so they render as done, which is wrong (Obsidian shows the
  character). A `mark_task_symbols` pass sets `symbol = None` (honest: not
  done) and prepends `<span class="task-marker">?</span>` to the item's first
  paragraph. `x`/`X` are left alone.

**Deliberately out of scope (D11):** transclusion (link-card instead), `#tag` pills
(no search index behind them, so a pill is decoration), Templater, Bases/`.base`, Canvas
rendering, Publish-only properties, and the `internal-link` mermaid node class.
Dataview/dataviewjs/tasks/query fences need no work — `highlight_code_blocks`
already falls back to plain-text syntect for unknown languages, so they render
as code blocks today.

### D12: A document opens where it is meant to open (post-1.0; implemented)

**A reading position is part of a load, not something done to a document
afterwards.** Every position — a remembered offset, a link fragment, a
`--forward` line — used to be applied from Rust in the `LoadEvent::Finished`
handler. By then WebKit has parsed, laid out and composited the document at
scroll 0, and the correction costs a further UI→web IPC hop, so the unscrolled
top is on screen for that whole window. That is the flash the reader sees
walking the jumplist, and it is worse the more real the document is: `Finished`
waits for subresources while WebKit paints incrementally during parsing, so
images and math fonts widen the gap.

- **One position per load, resolved once.** `shell::view::InitialPosition` is
  `Top | Offset(f64) | Anchor(String) | SourceLine(u32)`, armed by whoever
  initiates the load. It replaces two `Option` fields whose precedence was
  decided by the *order of two statements* at load-finished time, and which
  could between them show three positions in one load (top → history offset →
  fragment). `Top` is the position, not the absence of one, which is why it
  carries no data.
- **The position travels as inert markup**, `data-jmnj-open` on `<html>`,
  written by the same one-shot rewrite that already pre-applies `class="dark"`
  and now also the text-zoom `--font-size`. Inline `<script>` is not available
  and should not be: the page CSP is `default-src 'none'` with no `script-src`,
  and D3 keeps the webview a dumb static renderer whose HTML can later feed an
  export path. A `data-` attribute survives that export; a script would break
  both.
- **A permanent document-start user-script applies it**, alongside the four that
  already exist — shell viewport glue, the sanctioned category, not
  content-pipeline JS. It applies on `requestAnimationFrame`, whose callbacks
  run *before* the frame they belong to is painted, re-arming on
  `DOMContentLoaded` and `load` for a document still growing, and stopping once
  the position is reached or `readyState === 'complete'`.
- **`html.jmnj-restoring body { visibility: hidden }` closes the gap the timing
  cannot**, a shell-toggled class on the same contract as `html.dark`. The
  reveal is unconditional and timer-backed: a page left permanently blank would
  be a worse bug than the flash, and graceful degradation is binding (D8/D9).
- **`LoadEvent::Finished` keeps one job**: re-running the script's *own* apply
  once subresources are in, since an offset can clamp against a document that
  has not finished growing. Idempotent, and a no-op for a document opening at
  the top.

Verified the way the 2026-07-06 flicker was: Xvfb + `ffmpeg` frame capture across
a cross-document `Ctrl-o`, with a CSS-painted marker at the top of the departed
document. Pre-fix, two captured frames are a full screen of that marker; after,
zero — what replaces them is two frames of the page's own `--bg`. The
load-finished restore passed a "lands in the right place" assertion the whole
time, which is why the e2e observable is `first_frame_scroll_y`: the offset the
*first* painted frame was placed at, recorded from inside the page.

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
- **M3:** **editor sync (done — D7: `--forward` + `GotoLine` D-Bus forward,
  Ctrl+click → `editor-command` reverse, comrak `data-sourcepos` line map)**;
  **external fence renderers (done — D6.2: `sh -c` + stdin, 5 s timeout, graceful
  degradation)**; **math (done — D8: pulldown-latex → MathML Core, no JS)**;
  **stdin streaming (done — D9: `jumanji -` / piped, reader thread + debounced
  re-render, scroll preserved, history/watch/`--forward` skipped)**; AUR package.
- **M4 (post-1.0):** cross-document jumplist (done — D10: `Location`-valued
  entries, `Backspace`); **Obsidian dialect & vault resolution (done —
  D11)** — wikilinks + vault-wide resolution (filenames, `aliases`, heading→slug,
  `#^block` anchors), image embeds, the full 27-spelling callout set with
  `<details>` folding, `==highlight==` / `%%comments%%` / frontmatter /
  inline footnotes / relaxed task markers, link-cards for deferred
  transclusion and media embeds, and cross-document fragment scroll.

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
| `Ctrl-o`/`Ctrl-i`, `Backspace` | jumplist back/forward (spans documents) | M2 |
| `:` | command line (open, set, exec; tab completion) | M2 |

## Component boundaries

Functional core, imperative shell. The core is pure and GTK-free.

```
┌─ core (pure, no GTK, unit-tested) ─────────────────────────────┐
│ pipeline.rs   md text ──comrak AST──▶ transform ──▶ HTML doc   │
│               ├─ highlight.rs  syntect adapter (two-face)      │
│               ├─ diagram.rs    ```mermaid → merman SVG inline  │
│               └─ math.rs       $…$/$$…$$ → pulldown-latex MathML│
│ toc.rs        heading extraction → outline tree + anchors      │
│ config.rs     serde+toml: typed options, key tables            │
│ keymap.rs     mode × count × key-seq → Action (pure lookup)    │
└────────────────────────────────────────────────────────────────┘
┌─ shell (gtk4-rs + webkit6) ────────────────────────────────────┐
│ app.rs        window ─ EventControllerKey(Capture) → Action    │
│ view.rs       WebView; app:// scheme (HTML + embedded assets)  │
│ bar.rs        statusbar Label + inputbar Entry                 │
│ watch.rs      notify debouncer → re-render → reload w/ scroll  │
│ stdin.rs      stdin reader thread → debounced re-render (D9)   │
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
- **WebKitGTK DMABUF-renderer layer dropouts** — on some Intel/Mesa X11 GPUs
  WebKit's DMABUF renderer intermittently drops composited layers while
  scrolling (each `overflow-x: auto` box — tables, code, diagrams — is a
  composited layer that flickers out and back). Known upstream (WebKit bug
  262607 family). **Mitigation (binding default):** the shell sets
  `WEBKIT_DISABLE_DMABUF_RENDERER=1` at process start *unless the user already
  set the variable* (any value wins, so it stays an escape hatch without a
  config option); it must run before WebKit spawns its first render process, so
  it lives at the very top of `main`. Env-var + no-GPU-headless means this can't
  be e2e-asserted; verified on evidence + upstream precedent, feel-tested on the
  real GPU before release.
- **merman parity gaps** — degrade to highlighted code block + error note;
  external-renderer seam (D6.2) as user-side fallback.
- **Editor save races** — editors rename-replace on save; watch the parent
  directory with notify-debouncer-full (~100 ms), not the file inode.
