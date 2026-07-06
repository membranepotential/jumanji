# Research: zathura architecture & UX

*Web research conducted 2026-07-06. Primary sources: pwmt.org, zathura/girara
git repos, Arch man pages.*

## 0. Naming: the historical jumanji

pwmt = **"Programs With Movie Titles"**: Zathura (2002) and Jumanji (1981) are
both Chris Van Allsburg books, and Zathura is fictionally a sequel to Jumanji.
pwmt's original **jumanji** was "a highly customizable and functional web
browser based on libwebkit and gtk+", styled after vimperator — keyboard-first,
minimal chrome, girara-based. Discontinued 2016-02-03; GitHub mirror archived.
This project reclaims the name for the zathura family's missing member.
([project page](https://pwmt.org/projects/jumanji/),
[archived repo](https://github.com/pwmt/jumanji))

## 1. Architecture

### 1.1 girara — the shared UI library

Historically supplied three components:

- **The view** — widget holding the application content.
- **The input bar** — entry for `:commands` (also `/`, `?` search), with
  tab-completion driven by per-command completion callbacks.
- **The status bar** — bottom bar of dynamically-updatable text fields
  (filename, position, mode) that also receives mouse events.

Plus: **keybinding dispatch** (`map`/`unmap`, multi-key "buffered commands"
like `gg`, numeric-prefix counts like `5j` handled once in the dispatcher,
mode-scoped bindings), **config parsing** (`set`/`map`/`unmap`/`include`
mini-language with typed settings INT/FLOAT/STRING/BOOL/color), a **settings
registry** shared by code and config, and mouse-event plumbing.

**Important recent development:** the GTK widgets have been folded back into
zathura itself (GTK4 port); standalone girara today is documented as "common
datastructures and utilities" only.
([girara](https://pwmt.org/projects/girara/),
[README](https://raw.githubusercontent.com/pwmt/girara/develop/README.md))

### 1.2 Plugin system (C ABI)

Zathura contains no format-specific code; document support is dynamically
loaded shared objects:

- Startup scans a plugin dir for `.so` files; each self-registers via
  `ZATHURA_PLUGIN_REGISTER` (name, semver, registration callback, MIME list).
  Files are matched by libmagic MIME type, not extension.
- Registration fills a `zathura_plugin_functions_t` struct: minimally
  `document_open/free`, `page_init/clear` (dimensions), `page_render_cairo`.
  Optional: text extraction, link extraction, outline/TOC, image export, page
  labels, form fields.
- Opaque-pointer data ownership: plugins stash state via
  `zathura_document_set_data()`; core never reaches into plugin internals.
  Load-time API-version compatibility check.

**Relevance to jumanji:** the ABI is overkill for one format, but the **narrow,
page-oriented interface boundary** is the thing to steal — a backend answers
"how many sections," "render this one," "what are its links/anchors," "what's
the outline." A compiled-in trait is the right-sized version.
([plugin dev docs](https://pwmt.org/projects/zathura/plugins/development/))

### 1.3 Rendering model — page cache + render threads

From `zathura/render.c`:

- A `GThreadPool` renders off the GTK main thread; queued jobs sorted so
  aborted jobs surface first and get discarded cheaply.
- Render requests are de-duplicated; each job has an atomic `aborted` flag so
  scroll-past cancels cheaply.
- Completion marshalled to the main context via `g_main_context_invoke()` —
  workers never touch GTK widgets.
- **Page cache**: fixed-size (`page-cache-size`, default 15), LRU eviction by
  `last_view_time`; viewing bumps recency.

This is why zathura feels instant: nearby pages are pre-rendered at the current
zoom; the UI thread never blocks on rasterization.
([render.c](https://github.com/pwmt/zathura/blob/master/zathura/render.c))

### 1.4 Data/storage layer

- Bookmarks, per-file view state, and input history via a pluggable database
  backend (`sqlite|plain|null`, default sqlite) at `$XDG_DATA_HOME/zathura/`.
- **Jump list** (Ctrl-O/Ctrl-I, `jumplist-size` default 2000) per document.
- **D-Bus** service `org.pwmt.zathura.PID-<pid>`: `GotoPage(uint32)` method +
  document properties — lets external tools drive a running instance.
- **SyncTeX**: forward sync via `--synctex-forward` CLI flag → D-Bus; backward
  sync shells out to `synctex-editor-command` with `%{input}`/`%{line}`
  substitution on modifier-click (default ctrl).

## 2. UX in detail

### 2.1 Modes

Explicitly modal: **normal**, **fullscreen** (`F11`), **presentation** (`F5`),
**index** (`Tab`, tree view over the outline). `map` is mode-scoped. `Esc`
(`abort`) always returns to normal — the universal escape hatch.

### 2.2 Mouse behavior

- Wheel → scroll; **Ctrl+wheel → zoom** (the "feels native" gesture).
- Middle-drag → pan. Left-click link → follow; left-drag → selection (primary
  clipboard, configurable). Shift+drag → highlight. Right-click → context menu.

### 2.3 Config format (zathurarc)

```
set <option> <value>            # typed: INT/FLOAT/STRING/BOOL/color
map [mode] <key> <function> [arg]
unmap [mode] <key>
include <path>
```

Keys: bare chars, bracketed specials (`<Space>`, `<C-r>`, `<S-space>`), mouse
buttons (`<Button1>`). Multi-char sequences map as buffered commands; numeric
prefixes are repeat counts for free.

### 2.4 What makes it feel fast & minimal

- **Chrome is opt-in:** default shows only the statusbar; inputbar appears on
  `:`. No menus, toolbars, tabs. Single document, single window.
- **No modal dialogs** — `:`-commands with tab completion instead.
- **Instant perceived response** from the render-thread + LRU cache design.
- **Recolor (`Ctrl-r`) is a first-class fast operation** — fg/bg remap at
  render time (`recolor-keephue`, `recolor-reverse-video`), not a theme system.
- **Everything keyboard-addressable**, including chrome toggles.

## 3. Default keybindings worth replicating

| Key(s) | Action |
|---|---|
| `j`/`k` | scroll down/up |
| `h`/`l` | scroll left/right |
| `J`/`K` | next/previous page |
| `gg` / `G` / `<N>G` | first / last / page N |
| `/`, `?` | search forward/backward |
| `n`/`N` | next/previous match |
| `f` | follow link (hint labels) |
| `F` | display link target |
| `Tab` | index/TOC view |
| `+`/`-`/`=` | zoom in/out/reset |
| `a` / `s` | fit page / fit width |
| `r` | rotate |
| `Ctrl-r` | recolor (dark mode) |
| `R` | reload |
| `d` | dual-page toggle |
| `m<X>` / `'<X>` | set / jump quickmark |
| `F11` / `F5` | fullscreen / presentation |
| `:` | command line |
| `Esc` | abort |
| `q` | quit |
| `Ctrl-o`/`Ctrl-i` | jumplist back/forward |

Index mode: `l`/`h` expand/collapse, `zO`/`zC`/`zR`/`zM` recursive variants.

## 4. zathurarc idioms worth imitating

1. **Three verbs only**: `set`, `map`/`unmap`, `include`.
2. **Typed settings registry** (type + default + validator per key).
3. **Mode-scoped keybindings** as a first-class map argument.
4. **Count/buffered-sequence handling done once** in the dispatcher.
5. **`include` for config splitting** — cheap, disproportionately useful.
6. **Colors as CSS-ish strings** — theme snippets copy-paste cleanly.
7. **Compact chrome toggle** (`guioptions`-style).
8. **Separate "default on open" from "current live setting"** (e.g.
   `adjust-open` vs live zoom).
9. **`exec` escape hatch** with `$FILE`/`$PAGE` expansion.
10. **A single universal `abort` (Esc)** in every mode.
11. **SyncTeX-style editor pairing** — CLI flag + D-Bus for forward sync,
    modifier-click shelling to `$EDITOR +LINE FILE` for reverse. Maps 1:1 onto
    a markdown reader paired with Neovim.

## Sources

- https://pwmt.org/projects/zathura/ (+ documentation, plugin docs)
- https://pwmt.org/projects/girara/ · https://github.com/pwmt/girara
- https://pwmt.org/projects/jumanji/ · https://github.com/pwmt/jumanji
- https://github.com/pwmt/zathura (render.c, dbus-interface.c)
- https://man.archlinux.org/man/extra/zathura/zathura.1.en
- https://man.archlinux.org/man/extra/zathura/zathurarc.5.en
- https://wiki.archlinux.org/title/Zathura
