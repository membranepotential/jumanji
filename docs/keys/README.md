# Keys and config

jumanji is driven by modal, zathura-style keys with counts, and every binding
and option lives in one TOML file. This page records how key events reach the
dispatcher, what the config file can say, and the default key map.

Decisions on this page:

- [D4: Keybindings — GTK capture phase, zathura semantics](#d4-keybindings--gtk-capture-phase-zathura-semantics)
- [D5: Config — TOML, zathura idioms](#d5-config--toml-zathura-idioms)
- [Keybinding spec (M1 + M2)](#keybinding-spec-m1--m2)

Code: [`src/core/keymap.rs`](../../src/core/keymap.rs),
[`src/core/config.rs`](../../src/core/config.rs),
[`src/shell/gtk/app.rs`](../../src/shell/gtk/app.rs) (the capture-phase
controllers), [`resources/config.example.toml`](../../resources/config.example.toml).
Research: [02 — zathura](../research/02-zathura.md).

## D4: Keybindings — GTK capture phase, zathura semantics

GTK4 dispatches key events capture-phase from the window down before the target
widget, so an `EventControllerKey` with `PropagationPhase::Capture` on the
toplevel handles vim keys *before* WebKit — architecturally guaranteed, no
focus fights. Dispatch is girara-style: `mode × count × key-sequence → Action`,
count-prefix handling done once in the dispatcher, never per-binding.
Scrolling/zoom drive the webview via `webkit6` APIs and small JS snippets
(`window.scrollBy`, anchor jumps). Search is JS the controller owns too
(`controller/assets/search.js`): it searches the rendered text, paints matches
with the CSS Custom Highlight API, and posts the match count back; see
[D2a](../architecture/README.md#d2a-three-layers--core-controller-toolkit-shell-2026-09-02).
Like zathura, the statusbar shows `[Search 3/12]` while a search is active, a
search that finds nothing says `Pattern not found: <query>`, and `Esc` drops
the search.

## D5: Config — TOML, zathura idioms

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
| `highlight-color` | colour (`"rgba(159, 251, 0, 0.5)"`) | colour of a selection and of every `/` match; zathura's option and default. `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb(…)`, `rgba(…)` |
| `highlight-active-color` | colour (`"rgba(0, 188, 0, 0.5)"`) | colour of the current `/` match, the one `n`/`N` step from; zathura's option and default. Same forms as `highlight-color` |
| `copy-on-select` | bool (`false`) | copy a selection when the mouse is released |
| `selection-clipboard` | `"primary"` \| `"clipboard"` (`primary`) | which clipboard copy-on-select writes to |
| `background` | bool (`false`) | detach from the terminal at startup, so the prompt returns immediately; startup-only, and `--background`/`--foreground` override it |

Font names are CSS-escaped and quoted before emission into the generated
`:root{…}` block (the stylesheet already consumes `--font-body`/`--font-mono`/
`--font-size`). Copy-on-select is opt-in (`copy-on-select`, off by default), a
deliberate deviation from zathura, which always copies: a selection should not
overwrite your Ctrl-V clipboard unless you ask for it. Two copies happen without
it, both WebKit's own: Ctrl-C copies to CLIPBOARD (the key is unbound, so it
passes through to the view), and every selection claims X11 PRIMARY
(`WebEditorClient::respondToChangedSelection` calls `updateGlobalSelection` on
GTK unconditionally; no setting turns it off). With the option on, a `UserContentManager` script
message handler + injected user-script post the current non-empty selection to
Rust on **`mouseup`** — the end of a real pointer-selection gesture — which
writes it to the configured GDK clipboard. Keying off `mouseup` (not
`selectionchange`) copies only what the pointer selected, never a selection a
script set. Search leaves every clipboard alone by construction: it paints
highlights and never touches the DOM selection. (WebKit's native find did select
each match, which copied it into PRIMARY; the shell had to restore PRIMARY after
every `/`, `n` and `N`. That workaround went with the native find.) The input bar
also takes focus without selecting its own text, which would claim PRIMARY.

`background` detaches by **re-executing the binary**, not by forking: the process
is about to bring up GTK, WebKit and D-Bus, and `fork` in a soon-to-be-threaded
program is a well-known footgun, while a fresh process starts from a clean slate.
The child gets the original argv plus a trailing `--foreground` (the two flags
override each other last-one-wins, so that also neutralises an explicit
`--background`), its own process group, and null stdio — a detached process that
outlives its terminal must not write to a closed tty. The detach happens as the
last step of startup, after every diagnostic the user needs to see on the
terminal, and never for a stdin source, whose pipe we are still consuming.

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
| `t` | document graph (`hjkl` or arrows move/expand, `v` view, `Enter` open; [D14](../graph/design.md#d14-the-document-graph--a-route-spine-with-its-links-fanned-out-2026-09-18)) | post-1.0 |
