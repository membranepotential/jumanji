# Reading position, zoom and wide blocks

These decisions keep the reader's place and give pictures room. Zoom has two
axes that hold the reading position; any change that alters a block's height
is anchored; diagrams, fences and tables can break out of the reading column;
and every document opens at its intended position without a flash.

Decisions on this page:

- [D5a: Two-axis zoom](#d5a-two-axis-zoom)
- [D5a.0: Anything that changes a block's height must be anchored (2026-09-13)](#d5a0-anything-that-changes-a-blocks-height-must-be-anchored-2026-09-13)
- [D5a.1: Wide blocks — pictures get the window, prose keeps the measure (2026-09-13)](#d5a1-wide-blocks--pictures-get-the-window-prose-keeps-the-measure-2026-09-13)
- [D5a.2: Diagram fit and per-diagram zoom (2026-09-13)](#d5a2-diagram-fit-and-per-diagram-zoom-2026-09-13)
- [D12: A document opens where it is meant to open (post-1.0; implemented)](#d12-a-document-opens-where-it-is-meant-to-open-post-10-implemented)

Code: [`src/controller/page.rs`](../../src/controller/page.rs) (the anchor and
zoom JS), [`src/controller/scripts.rs`](../../src/controller/scripts.rs) (the
document-start scripts), [`src/core/diagram.rs`](../../src/core/diagram.rs),
[`src/core/assets/style.css`](../../src/core/assets/style.css).
Research: [02 — zathura](../research/02-zathura.md).

## D5a: Two-axis zoom

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

**Both axes are session-scoped, not per-document.** Zoom is a live *view*
setting: once set it carries unchanged across every document switch — following
a wikilink, `:open`, `Ctrl-o`/`Ctrl-i` — including into files that have never
been opened. The per-file `zoom`/`text_zoom` in `history.toml` is the
*default on open*, read only at a window's **cold start** (the initial document,
`build_ui`), where there is no session zoom to inherit; `load_document` never
touches zoom. This is zathura's own split between "default on open" and "current
live setting" (`adjust-open` vs live zoom — [`docs/research/02-zathura.md`](../research/02-zathura.md)).
Because the cold start is the sole reader, the distinction needs no extra state:
the `Shell.zoom` / `Shell.text_zoom` fields *are* the session value, seeded once
and thereafter owned by the session. Recording stays per-file and unchanged, so
reopening a note in a fresh window still lands at the zoom you last read it at.
Text zoom rides into the switch through the inlined `--font-size` ([D12](#d12-a-document-opens-where-it-is-meant-to-open-post-10-implemented)), so the
new document's first painted frame is already at the inherited size.

The statusbar shows `{geometric}%/{text}%T` on the right whenever either axis is
off 100%, and nothing when both are 100%. `GetState` exposes both as `zoom` and
`text_zoom`. The wheel controller lives on the **toplevel window** in capture
phase — the same architectural guarantee the key controller relies on ([D4](../keys/README.md#d4-keybindings--gtk-capture-phase-zathura-semantics)); a
controller attached to the WebView never receives the scroll events.

The scroll **percent** on the right updates live on any scroll. Keyboard scrolls
are Rust-driven and refresh the statusbar directly, but WebKit handles wheel /
touchpad / scrollbar scrolls itself and Rust never hears about them — so an
injected `scroll` listener (coalesced to one report per animation frame, and only
when the rounded percent changes) pings Rust to re-query and repaint the percent.

`Esc`/abort is the universal reset: besides leaving TOC/hint/input modes, it
clears any active search (highlight + `n`/`N` state) and any transient statusbar
notice, returning the chrome to its resting state.

The statusbar's **left** field is the jumplist breadcrumb — the route to the
document you are reading, not just its name: `index.md > topic.md > note.md`.
It is derived, never stored: `Jumplist::trail` walks the entries *behind* the
cursor and appends the live document, collapsing consecutive jumps within one
file, so following a link extends the trail and `Ctrl-o`/`Ctrl-i` shorten and
re-extend it. Overflow is cut on the **left** — whole segments are dropped
oldest-first behind a leading `…` (`core::jumplist::breadcrumb`, fitted to the
label's monospace column count), so the current filename is always visible; the
label also ellipsizes `Start` as a fallback between re-fits. `GetState` reports
the untruncated trail as `trail`, which is how the e2e suite asserts it.

Tab-completion on the `:` line **paginates** rather than truncating: candidates
are packed into pages that fit the bar (`core::command::completion_line`), the
echo shows the page holding the selection with `▸` on it, and repeated `Tab`
walks the pages in order — so every candidate is reachable, not just the first
few. The header is `[candidate/total] (page/pages)`; the page counter is
omitted when everything fits on one page.

## D5a.0: Anything that changes a block's height must be anchored (2026-09-13)

An invariant, learned by shipping v1.9.0 without it: **"no re-render" is not
"nothing moves".**

`s` and `a` were built as class flips — the rule they gate is already in the
stylesheet, so no pipeline pass runs and the text column does not reflow. That
reasoning is true and was still the wrong conclusion. Flipping the class changes
the *height* of the blocks it affects: a table re-wraps at a different measure,
a fitted diagram sheds height by whatever factor its box demands. Everything
after that block shifts, `scrollY` does not, and the reader's place slides out
from under them. Both shipped doing exactly that.

So: **any change that can alter a block's height is wrapped in the [D5a](#d5a-two-axis-zoom) anchor**
(`capture_anchor_js` → apply → `RESTORE_ANCHOR_JS`), keyboard changes at the
viewport top and pointer-driven ones at the cursor. That now covers both zoom
axes, `ToggleWide`, `ToggleDiagramFit` and per-diagram `Ctrl`+wheel. Only
genuinely height-neutral changes are exempt, and there is exactly one: recolor.

The observable is `GetState`'s `probe_text` / `probe_top` — *what* is at the top
of the reading column and *where*. `scroll_y` cannot express this (an anchored
toggle is supposed to change it) and `scroll_percent` is a lossy proxy that let
the bug through. A new toggle is not done until an e2e asserts the same content
sits at the same offset across it, verified red with the anchor removed.

The one case the anchor cannot serve is a document that shrinks below the
current offset: the browser clamps, and there is no position left to hold. That
is not a regression, and the tests stay clear of it by sitting mid-document.

**Resizes too (2026-09-18).** A window resize (fullscreen, an i3 re-tile, a
panel opening) re-lays the page out with no "before" the controller ever sees,
so capture-on-demand cannot serve it. A document-start script
(`resize_anchor_js`) keeps the anchor continuously instead: every scroll
re-takes it, and `resize` scrolls it back. It anchors the *character* under the
probe, not the element, because a resize re-wraps prose and a long paragraph's
top is not where the reader's line is. Scrolls it caused itself do not re-take
it (so fullscreen and back at the end of a document returns to the same place),
and every controller-driven anchored change and the no-flash reveal re-base it,
so a zoom step — which changes the CSS viewport and fires `resize` too — does
not have two anchors fighting. Guarded by
`resizing_the_window_holds_the_reading_position`, verified red without it.

## D5a.1: Wide blocks — pictures get the window, prose keeps the measure (2026-09-13)

Bounded measure is right for prose and wrong for pictures. A 1865 px diagram
read through a 912 px column is mostly scrollbar while the rest of a wide
monitor sits empty. Selected block kinds therefore break out of the reading
column to the window width less a gutter:

```css
width:       calc(100vw - 2 * var(--wide-gutter));
margin-left: calc(50% - 50vw + var(--wide-gutter));
```

- **Margins, not CSS grid.** Grid items do not collapse margins, so making
  `main` a grid would change the vertical rhythm of every block in every
  document to buy a horizontal effect.
- **No media query, because the rule cancels itself out.** `50%` resolves
  against `main`'s content box, `50vw` against the viewport. On a window
  narrower than the column `main` is `width: 100%`, so its content box is
  `100vw - 2 * --main-pad-x`; with `--wide-gutter == --main-pad-x` the margin
  computes to exactly 0 and the width back to exactly that content box —
  today's geometry to the pixel, so there is no narrow-window regression to
  guard against.
- **The no-page-h-scroll invariant holds either way.** If `100vw` includes a
  classic scrollbar, the element is offset half a scrollbar left and half a
  scrollbar narrow of ideal, which the 2 × 1.5 rem gutter absorbs. The blocks
  keep their own `overflow-x: auto`, so anything wider still scrolls inside the
  (now much larger) box, never the page.
- **Only top-level blocks break out** (`main.markdown-body > …`). The argument
  above rests on `50%` resolving against a containing block centred on the
  viewport — true of `main` and of a `<p>` in it, false for a list item, whose
  `ul` indent of 1.6em would push a broken-out table past the gutter and give
  the page a horizontal scrollbar. A block inside a list, callout or blockquote
  keeps in-column geometry, which is also the right reading of it: those are
  bounded contexts.

**Two class levels on `<html>`, both emitted by the pipeline:** one
`jmnj-wide-<kind>` per kind the `wide-blocks` option makes *eligible*, and the
master `jmnj-wide` saying the breakout is *on*. Only the master is touched at
runtime (`s` → `Action::ToggleWide`), so the toggle costs a class flip, no
re-render and no vertical movement — the same contract `html.dark` has with
recolor. A changed *set* re-renders, which is the honest way to change what the
document declares. `WIDE_CLASS` and each kind's class name live in
`core::config` (the pipeline emits them, `controller::scripts` flips them; core
cannot depend on the controller), and a pipeline test asserts the stylesheet
carries a rule for every class the Rust emits, so the two cannot drift.

Defaults: `wide-blocks = diagrams,fences,tables`, `wide = true`. `code` and
`math` are configurable but ship off — code is text, where a bounded measure
helps, and centred display math reads wrong full-bleed.

Consequence for the shell: the recolor class must **join** the pipeline's class
list rather than arrive as a second `class` attribute, which the parser would
drop — taking the breakout with it. `load_document` anchors its rewrite on
`pipeline::HTML_OPEN` / `HTML_CLASS_OPEN` so the tag's shape has one spelling.
`GetState` gained `diagram_box_width` (the `.mermaid` *box*, not the SVG in it)
because the existing `diagram_width` is the SVG at intrinsic width and does not
move when the box widens — there was otherwise no observable for the feature.

## D5a.2: Diagram fit and per-diagram zoom (2026-09-13)

The breakout ([D5a.1](#d5a1-wide-blocks--pictures-get-the-window-prose-keeps-the-measure-2026-09-13)) gives a diagram the window; these two give the reader
control of the picture inside it.

**`a`** (zathura's "adjust to best fit", previously unbound) toggles a
document-wide `jmnj-diagram-fit` class capping `.mermaid svg` at
`min(100%, var(--dw))` — `min()`, so a diagram *smaller* than its box is never
blown up. Same two-part contract as `jmnj-wide`: the pipeline emits it from
`diagram-fit` (default **false** — [D5a](#d5a-two-axis-zoom) decided intrinsic, and the breakout
already recovers most of the width), the runtime toggle is a class flip.

**`Ctrl`+wheel over a diagram** scales that diagram instead of the page,
multiplicatively in `zoom-step` steps, clamped 0.25×…8×, through a per-element
`--dz` the stylesheet multiplies into `--dw`. Transient DOM state, not session
state — a look-closer gesture, unlike [D5a](#d5a-two-axis-zoom)'s session-scoped page zoom — so the
DOM is its only home and a reload drops it. `=` clears it along with both zoom
axes, inside the same anchored capture, because a diagram left at 4× after a
reset would make `=` a lie.

**The routing had to be shell-side and synchronous.** GTK dispatches the scroll
capture-phase from the toplevel before WebKit sees it ([D4](../keys/README.md#d4-keybindings--gtk-capture-phase-zathura-semantics)), so a page `wheel`
listener never fires, and a per-tick round trip is unaffordable. The page
instead posts a pointer-over-diagram index through the existing
`window.__jmnj_post` bridge (`message::DIAGRAM_HOVER`, a `mouseover`/`mouseout`
pair in [`scripts.rs`](../../src/controller/scripts.rs)) and the controller caches it; each tick reads the cache.
No `Viewport` method — this is not a native capability. A stale cache costs one
tick going to the wrong target, never correctness.

**Two consequences.** Scaling up must not grow the document or the page: at
`--dz != 1` the box takes `max-height: 80vh` and both scrollers (plus
`text-align: left`, since a centred child wider than its scroller has an
unreachable left edge); at 1 the geometry is untouched. And merman writes the
intrinsic width onto the SVG root as an **inline** `max-width:<N>px` — the very
declaration `--dw` is parsed from — which clamped every scale above 1 back to
intrinsic. The fix is at the source: `core::diagram` now **moves** that
declaration onto `--dw` rather than copying it, deleting the inline one (root
tag only; a `<foreignObject>` label's own styles are left alone). The
alternative, `max-width: none !important`, would also have outranked the user's
themes — and those are emitted last precisely so they win.

Fit wins over `--dz` throughout, box included: fit is a statement that
everything must be visible. The scale survives and returns when fit goes off.

## D12: A document opens where it is meant to open (post-1.0; implemented)

**A reading position is part of a load, not something done to a document
afterwards.** Every position — a remembered offset, a link fragment, a
`--forward` line — used to be applied from Rust in the `LoadEvent::Finished`
handler. By then WebKit has parsed, laid out and composited the document at
scroll 0, and the correction costs a further UI→web IPC hop, so the unscrolled
top is on screen for that whole window. That is the flash the reader sees
walking the jumplist, and it is worse the more real the document is: `Finished`
waits for subresources while WebKit paints incrementally during parsing, so
images and math fonts widen the gap.

- **One position per load, resolved once.** `shell::view::InitialPosition` is
  `Top | Offset(f64) | Anchor(String) | SourceLine(u32)`, armed by whoever
  initiates the load. It replaces two `Option` fields whose precedence was
  decided by the *order of two statements* at load-finished time, and which
  could between them show three positions in one load (top → history offset →
  fragment). `Top` is the position, not the absence of one, which is why it
  carries no data.
- **The position travels as inert markup**, `data-jmnj-open` on `<html>`,
  written by the same one-shot rewrite that already pre-applies `class="dark"`
  and now also the text-zoom `--font-size`. Inline `<script>` is not available
  and should not be: the page CSP is `default-src 'none'` with no `script-src`,
  and [D3](../architecture/README.md#d3-content-pipeline--100-rust-no-javascript) keeps the webview a dumb static renderer whose HTML can later feed an
  export path. A `data-` attribute survives that export; a script would break
  both.
- **A permanent document-start user-script applies it**, alongside the four that
  already exist — shell viewport glue, the sanctioned category, not
  content-pipeline JS. It applies on `requestAnimationFrame`, whose callbacks
  run *before* the frame they belong to is painted, re-arming on
  `DOMContentLoaded` and `load` for a document still growing, and stopping once
  the position is reached — or, for a position that cannot be reached, once the
  document has **stopped growing**: `readyState === 'complete'` *plus*
  `STABLE_FRAMES` consecutive unchanged `scrollHeight` readings.
  `complete` alone is not a statement that layout is final, and `apply` places
  the position with `scrollTo`, which clamps against the height it finds — so
  conceding on `complete` revealed the body at a near-top offset for any deep
  position, which *was* the document-switch flash (2026-08-07). The height
  check is what distinguishes "still laying out" from "genuinely too short".
- **`html.jmnj-restoring body { visibility: hidden }` closes the gap the timing
  cannot**, a shell-toggled class on the same contract as `html.dark`. The
  reveal is unconditional and timer-backed: a page left permanently blank would
  be a worse bug than the flash, and graceful degradation is binding ([D8](../math/README.md#d8-math--pulldown-latex--mathml-core-no-javascript-m3)/D9).
- **`LoadEvent::Finished` keeps one job**: re-running the script's *own* apply
  once subresources are in, since an offset can clamp against a document that
  has not finished growing. Idempotent, and a no-op for a document opening at
  the top.

Verified the way the 2026-07-06 flicker was: Xvfb + `ffmpeg` frame capture across
a cross-document `Ctrl-o`, with a CSS-painted marker at the top of the departed
document. Pre-fix, two captured frames are a full screen of that marker; after,
zero — what replaces them is two frames of the page's own `--bg`. The
load-finished restore passed a "lands in the right place" assertion the whole
time, which is why the e2e observable is `first_frame_scroll_y`: the offset the
*first* painted frame was placed at, recorded from inside the page.

That observable is necessary but not sufficient, and the 2026-08-12 gap says
why: with the gate in place the early frames are *hidden*, so a document still
growing legitimately paints them at a clamped, near-top offset — and no e2e
fixture grew after `complete` in the first place, so the suite could not
reproduce the flash even in principle. Both halves are now covered: the page
also records **`reveal_scroll_y`** and **`reveal_failsafe`** — the offset the
body was unhidden at, and whether the 400 ms timer is what unhid it, i.e. the
first frame the reader can actually see and how it was earned — and the suite
carries a fixture that keeps growing for ~200 ms after `complete` (a CSS
`height` animation; inline `<style>` is CSP-allowed, JS is not). Weakening
`STABLE_FRAMES` to the pre-fix behaviour turns both restore tests red at
~34% of the target offset, so the gate is now defended by a test that has been
shown to fail without it.
