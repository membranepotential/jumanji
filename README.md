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
| `t` | document graph: the route you took as a line, and where its notes lead (see below) |
| `m<x>` / `'<x>` | set / jump to quickmark `x` |
| `Ctrl-o` / `Ctrl-i`, `Backspace` | jumplist back / forward — spans documents, so `Ctrl-o` / `Backspace` returns to the previous file after following a link |
| `Ctrl-r` | recolor (dark mode) |
| `s` | wide blocks: break diagrams, rendered fences and tables out to window width |
| `a` | diagrams: fit to width / intrinsic size (see below) |
| `r` | reload file |
| `:` | command line (`open`, `set`, any action; `Tab` completes) |
| `Esc` | abort / back to normal mode |
| `q` | quit |

Counts work as prefixes (`5j`). Every binding is remappable in the config file.

Mouse: wheel scrolls, `Ctrl`+wheel zooms geometrically — or scales just the
diagram under the pointer, if there is one — `Ctrl`+`Shift`+wheel
zooms the text, the back/forward side buttons walk the jumplist (same as
`Ctrl-o` / `Ctrl-i`), and links are clickable (external links open in your
browser — jumanji itself never touches the network). Scroll position and zoom are
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

### Neovim plugin (two-way sync)

The repo doubles as a Neovim plugin (fzf-style: `lua/jumanji/` at the repo
root). With lazy.nvim:

```lua
{
  "membranepotential/jumanji",
  name = "jumanji.nvim",
  ft = "markdown",
  opts = {},
  keys = {
    { "<leader>cj", function() require("jumanji").open() end,
      desc = "Jumanji Reader (sync)", ft = "markdown" },
  },
}
```

- **Forward** — the keybind (or `:Jumanji`) pairs the buffer with a reader:
  it reuses the instance that already has the file open (found over the
  session bus, pid taken from its bus name) or spawns one at the cursor line.
  While the paired reader is open, `CursorHold`/`BufWritePost` keep it
  following the cursor (`opts = { live = false }` to sync only on explicit
  jumps); closing the reader disarms sync silently.
- **Reverse** — `Ctrl`+click jumps inside your *running* Neovim, no `--listen`
  or `neovim-remote` needed. Point `editor-command` at the bundled entry point
  (absolute path — template arguments get no `~`/`$VAR` expansion):

  ```toml
  [options]
  editor-command = "nvim -l /abs/path/to/jumanji/lua/jumanji/reverse.lua %l %f"
  ```

  With lazy.nvim that's
  `~/.local/share/nvim/lazy/jumanji.nvim/lua/jumanji/reverse.lua`, spelled
  out. It works the way vimtex's inverse search does: a short-lived
  scripting-mode `nvim -l` finds every running instance through its default
  server socket (`stdpath("run")/nvim.<pid>.0`), asks each over RPC, and the
  one with the file loaded claims the jump — falling back to opening the file
  in the first reachable instance.

The default `$EDITOR +%l %f` also works without any of this — it just opens a
new editor per click rather than reusing a session.

## Obsidian notes

jumanji reads Obsidian's markdown dialect — the syntax, not the app. Nothing is
gated on Obsidian being installed and there is no mode to switch on:
`[[wikilinks]]` and `![[embeds]]` (with `#Heading`, `#^block-id`, `|alias` and
`|WxH` forms), all 27 callout spellings with `+`/`-` folding, `==highlight==`,
`%%comments%%`, `^block-ids`, inline footnotes and `- [?]` task markers all
render. A link that resolves to nothing renders inert rather than broken.

**The vault root** — what a bare `[[Note]]` is looked up in — is derived from
the file you opened, once, at launch:

1. the nearest ancestor directory containing `.obsidian/`, else
2. the nearest containing `.git/`, else
3. the file's own directory.

So an Obsidian vault works as a vault, a git-tracked notes tree works without a
marker, and a loose file resolves against its neighbours. Following links never
re-roots — you opened a collection, not a directory. Resolution is Obsidian's:
vault-wide by filename, root beats a sibling folder, case-insensitive, and
frontmatter `aliases` participate.

**What gets indexed** is your notes, not your whole checkout. `.gitignore` and
`.ignore` are obeyed (in the vault and above it, whether or not the tree is a
git repo), hidden directories are skipped, and only Obsidian's accepted file
formats are indexed — notes, images, audio/video, PDF, `.canvas`, `.base`. So a
notes tree rooted at a source repo indexes the notes and ignores the source. The
scan runs off the UI thread, so opening a document never waits on it.

**Frontmatter is hidden** so a note opens as prose. `:frontmatter` shows it as a
properties table (or set `show-frontmatter = true` to start that way).

## Document graph

`t` draws the notes around the one you are reading — the breadcrumb only shows
the route you took; this shows where else you could go. The route from the
first document of your session to the current one is a straight line in the
accent colour, one column per step; everything else hangs above and below it,
and nothing jumps around between openings. A step no link explains (`:open`, a
jump from the graph) is dashed.

Two views, `v` switches (option `graph-view`, default `links`); the statusbar
names the one on screen (`Graph: links`):

- **links** — what the current note leads to: its links fan out to the right,
  one item per link (a link back to a note on the route is there too). The
  other route notes' links fold into a `+n` cluster above and below the line.
  A cluster, or a note with `+n` links of its own, expands in place; hovering
  one (or the `+n` badge) lists the titles it holds.
- **tree** — every note reachable from where you started, once each, under the
  first note that reached it. Selecting a note outlines the notes it links to
  and fades everything else off the route.

Inside the graph:

| Key | Action |
|---|---|
| `j` / `k` | next / previous item in the column |
| `l` | expand the selected item, else select a child (the route first) |
| `h` | collapse the selected item, else select its parent |
| `Enter`, double-click | open the selected note (on a cluster: expand it) |
| `v` | switch between the links and the tree view |
| click | select an item; on a cluster or a `+n` badge, expand it |
| `+` / `-` / `=` | zoom in / out / back to 1:1 on the current note |
| wheel, `Shift`+wheel, drag | pan (up/down, sideways, freely) |
| `Ctrl`+wheel | zoom at the cursor |
| `:` | command line (`:set graph-view tree` redraws the open graph) |
| `t`, `Esc` | close, back to normal mode |

The graph opens with the whole route and the current note's links in view
when they fit the window, else with the current note a third of the way
across. The panel bottom left shows the selected note's title and path, and
the route as a breadcrumb: click a segment to select that note. Zoomed out,
the graph shows less but stays readable: file names go first, then sibling
notes without links of their own merge into one `12 notes` bar, and the labels
that remain keep a readable size — columns shrink less than rows, labels are
shortened to fit a column, and where two would still collide, the one nearer
the route wins (hover shows the other).

Opening a note goes through the same jumplist as a clicked link, so
`Backspace` / `Ctrl-o` returns. Markdown links and wikilinks resolve exactly as
they do when reading; only `.md`/`.markdown` targets become items. All of it —
the walk, the layout, the SVG — is Rust in `core::graph`; no bundled JS
library.

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
show-frontmatter = false # show YAML frontmatter as a properties table
page-width = 960        # px, content column width
wide-blocks = "diagrams,fences,tables"  # kinds that may break out (see below)
wide = true             # start with the breakout on (`s` toggles it)
diagram-fit = false     # start with diagrams fit to width (`a` toggles it)
graph-view = "links"    # document graph: "links" or "tree" (`v` toggles it)
background = false      # detach from the terminal at startup (see below)
editor-command = "$EDITOR +%l %f"  # reverse editor sync (Ctrl+click), %l line / %f file

[keys.normal]
"J" = "section next"
"K" = "section previous"

[keys.graph]             # remap the document graph's own mode (`t` opens it)
"o" = "graph open"

[renderers]              # optional: fence language → shell command (stdin → stdout)
d2 = "d2 - -"            # ```d2 fences rendered with d2lang.com
dot = "dot -Tsvg"        # ```dot fences rendered with Graphviz
```

### Wide blocks

A 1800 px diagram read through a 960 px column is mostly scrollbar, while the
rest of a wide monitor sits empty. `s` (or `:toggle wide`) lets the selected
block kinds break out of the reading column and span the window instead, less a
small gutter; prose keeps its bounded measure. Toggling is instant — nothing
re-renders and the reading position does not move.

`wide-blocks` picks which kinds are eligible: `none`, `all`, or any
comma-separated mix of

| kind | blocks |
|---|---|
| `diagrams` | mermaid diagrams |
| `fences` | output of external fence renderers |
| `tables` | GFM tables |
| `code` | highlighted code blocks |
| `math` | display math (`$$…$$`) |

Both keys are also runtime `:set` targets (`:set wide-blocks all`, `:set wide
false`), and both take effect on the next render. Only top-level blocks break
out — one nested in a list, a callout or a blockquote stays in its container,
and on a window narrower than the reading column the breakout does nothing at
all. The page itself never scrolls horizontally either way.

### Diagram zoom

Two affordances for a diagram that is still bigger than the window after the
breakout:

- **`a`** (or `:toggle diagram fit`) switches every diagram in the document
  between **fit to width** — scaled down so all of it is visible — and its
  intrinsic size, which is the default. A diagram already smaller than its box
  is never blown up to fill it. Instant, like `s`: nothing re-renders.
- **`Ctrl`+wheel over a diagram** scales *that* diagram instead of zooming the
  page, in `zoom-step` multiples between 0.25× and 8×. A scaled-up diagram
  scrolls inside its own box (which is why the page still never scrolls
  horizontally), and `=` clears the scales along with both zoom axes. It is a
  look-closer gesture, so it does not survive a reload.

`diagram-fit` sets the startup state and is a runtime `:set` target
(`:set diagram-fit true`).

### Running in the background

By default jumanji holds the terminal until you close the window, like any other
command. Set `background = true` (or pass `--background`) and it detaches at
startup instead — the reader opens and the prompt comes straight back, without
the trailing `&`:

```sh
jumanji --background notes.md   # detach: prompt returns immediately
jumanji --foreground notes.md   # stay in front, even with background = true
```

Whichever flag you pass wins over the config file; passing both leaves the last
one standing. The detached reader keeps running after the terminal closes, and
its output goes nowhere — so startup diagnostics (a missing file, a broken
config, a `--forward` handoff) are all reported *before* it detaches. Reading
from a pipe never detaches: the shell is still feeding that stdin.

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

## Development

```sh
cargo test                   # core + controller unit tests, then the headless e2e suite
cargo test --test e2e        # just the e2e suite (real Xvfb + WebKit + D-Bus)
cargo clippy -- -D warnings
scripts/bench-compare.sh     # perf A/B: latest tag vs. your tree
```

The code is three layers — `src/core/` (pure Markdown → HTML, config,
keymap), `src/controller/` (the toolkit-agnostic reader: session, dispatch,
viewport behaviour as JS, generic over three small traits) and `src/shell/gtk/`
(GTK4 + WebKitGTK, wiring only). `docs/DESIGN.md` is the decision record and
binding; `docs/TESTING.md` covers the test layers and the performance guard.

CI runs on every push and pull request: `ci` (fmt, clippy, unit tests, the
e2e suite in an Arch container) and `bench` (criterion, startup timing, and
the pipeline's instruction count per render, which gates at 105 %). Each bench
run's numbers and the accumulated trail are workflow artifacts —
`docs/TESTING.md` says how to fetch them.

## Documentation

Design decisions, research, and a development log live in [`docs/`](docs/):
[DESIGN.md](docs/DESIGN.md) is the architecture decision record,
[DEVLOG.md](docs/DEVLOG.md) chronicles progress, and
[research/](docs/research/) holds the full landscape/architecture research the
design is based on.

## License

[MIT](LICENSE).
