# Editor pairing

jumanji pairs with an editor the way zathura pairs with a TeX editor through
SyncTeX: the editor scrolls the reader to its cursor line, and a Ctrl+click in
the reader opens the editor at that line. A Neovim plugin ships in the repo.

Decisions on this page:

- [D7: Editor pairing — the SyncTeX analogue (built)](#d7-editor-pairing--the-synctex-analogue-built)

Code: [`src/core/editor.rs`](../../src/core/editor.rs),
[`src/shell/gtk/dbus.rs`](../../src/shell/gtk/dbus.rs),
[`src/controller/scripts.rs`](../../src/controller/scripts.rs),
[`lua/jumanji/`](../../lua/jumanji/init.lua).
Research: [02 — zathura](../research/02-zathura.md).

## D7: Editor pairing — the SyncTeX analogue (built)

zathura's most distinctive feature maps 1:1 onto markdown. Both directions are
built; the surface is fixed below.

- **Forward (editor → reader).** A `--forward <LINE>` CLI flag plus a
  `GotoLine(line: u32)` method on the existing per-instance interface
  (`org.membranepotential.jumanji.PID-<pid>`, [`src/shell/gtk/dbus.rs`](../../src/shell/gtk/dbus.rs)). The per-PID
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
  ([`src/controller/scripts.rs`](../../src/controller/scripts.rs)) walks up from the target to the nearest `[data-sourcepos]`
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

- **Neovim plugin ([`lua/jumanji/`](../../lua/jumanji/), in-repo).** The editor half of D7 ships in
  this repo as a Neovim plugin at the root ([`lua/jumanji/init.lua`](../../lua/jumanji/init.lua)), the fzf
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
