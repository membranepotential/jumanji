# jumanji

A markdown reader in the spirit of [zathura](https://pwmt.org/projects/zathura/):
minimal chrome, vim keybindings, instant startup, and rendering good enough that
you actually *want* to read in it.

Open a `.md` file, read it with real typography — proportional fonts, proper
tables, highlighted code blocks, rendered [Mermaid](https://mermaid.js.org/)
diagrams — and drive everything from the keyboard. Close it. That's the whole
program.

> **Name lineage:** pwmt (*Programs With Movie Titles*) once shipped a
> keyboard-driven WebKit browser called [jumanji](https://pwmt.org/projects/jumanji/),
> discontinued in 2016. Zathura is — fictionally — the sequel to Jumanji, both
> Chris Van Allsburg books. This project reclaims the name for the zathura
> family's missing member: the markdown backend that never existed.

## Status

Early development. The MVP targets Linux (X11/Wayland via GTK4). Expect sharp
edges.

## Why

Reading markdown in a text editor is a chore: monospace fonts, no rendering,
tables wider than the window, no diagrams. The existing alternatives each miss:

- **Terminal viewers** (glow, mdcat, md-tui) are bound to the character grid — no
  real typography, no diagrams, no math.
- **Native viewers** (inlyne) are fast but have no vim keybindings, no mermaid,
  and hand-rolled text layout with a long tail of rendering bugs.
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
| `gg` / `G` | go to top / bottom |
| `+` / `-` / `=` | zoom in / out / reset |
| `/` | search (`n` / `N` for next / previous match) |
| `Tab` | table of contents |
| `Ctrl-r` | recolor (dark mode) |
| `r` | reload file |
| `:` | command line |
| `Esc` | abort / back to normal mode |
| `q` | quit |

Counts work as prefixes (`5j`). Every binding is remappable in the config file.

Mouse: wheel scrolls, `Ctrl`+wheel zooms, links are clickable.

## Configuration

`~/.config/jumanji/config.toml`:

```toml
[options]
scroll-step = 60        # pixels per j/k
zoom-step = 0.1
default-recolor = false # start in dark mode
page-width = 720        # px, content column width

[keys.normal]
"J" = "section next"
"K" = "section previous"
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
```

## Documentation

Design decisions, research, and a development log live in [`docs/`](docs/):
[DESIGN.md](docs/DESIGN.md) is the architecture decision record,
[DEVLOG.md](docs/DEVLOG.md) chronicles progress, and
[research/](docs/research/) holds the full landscape/architecture research the
design is based on.

## License

[MIT](LICENSE).
