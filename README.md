# jumanji

A markdown reader in the spirit of [zathura](https://pwmt.org/projects/zathura/):
minimal chrome, vim keybindings, instant startup, and rendering good enough that
you actually *want* to read in it.

Open a `.md` file, read it with real typography — proportional fonts, proper
tables, highlighted code blocks, rendered [Mermaid](https://mermaid.js.org/)
diagrams, and LaTeX math (`$…$` / `$$…$$`) typeset as native MathML — and drive
everything from the keyboard. Close it. That's the whole program.

> **Name lineage:** pwmt (*Programs With Movie Titles*) once shipped a
> keyboard-driven WebKit browser called [jumanji](https://pwmt.org/projects/jumanji/),
> discontinued in 2016. Zathura is — fictionally — the sequel to Jumanji, both
> Chris Van Allsburg books. This project reclaims the name for the zathura
> family's missing member: the markdown backend that never existed.

## Status

**1.0** — milestone M3 complete: real typography, vim keybindings, Mermaid and
external-tool diagrams, LaTeX math (MathML), editor pairing (SyncTeX-style),
and reading from a pipe. Linux-first (X11/Wayland via GTK4). Young, but the
whole reader loop is there; expect the occasional sharp edge.

## Why

Reading markdown in a text editor is a chore: monospace fonts, no rendering,
tables wider than the window, no diagrams. The existing alternatives each miss:

- **Terminal viewers** (glow, mdcat, md-tui) are bound to the character grid — no
  real typography, no diagrams, no math.
- **Native viewers** (inlyne) are fast but have no vim keybindings, no mermaid,
  no math, and hand-rolled text layout with a long tail of rendering bugs.
- **Editor apps** (Typora, Obsidian, Zettlr) bundle a browser engine, start
  slowly, and are editors first — reading is the afterthought.

"Zathura for markdown" did not exist. Now it does.

## Architecture in one paragraph

All content transformation happens in Rust before anything is displayed:
[comrak](https://github.com/kivikakk/comrak) parses GitHub-Flavored Markdown,
[syntect](https://github.com/trishume/syntect) highlights code fences, and
[merman](https://github.com/Latias94/merman) — a pure-Rust Mermaid
implementation — renders diagram fences to inline SVG. The finished HTML is
handed to a system WebKitGTK 6 webview, which does exactly one job: typeset it
beautifully. There is no JavaScript pipeline, no bundled browser, no network
access. Keybindings are handled by GTK4 capture-phase controllers *before* the
webview sees a keypress, so the vim layer is absolute. See
[docs/DESIGN.md](docs/DESIGN.md) for the full decision record.

## Keybindings (defaults)

| Key | Action |
|---|---|
| `j` / `k` | scroll down / up |
| `h` / `l` | scroll left / right |
| `d` / `u` | scroll half page down / up |
| `J` / `K` | next / previous section (heading) |
| `gg` / `G` / `<N>G` | go to top / bottom / section N |
| `+` / `-` / `=` | zoom in / out / reset (reset clears text zoom too) |
| `/` | search (`n` / `N` for next / previous match) |
| `Tab` | table of contents (`j`/`k` move, `l`/`h` expand/collapse, `Enter` jump) |
| `f` / `F` | follow link via hints / show link target |
| `m<x>` / `'<x>` | set / jump to quickmark `x` |
| `Ctrl-o` / `Ctrl-i`, `Backspace` | jumplist back / forward — spans documents, so `Ctrl-o` / `Backspace` returns to the previous file after following a link |
| `Ctrl-r` | recolor (dark mode) |
| `r` | reload file |
| `:` | command line (`open`, `set`, any action; `Tab` completes) |
| `Esc` | abort / back to normal mode |
| `q` | quit |

Counts work as prefixes (`5j`). Every binding is remappable in the config file.

Mouse: wheel scrolls, `Ctrl`+wheel zooms geometrically, `Ctrl`+`Shift`+wheel
zooms the text, links are clickable (external links open in your browser —
jumanji itself never touches the network). Scroll position and zoom are
remembered per file. Drop `.css` files into `~/.config/jumanji/themes/` to
restyle the reader; GFM alerts (`> [!NOTE]` …) render as callouts. LaTeX math —
inline `$…$` and display `$$…$$`, matrices and aligned environments — is
typeset to native MathML (no JavaScript), and recolors with the page.

## Editor pairing (zathura's SyncTeX, for markdown)

Round-trip navigation between your editor and the reader, both directions:

- **Forward (editor → reader):** `jumanji --forward <line> file.md` scrolls the
  reader to the element whose source line is the greatest at-or-before `<line>`.
  If an instance already has the file open, the jump is handed to it over D-Bus
  and **no new window opens**; otherwise one opens and jumps once loaded.
- **Reverse (reader → editor):** `Ctrl`+click any element to open your editor at
  its source line via the `editor-command` config option (`%l` = line, `%f` =
  file, `%%` = literal `%`; default `$EDITOR +%l %f`). Failures (unset `$EDITOR`,
  bad line, spawn error) are a statusbar notice, never a crash.

### Neovim hook

**Forward** — push the reader to your cursor line. Drop this in your config
(e.g. `after/ftplugin/markdown.lua`); `CursorHold` keeps it from spamming on
every motion (tune with `:set updatetime`):

```lua
vim.api.nvim_create_autocmd({ "CursorHold", "BufWritePost" }, {
  buffer = 0,
  callback = function()
    vim.system({ "jumanji", "--forward",
      tostring(vim.fn.line(".")), vim.fn.expand("%:p") })
  end,
})
```

**Reverse** — make `Ctrl`+click jump inside your *running* Neovim instead of
spawning a fresh one. Start Neovim listening on a known socket:

```sh
nvim --listen /tmp/nvim.pipe file.md
```

then point `editor-command` at it (needs [`neovim-remote`](https://github.com/mhinz/neovim-remote), `pipx install neovim-remote`):

```toml
[options]
editor-command = "nvr --servername /tmp/nvim.pipe +%l %f"
```

The default `$EDITOR +%l %f` also works — it just opens a new editor per click
rather than reusing a session.

## External fence renderers

Extend diagram support to any tool without a plugin API: map a fence language to
a shell command in `[renderers]`, and jumanji pipes the fence body to that
command's **stdin** and inlines the SVG/HTML it prints on **stdout** — the same
pipeline seam mermaid uses internally.

```toml
[renderers]
d2   = "d2 - -"        # ```d2   fences via https://d2lang.com  (pacman -S d2)
dot  = "dot -Tsvg"     # ```dot  fences via Graphviz            (pacman -S graphviz)
gnuplot = "gnuplot -e 'set terminal svg' -"
```

A ```` ```dot ```` fence then renders as a diagram. Details:

- The command runs via `sh -c` with the fence body on stdin — no temp files, no
  argument substitution. Language keys match the fence's first info token,
  case-insensitively.
- Hard **5 s** timeout and **4 MiB** output cap. Any failure (spawn error,
  non-zero exit, timeout, empty or non-UTF-8 output) degrades gracefully to a
  highlighted code block plus an error note — never a crash or blank page.
- A configured `mermaid` renderer **overrides** the built-in one.
- Live reload re-runs the whole pipeline, so edits re-render for free.

## Configuration

`~/.config/jumanji/config.toml`:

```toml
[options]
scroll-step = 60        # pixels per j/k
zoom-step = 0.1
default-recolor = false # start in dark mode
page-width = 960        # px, content column width
editor-command = "$EDITOR +%l %f"  # reverse editor sync (Ctrl+click), %l line / %f file

[keys.normal]
"J" = "section next"
"K" = "section previous"

[renderers]              # optional: fence language → shell command (stdin → stdout)
d2 = "d2 - -"            # ```d2 fences rendered with d2lang.com
dot = "dot -Tsvg"        # ```dot fences rendered with Graphviz
```

## Installation

Requires GTK4 and webkitgtk-6.0 (Arch: `pacman -S gtk4 webkitgtk-6.0`).

### Arch Linux / AUR

Not yet published to the AUR. Until it is, build the package straight from
this repo:

```sh
cd packaging/aur
makepkg -si
```

This builds jumanji as a real Arch package (binary in `/usr/bin`, a desktop
entry so it shows up as a `.md` handler, and `/usr/share/doc/jumanji/config.example.toml`
as a starting point for `~/.config/jumanji/config.toml`) and installs it with
pacman, so it upgrades/removes cleanly like any other package.

### From source (any distro)

```sh
cargo build --release
./target/release/jumanji README.md
./target/release/jumanji -      # read markdown from stdin
some-tool | ./target/release/jumanji   # or just pipe it (renders as data streams in)
```

## Documentation

Design decisions, research, and a development log live in [`docs/`](docs/):
[DESIGN.md](docs/DESIGN.md) is the architecture decision record,
[DEVLOG.md](docs/DEVLOG.md) chronicles progress, and
[research/](docs/research/) holds the full landscape/architecture research the
design is based on.

## License

[MIT](LICENSE).
