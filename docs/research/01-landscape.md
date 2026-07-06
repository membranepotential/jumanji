# Research: Desktop markdown readers/viewers — landscape survey

*Web research conducted 2026-07-06 to inform jumanji's design. Findings grouped
by category, with synthesized lessons at the end. Source URLs inline.*

## 1. Native GPU/toolkit viewers (the closest competitors)

### Inlyne — Rust, GPU-accelerated, browserless

The single most relevant existing project: a dedicated *viewer* (not editor),
explicitly built to open a `.md` file fast without a browser.

- **Architecture:** Parses markdown to HTML via **comrak**, then renders on the
  GPU via **wgpu** (glyph rendering via glyphon/cosmic-text). No webview, no
  Chromium. Syntax highlighting via **syntect**.
  ([GitHub](https://github.com/Inlyne-Project/inlyne), [lib.rs](https://lib.rs/crates/inlyne))
- **Feature coverage:** Tables, sized images, code blocks (syntect), lists,
  task lists, links, `<details>` sections, blockquotes, alignment,
  bold/italic/underline/small. Supports only "enough HTML to render ~95% of
  files" (`<br>`, `<h1>`, `<img>`); forms, buttons and most CSS are explicitly
  out of scope.
- **Missing / weak:** No LaTeX/math (open request #322), **no Mermaid**, no GFM
  alerts/callouts (#461), no footnote anchoring (#135), no fragment/anchor
  links (#406), no stdin reading (#405).
- **Maintenance:** Actively maintained. v0.5.2 May 2026, last push 2026-07-01,
  ~1,315 stars, 61 open issues.
- **What users complain about (issue tracker):** scrollbar "way too fat"
  (#426); no smooth scrolling (#352); very poor performance resizing large
  documents (#229); long words trail off-screen (#351); images dim/distorted
  (#160, #102); CJK/non-Latin glyphs render as squares (#432, #149); panics on
  clicking bare-path links (#277, #373, #388); libc-tied Linux binary (#456);
  no window-state persistence (#185).
- **Keyboard/extensibility:** Config via `inlyne.toml` (theme, font,
  page-width, scale). Navigation is scroll/arrow-based; **no vim modal
  keybindings**, no plugin system.

**Takeaway:** Inlyne proves the core thesis (fast, browserless markdown
viewing) is viable and wanted — but it is not keyboard-driven, not extensible,
and stops at a "renders most files" ceiling. That ceiling *is* the market gap.

### Ferrite — Rust + egui, native Mermaid (Dec 2025)

A brand-new native markdown *editor* with the most ambitious native rendering
to date.

- **Architecture:** Rust + **egui**, no Electron. ~15 MB binary vs 300 MB+
  Electron; "instant startup." ([getferrite.dev](https://getferrite.dev/))
- **Headline feature:** Mermaid rendered entirely in pure Rust (~6,000 LOC), no
  JS runtime — 11 diagram types.
  ([HN](https://news.ycombinator.com/item?id=46571980),
  [mermaid-js discussion #7322](https://github.com/orgs/mermaid-js/discussions/7322))
- **HN reception:** Praised for native rendering. Criticized: no LaTeX/math, no
  true WYSIWYG, missing wikilinks; one user reported **fans spinning up on
  basic formatting** (egui immediate-mode redraw cost). Native mermaid isn't
  pixel-identical to mermaid.js.

**Takeaway:** Validates native Mermaid without a JS engine — but warns that
immediate-mode GUI burns CPU/battery, and reimplementing rendering engines is a
large, imperfect surface.

### mdr (CleverCloud) — Rust, full GFM + Mermaid

Light Rust reader targeting web/native/TUI with full GFM (incl. footnotes) and
Mermaid (flowcharts, sequence, pie). Explicitly labeled "vibe coded" — low
maturity but another native+mermaid data point.
([GitHub](https://github.com/CleverCloud/mdr))

## 2. Terminal viewers — why they aren't enough

| Tool | Stack | Strengths | Why not enough |
|---|---|---|---|
| **glow** | Go, Glamour | Beautiful ANSI render, themable, TUI browser | No HTML → badges/complex tables break; no images; cramped reflow; no math/mermaid ([Terminal Trove](https://terminaltrove.com/glow/)) |
| **mdcat** | Rust | Fast, inline images (iTerm2/kitty) | **No table rendering**; one-shot, relies on pager ([adamsdesk](https://www.adamsdesk.com/posts/linux-markdown-viewers/)) |
| **frogmouth** | Python, Textual | Nav stack, history, bookmarks, TOC | Python startup latency; terminal grid caps typography ([GitHub](https://github.com/Textualize/frogmouth)) |
| **md-tui** | Rust, ratatui | **Keyboard-driven, link navigation, search + link-select modes**, real tables | Terminal grid: no math/mermaid, constrained typography ([GitHub](https://github.com/henriklovhaug/md-tui)) |

**Cross-cutting ceiling:** the character-cell grid — no proportional fonts, no
typographic hierarchy, protocol-dependent images, no rendered math or diagrams.
md-tui is the closest *interaction model* to copy; its rendering surface is the
wrong medium.

## 3. GUI editors used as readers (Electron & webview)

- **Typora** — Best-in-class live WYSIWYG; full GFM + footnotes + LaTeX +
  Mermaid. Paid (~$15), closed source, embedded browser engine, slower cold
  start, always an editor. ([typora.io](https://typora.io/))
- **Obsidian** — Chromium bundle, ~300 MB, requires a vault before viewing a
  file. Vim-mode users report input lag on repeated nav keys.
  ([forum](https://forum.obsidian.md/t/lag-in-vim-mode/89355))
- **Zettlr** — FOSS, academic focus; Electron weight, editor-first.
- **Mark Text** — FOSS live-preview editor; Electron; intermittent maintenance.
- **Apostrophe** — GTK/libadwaita, minimalist; writing-focused, modest coverage.
- **Formiko** — Python + GTK + WebKit2 preview; primarily reStructuredText.
- **Marker** — GTK3 editor with WebKit preview that had **vim keys (h,j,k,l,g,G)
  in the previewer**, plus Mermaid/Charter/KaTeX and swappable preview CSS. The
  closest prior "GUI markdown + vim + math/diagrams" — but an editor, and
  effectively unmaintained for years.
  ([GitHub](https://github.com/fabiocolacio/Marker))

**Takeaway:** Webview editors deliver the richest rendering (math+mermaid+HTML
"for free") but pay in startup, RAM, and editor-shaped UX. None offers a fast,
open-a-file, keyboard-first reading experience.

## 4. Browser / server preview tools

- **grip** — renders via the **GitHub Markdown API**: pixel-perfect fidelity,
  but requires network and hits rate limits (60/hr unauthenticated). The
  offline renderer never shipped. ([GitHub](https://github.com/joeyespo/grip))
- **go-grip** — Go reimplementation, local rendering, no API.
  ([GitHub](https://github.com/chrishrb/go-grip))
- **mdopen** — local-only, lighter grip alternative.
- **Browser extensions** — render `.md` in-tab; per-file `file://` permission
  fuss, inconsistent GFM/math support.

**Takeaway:** All route through a browser tab — heavy context switch, no
keyboard-document model. grip's fatal flaw: the network dependency.

## 5. Existing zathura-like / vim markdown viewers

Searched hard ("zathura markdown", "vim markdown viewer GUI", "keyboard driven
markdown reader"):

- **There is no zathura-for-markdown.** No markdown backend exists for zathura;
  the common workaround is markdown → pandoc → PDF → zathura.
  ([ArchWiki](https://wiki.archlinux.org/title/Zathura))
- The keyboard-driven vim-like GUI ecosystem is **web browsers** (qutebrowser,
  vimb, luakit) — great UX references, but general browsers.
- **Marker** (above) was the only GUI markdown tool with real vim nav — dead.
- **md-tui** is the only dedicated viewer with genuine keyboard navigation —
  terminal-bound.

**Conclusion:** The combination — fast native feel + dedicated reader + modal
vim control + extensibility + full GFM/math/mermaid/highlighting — **does not
exist**. That is the open gap.

## 6. Lessons learned

1. **Own the gap: "zathura for markdown" is genuinely vacant.** Position as a
   reader, not another editor.
2. **Don't hand-roll text layout.** Inlyne's tracker is dominated by the hard
   5%: CJK shaping, wrapping, selection, smooth scrolling.
3. **Copy zathura's architecture, not just its keybindings:** pluggable
   rendering seams + a config file that rebinds everything.
4. **Modal, rebindable vim navigation is the differentiator** — including
   count prefixes, `/` search, link hints, and a `:` command line.
5. **The missing 5% is the reason to switch:** math, mermaid, callouts,
   footnote anchors, fragment links. First-class, not polish.
6. **Native mermaid without JS is achievable but large** — make diagram
   rendering a pluggable seam with graceful degradation.
7. **For math, embed a real typesetting engine** — table stakes for the
   technical audience.
8. **Use a proven highlighting engine** (syntect) — one of inlyne's
   least-complained-about parts.
9. **Beware immediate-mode GUI battery cost** — idle at zero CPU.
10. **Nail the boring UX:** smooth scrolling, persisted window state, live
    reload (watch images too), stdin input, sane scrollbar.
11. **Local and offline by default. Always.**
12. **Match GFM semantics** — that's the mental model users bring.
13. **Real, hot-swappable theming** (CSS-level), plus page-width control.
14. **Self-contained binary,** AUR packaging for the Arch audience.
15. **The pitch is the zathura pitch:** open in milliseconds, drive from the
    keyboard, close. Optimize cold-start-to-readable relentlessly.
