# Architecture

This page holds what every feature rests on: the goal, the language and UI
stack, the three code layers, the Rust content pipeline, the extension seams,
and how performance regressions are judged. It also keeps the non-goals, the
milestones, the component map and the risks. The feature decisions live in
their own folders, listed in the [docs index](../README.md#design-decisions).

Decisions on this page:

- [D1: Language — Rust](#d1-language--rust)
- [D2: UI — gtk4-rs + system WebKitGTK 6, girara-style shell reimplemented](#d2-ui--gtk4-rs--system-webkitgtk-6-girara-style-shell-reimplemented)
- [D2a: Three layers — core, controller, toolkit shell (2026-09-02)](#d2a-three-layers--core-controller-toolkit-shell-2026-09-02)
- [D3: Content pipeline — 100% Rust, no JavaScript](#d3-content-pipeline--100-rust-no-javascript)
- [D6: Extensibility — pipeline seams, not a plugin ABI](#d6-extensibility--pipeline-seams-not-a-plugin-abi)
- [D13: Performance regressions are judged in instructions, felt in milliseconds (2026-09-02)](#d13-performance-regressions-are-judged-in-instructions-felt-in-milliseconds-2026-09-02)

Code: [`src/core/`](../../src/core/), [`src/controller/`](../../src/controller/),
[`src/shell/gtk/`](../../src/shell/gtk/), [`benches/`](../../benches/).
Research: [01 — landscape](../research/01-landscape.md),
[03 — Rust stack](../research/03-rust-stack.md),
[05 — macOS port](../research/05-macos-port.md).

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
  escape hatch: the entire content pipeline ([D3](#d3-content-pipeline--100-rust-no-javascript)) is UI-independent, so if
  WebKit's footprint disappoints, an egui_commonmark front end can replace the
  shell without touching the core.

### D2a: Three layers — core, controller, toolkit shell (2026-09-02)

**The shell was never thin.** [D2](#d2-ui--gtk4-rs--system-webkitgtk-6-girara-style-shell-reimplemented)'s escape hatch — "a different front end can
replace the shell without touching the core" — assumed the GTK shell would be
adapter code. By v1.8.0 `shell/app.rs` held ~1,600 lines of session logic
(the mode machine, hint mode, jumplist/quickmarks, `:` completion, the
deferred initial render, the automation surface) and `shell/view.rs` ~500
lines of JavaScript, all reachable only through the real WebKit under Xvfb.
The macOS proposal (issue #1, [`docs/research/05-macos-port.md`](../research/05-macos-port.md)) made the cost
concrete: a second shell would have had to copy all of it.

So the reader is three layers, not two:

- **[`core/`](../../src/core/)** — unchanged. Pure, display-free, unit-tested. Markdown → HTML,
  TOC, config, keymap.
- **[`controller/`](../../src/controller/)** — the toolkit-agnostic imperative half. `Controller<T:
  Toolkit>` owns the session state and every flow, and drives the window
  through three small traits it defines in `controller::toolkit`:
  `Viewport` (a webview reduced to load / eval / eval-to-JSON / zoom /
  background / find / focus), `Chrome` (status line, input bar, TOC page) and
  `Host` (timers, a worker thread whose result lands on the main loop, the
  system URI handler, the selection clipboard, detached spawn, quit).
  `controller::page` composes all viewport *behaviour* — scrolling, hints,
  anchored zoom, the load-time `<html>` rewrite, the state snapshot — as JS
  the controller owns and runs through `Viewport::eval`, so it is the same on
  every toolkit by construction. `controller::scripts` holds the document-
  start user scripts, byte-identical everywhere; they post back through a
  shell-defined `window.__jmnj_post(name, payload)` rather than WebKit's
  `messageHandlers`, which a wry-hosted WKWebView owns itself. Live reload
  and stdin streaming live here too, generic over `Host`.
- **[`shell/gtk/`](../../src/shell/gtk/)** — the Linux shell: GTK4 widgets, the `webkit6` view
  implementing `Viewport`, `GtkChrome`, `GlibHost`, GTK event adapters
  (`KeyPress`, wheel, pointer, close), and the per-instance D-Bus interface,
  which calls the controller's automation surface (`state`, `execute_str`,
  `goto_source_line`). Native `FindController`, PRIMARY selection and D-Bus
  are deliberately GTK-only; a second shell provides its own or does without.

**Rules.** The controller never imports `gtk`, `glib`, `gio`, `webkit6` or
`javascriptcore` — the same rule core has, enforced by grep. Callbacks the
controller hands a toolkit are `'static` but never `Send`: the controller is
an `Rc<RefCell<…>>` on the main thread, and a toolkit whose native API
demands `Send` (wry) parks the callback and sends a key. Nothing in core, the
HTML contract (`data-jmnj-open`, `html.dark`, `jmnj-restoring`,
`--font-size`) or the CSP changed; the extraction was a zero-behaviour-change
refactor, proved by the 50-case e2e suite at every stage and by a startup and
instruction-count A/B against v1.8.0 ([D13](#d13-performance-regressions-are-judged-in-instructions-felt-in-milliseconds-2026-09-02)).

**What it buys.** The controller gets a fake toolkit and fast unit tests for
flows that previously needed a display. A macOS shell (tao + wry) becomes
`shell/mac/`, `cfg(target_os = "macos")`, tier 2, never on the Linux release
path — its design questions (in-page chrome, JS find, no D-Bus, macOS 14.2
floor) are recorded in the research note and become ADR entries only when
that shell lands.

### D3: Content pipeline — 100% Rust, no JavaScript

Markdown → HTML happens entirely in Rust before the webview sees content. The
webview is a dumb, static renderer: no bundled mermaid.js/highlight.js, no
script execution needed for content, no async render races, and the same
pipeline can later feed an export path (PDF/HTML) or a different front end.

- **Parse: comrak 0.55** — full GFM (tables, task lists, strikethrough,
  autolinks) + footnotes; mutable arena AST makes intercepting fences a
  first-class parse → mutate → format workflow; built-in syntect adapter.
  (pulldown-cmark: flat event stream makes fence interception awkward;
  markdown-rs: dormant.)
- **Highlight: syntect 5.3 + two-face** (bat's extended syntax/theme set).
  Proven, themeable, no JS. (tree-sitter-highlight: DIY per-language quality,
  no theme format.)
- **Mermaid: merman 0.8** — pure-Rust reimplementation of Mermaid.js (native
  parser, Rust ports of Dagre/fCoSE layout, 23+ diagram types, golden-snapshot
  parity tests against Mermaid 11.16). Adopted by Zed for the same purpose.
  Pre-1.0: parity gaps are possible, so diagram rendering errors must degrade
  gracefully (show the fence as a highlighted code block + error note).
  - Rejected: mmdc (needs Puppeteer + ~200 MB Chromium), QuickJS/boa + resvg
    (mermaid.js needs a layout-capable DOM — `getBBox()` — and resvg can't
    render `foreignObject`), kroki/mermaid.ink (network).
  - **On a pre-1.0 alpha, deliberately (2026-09-13).** 0.7.0 could not lex a
    `;` inside a quoted `subgraph` title — mermaid.js accepts it (its lexer
    pushes a `string` state on `"` and swallows everything to the closing
    quote), so this was a parity bug, not strictness, and it silently broke
    real documents. There is no 0.7.x patch; 0.8.0-alpha.6 fixes it. **A parity
    gap that eats a user's diagram outranks the stability of the version
    number** — the graceful-degradation rule above is what makes an alpha
    survivable here, and the diagram unit tests plus the CI instruction gate
    are what make the bump checkable. `semicolon_in_quoted_subgraph_title_parses`
    in `core::diagram` is the regression, red on 0.7.0.
  - **The renderer is built lazily,** on the first mermaid fence rather than
    per document: constructing it costs ~70 k instructions, and most documents
    have no diagram, so the eager construction (0.7's too) was a fixed tax on
    every render — visible as +4.6 % on the `prose_10k` bench and proportionally
    less on larger ones ([D13](#d13-performance-regressions-are-judged-in-instructions-felt-in-milliseconds-2026-09-02)). Lazily, that bench sits 4.3 % *under* v1.8.0.
  - **Parse failures are reported as `line:col`, not a byte offset.** merman
    says `Unexpected character at 964`; nobody counts bytes. 0.8's
    `RenderError::Parse` carries a structured `SourceSpan`, so `describe`
    resolves it against the fence body and quotes the offending line. Columns
    count characters, not bytes — a column is what the author sees.
- **Serving:** the implementation went with a fully self-contained page instead
  of the `app://` scheme sketched here — CSS is inlined (`style-src
  'unsafe-inline'`), math fonts are base64 `data:` URIs ([D8](../math/README.md#d8-math--pulldown-latex--mathml-core-no-javascript-m3)), and there is no URI
  scheme handler. Current CSP: `default-src 'none'; img-src file: data:;
  style-src 'unsafe-inline'; font-src data:`. Local images resolve relative to
  the document. (This supersedes the original `app://` plan; see [D8](../math/README.md#d8-math--pulldown-latex--mathml-core-no-javascript-m3).)

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
     beside [`diagram.rs`](../../src/core/diagram.rs)/[`math.rs`](../../src/core/math.rs) and runs inside the pipeline as one more
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
     mirroring [`diagram.rs`](../../src/core/diagram.rs) (reusing `.diagram-error`). Unlike `math.rs` no
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

### D13: Performance regressions are judged in instructions, felt in milliseconds (2026-09-02)

Two questions, two instruments ([`docs/TESTING.md`, "Performance"](../TESTING.md#performance-the-third-layer)):

- **Has the pipeline got more expensive?** Instructions retired per render,
  counted by valgrind's cachegrind through the bench binary's `--once NAME
  --repeat K` mode (two runs differing by exactly K renders, so start-up and
  one-time init cancel; rayon pinned to one thread). Deterministic to under
  1 %, so it is the number that **gates**: CI fails a push or PR at 105 %.
- **Has the reader got slower to open?** Wall clock from spawn to
  `loaded: true` over D-Bus, headless, median of N, interleaved with the
  baseline run by run ([`scripts/bench-compare.sh`](../../scripts/bench-compare.sh)). The felt number, and the
  one that spans WebKit's process spawn and the shell's own startup path — but
  it moves with the machine, so it **informs**: CI comments, never fails.

Rejected as the gate: criterion wall clock. Measured on byte-identical
binaries it read anywhere from −10 % to +140 % per bench on the development
laptop (governor `powersave`, 0.8–2.8 GHz) in every ordering tried, and
±30 % between shared-runner instances. It stays in CI as the felt number for
the pipeline, comment-only. The trail lives in the repository's own Actions
as a workflow-artifact chain; a data-only branch or a Pages site were
rejected — a branch that shares no history with `main` is not what branches
are for, and nothing about this project should publish anywhere.

## Non-goals

- Editing. Ever. Pair with an editor instead ([D7](../editor-sync/README.md#d7-editor-pairing--the-synctex-analogue-built)).
- Windows in v1. macOS is not a goal of the owner either, but the layering
  ([D2a](#d2a-three-layers--core-controller-toolkit-shell-2026-09-02)) leaves a contributor a clean, cfg-gated place to build it.
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
- **M3:** **editor sync (done — [D7](../editor-sync/README.md#d7-editor-pairing--the-synctex-analogue-built): `--forward` + `GotoLine` D-Bus forward,
  Ctrl+click → `editor-command` reverse, comrak `data-sourcepos` line map)**;
  **external fence renderers (done — [D6.2](#d6-extensibility--pipeline-seams-not-a-plugin-abi): `sh -c` + stdin, 5 s timeout, graceful
  degradation)**; **math (done — [D8](../math/README.md#d8-math--pulldown-latex--mathml-core-no-javascript-m3): pulldown-latex → MathML Core, no JS)**;
  **stdin streaming (done — [D9](../stdin/README.md#d9-stdin-streaming-m3): `jumanji -` / piped, reader thread + debounced
  re-render, scroll preserved, history/watch/`--forward` skipped)**; AUR package.
- **M4 (post-1.0):** cross-document jumplist (done — [D10](../jumplist/README.md#d10-cross-document-jumplist-navigation-post-10): `Location`-valued
  entries, `Backspace`); **Obsidian dialect & vault resolution (done —
  [D11](../obsidian/README.md#d11-obsidian-dialect--vault-resolution-post-10-implemented))** — wikilinks + vault-wide resolution (filenames, `aliases`, heading→slug,
  `#^block` anchors), image embeds, the full 27-spelling callout set with
  `<details>` folding, `==highlight==` / `%%comments%%` / frontmatter /
  inline footnotes / relaxed task markers, link-cards for deferred
  transclusion and media embeds, and cross-document fragment scroll.

## Component boundaries

Functional core, toolkit-agnostic controller, imperative toolkit shell ([D2a](#d2a-three-layers--core-controller-toolkit-shell-2026-09-02)).
The core is pure and toolkit-free; so is the controller.

```
┌─ core (pure, no toolkit, unit-tested) ─────────────────────────┐
│ pipeline.rs   md text ──comrak AST──▶ transform ──▶ HTML doc   │
│               ├─ highlight.rs  syntect adapter (two-face)      │
│               ├─ diagram.rs    ```mermaid → merman SVG inline  │
│               └─ math.rs       $…$/$$…$$ → pulldown-latex MathML│
│ toc.rs        heading extraction → outline tree + anchors      │
│ graph/        link walk → spine scene → level-of-detail SVG    │
│ config.rs     serde+toml: typed options, key tables            │
│ keymap.rs     mode × count × key-seq → Action (pure lookup)    │
│ jumplist.rs / marks.rs / history.rs / vault.rs  session models  │
└────────────────────────────────────────────────────────────────┘
┌─ controller (no toolkit; unit-tested against a fake) ──────────┐
│ toolkit.rs    traits Viewport · Chrome · Host · Toolkit        │
│ session.rs    Controller<T>: state, dispatch, hints, jumplist, │
│               :commands, deferred render, automation surface   │
│ page.rs       viewport behaviour as JS over Viewport::eval     │
│ scripts.rs    document-start user scripts, __jmnj_post seam    │
│ watch.rs      notify debouncer → re-render (generic over Host) │
│ stdin.rs      stdin reader thread → debounced re-render (D9)   │
└────────────────────────────────────────────────────────────────┘
┌─ shell/gtk (gtk4-rs + webkit6; Linux) ─────────────────────────┐
│ app.rs        window, GTK event adapters → Controller<Gtk>     │
│ view.rs       webkit6 WebView = Viewport; script router        │
│ chrome.rs     Bar + TocView + Stack = Chrome                   │
│ host.rs       glib main loop + gio services = Host             │
│ dbus.rs       per-instance automation / editor-sync interface  │
└────────────────────────────────────────────────────────────────┘
```

## Risks & mitigations

- **WebKitGTK footprint/cold-start** — *measured (2026-07, release build,
  target machine):* spawn → content ≈ **950–1050 ms**, of which the Rust
  pipeline is ~20 ms; the rest is WebKit web-process spawn (~250 ms) plus
  one-time engine warmup (~440 ms). Surgical fixes were tested and disproven
  (pre-warm, load-before-present, hwaccel/a11y toggles: ±0). A warm process
  re-loads in ~35 ms, so the honest levers are architectural, both deferred:
  a daemon/window-reuse mode over the D-Bus seam ([D7](../editor-sync/README.md#d7-editor-pairing--the-synctex-analogue-built)), or the egui escape
  hatch ([D2](#d2-ui--gtk4-rs--system-webkitgtk-6-girara-style-shell-reimplemented)). Smooth scrolling is deliberately **off** (zathura-instant
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
  external-renderer seam ([D6.2](#d6-extensibility--pipeline-seams-not-a-plugin-abi)) as user-side fallback.
- **Editor save races** — editors rename-replace on save; watch the parent
  directory with notify-debouncer-full (~100 ms), not the file inode.
