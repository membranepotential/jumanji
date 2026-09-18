# Cross-document jumplist

`Ctrl-o`/`Ctrl-i`, `Backspace` and the mouse's side buttons walk one jumplist
that spans documents, so following a link can always be undone. The status
line's breadcrumb and the document graph both read their route from it.

Decisions on this page:

- [D10: Cross-document jumplist navigation (post-1.0)](#d10-cross-document-jumplist-navigation-post-10)

Code: [`src/core/jumplist.rs`](../../src/core/jumplist.rs),
[`src/controller/session.rs`](../../src/controller/session.rs).
Research: [02 — zathura](../research/02-zathura.md).

## D10: Cross-document jumplist navigation (post-1.0)

Following a `.md` link swaps the document in place ([D3](../architecture/README.md#d3-content-pipeline--100-rust-no-javascript)'s single window; `:open`
does the same). Originally the jumplist (`Ctrl-o`/`Ctrl-i`, [D4](../keys/README.md#d4-keybindings--gtk-capture-phase-zathura-semantics)/M2) was reset on
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
  `GestureClick` on the toplevel with `set_button(0)`, for both the [D5a](../reading/README.md#d5a-two-axis-zoom) reason
  (a controller on the WebView never sees these) and because WebKit would
  otherwise walk its *own* session history — which is not the history the reader
  navigates, since jumanji loads each document itself. GDK names constants only
  for the primary three buttons; 8/9 are the X11/evdev numbers, spelled out at
  the binding.
- Rejected — a *separate* document back-stack for `Backspace` distinct from the
  scroll jumplist: two overlapping histories is accidental complexity (Tar Pit).
  One document-aware jumplist serves both keys; vim's jumplist already carries a
  buffer per entry, so this is the idiomatic shape.
