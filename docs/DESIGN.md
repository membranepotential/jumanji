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
  (`org.membranepotential.jumanji.PID-<pid>`, `src/shell/dbus.rs`). Semantics:
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
  - *statusbar / `GetState` file* — the label is `stdin`; `GetState.file` reports
    `stdin` too, which keeps the D-Bus forward-search (D7, matches on that field)
    from ever mistaking a stream for a file.
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
