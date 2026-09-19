# The document graph — interaction model

The document graph (`t`, [D14](design.md#d14-the-document-graph--a-route-spine-with-its-links-fanned-out-2026-09-18)) shows the documents reachable by links
from where the reading session started, one **node** per document: the route
you took as a straight line, the nodes around it, and where the current node
leads. [D14](design.md#d14-the-document-graph--a-route-spine-with-its-links-fanned-out-2026-09-18) records *how it is built* (the walk, the packing, the
overlay). This document records *how it
behaves*: the objects a reader sees, the states they can be in, what every
gesture does, and what every visual cue means. It is the spec for the
interaction; the code and the [feature README](README.md) follow it; the
[mockup](mockup.html) draws it.

## Maxim

**A consistent, simple and intuitive interface.** (The owner's words.)

## Principles

Each principle comes with the question that tests a design against it.

| # | Principle | Test question |
|---|---|---|
| P1 | **One gesture, one meaning.** A key or mouse gesture does the same thing in both views, on every kind of item, at every zoom level. | Can I describe this gesture in one sentence with no "except"? |
| P2 | **One object, one anatomy.** Everything you can select is a node, drawn the same way; its state shows on the node itself. | Does a new visual form carry a new *behaviour*, or only a new look? |
| P3 | **Every cue means one thing.** A colour, a line style or a glyph has one meaning in the whole graph. | If I see this cue elsewhere, does it mean the same? |
| P4 | **Nothing moves unless you moved it.** The layout is a function of the files and of your folds; the camera moves only in answer to your input, and a re-layout keeps the item you acted on still on screen. | Did anything move that I did not touch? |
| P5 | **Nothing hidden without a handle; nothing shown twice.** Whatever is hidden shows a count you can open, and whatever you opened you can close the same way. No text restates what the picture shows. | Where is the way back? Is this said twice? |
| P6 | **Your place is always legible.** Where you are (current), how you got here (route), what you have selected, and which view is on are visible at every zoom level. | Could I tell where I am from a screenshot? |
| P7 | **The reader's conventions carry over.** Keys and mouse mean what they mean elsewhere in jumanji (`hjkl` move, `Enter` opens, `Esc` leaves, wheel pans, `Ctrl`+wheel zooms at the cursor). | Would a jumanji user guess this without the help line? |

## The model

**Objects.** The graph is made of **nodes** joined by **links**. A node stands
for one markdown document; a link is a link in its text.

- Every node on screen is drawn the same way: a pill with its title, its file
  name below, and — if it links to anything — a **handle** at its right edge.
- The **route** is the chain of nodes from the **root** (where the session
  started) to the **current** node (the one being read), drawn as one straight
  accent line. Every other node hangs above or below it.
- A node's **children** are the nodes it links to. Which links count as
  children is the one thing the **view** decides — and the handle always
  counts children *in the current view*:
  - **links** — every outgoing link, so a node can appear more than once
    (a link back to the root is a child like any other). An occurrence of a
    node already on its own branch — a link back up the branch, a self-link —
    closes a cycle: it is shown, with no handle, since its children are
    already on screen above it. So a branch never holds a node twice, and `l`
    stops there;
  - **tree** — the spanning tree, so every node appears exactly once.

**States.**

| State | Values | Changed by |
|---|---|---|
| Fold, per node | **folded** (children hidden, handle shows `+n`) · **unfolded** (children shown, handle shows `−`) · no handle (no links) | the handle, `Space`, entering a folded node with `l` |
| Selection | one node | click, `hjkl`, breadcrumb |
| Hover | at most one node | the pointer |
| View | links · tree | `v`, `:set graph-view` |
| Camera | pan, zoom | wheel, drag, `Ctrl`+wheel, `+` `-` `=`, window resize |

**Defaults when the graph opens.** The route nodes are unfolded, so each route
step shows its **siblings** — its parent's other links, in the same column,
above (links listed before it) and below (after it). The current node is
unfolded, so its links fan out to the right. In **links** view every other node
starts folded; in **tree** view every node starts unfolded. That is the only
difference in defaults; folding works identically in both.

The route line itself cannot be folded away: folding a route node hides its
other children, never the next route step.

## Gestures

One row per intent. Each works the same in both views and on every node.

| Intent | Key | Mouse | Effect |
|---|---|---|---|
| Open / close the graph | `t` | — | Opens on the current node; `t` again closes. |
| Leave | `Esc`, `t` | — | Close the graph, back to the page. With the `:` prompt open, `Esc` closes the prompt first and leaves the graph up. `q` quits jumanji, as everywhere. |
| Select | `j` / `k` next / previous in the column, `h` parent, `l` child | click a pill; click a breadcrumb segment | Moves the selection; the camera follows only if the selection left the screen. |
| Enter a folded node | `l` | — | Unfolds it and selects its first child. |
| Fold / unfold | `Space` | click the handle | Toggles the selected (or clicked) node. `+n` becomes `−` and back. |
| Peek | — | hover a folded node | Its children appear as a temporary fan in a floating layer, drawn at full size whatever the zoom, next to the node; nothing else moves. Leaving removes it. |
| Open a node | `Enter` | double-click | Shows it in the reader; the graph closes. The current node: back to reading. |
| Switch view | `v` | — | links ⇄ tree; the status line names the view; the selection stays on its document — if the new view has it folded away, the path to it unfolds. |
| Zoom | `+` / `-` about the selection, `=` 1:1 on the current node | `Ctrl`+wheel about the cursor | The anchor is where your attention is: the selection for keys, the cursor for the mouse. |
| Pan | — | wheel, `Shift`+wheel, drag | Keys never pan; moving the selection reveals what it reaches. |
| Zoom into a bundle | — | click a bundle | Zooms in until its nodes are readable, centred on it. |
| Commands | `:` | — | `:set graph-view`, and every graph action by name (`:graph child`…). |

## Visual vocabulary

One row per cue; one meaning per cue.

| Cue | Meaning |
|---|---|
| Accent **line** | The route. |
| Accent **dot** on a node | The node is on the route (also when it appears again elsewhere). |
| Accent **tint** fill | The current node — at every zoom level, including far (P6). |
| Larger dot | The root. |
| Light outline | The selection. |
| Brighter fill | Hover. Nothing else: hover never borrows the accent. |
| Handle `+n` / `−`, muted, inside the pill's right edge (beside the label when zoomed out) | Folded, with `n` children in this view / unfolded. No handle: nothing to unfold here (a leaf, or an occurrence that closes a cycle). Visible and clickable at near and mid zoom; far out it goes with its label where labels collide, until hover or selection shows it (see [Deliberate exceptions](#deliberate-exceptions)). Muted, because accent belongs to the route. |
| **Dashed** line | A step no link explains (a jump). Nothing else is dashed. |
| Bar reading "N nodes" (zoomed out) | N nodes with nothing to unfold in this view, bundled; click to zoom in. |
| Accent **outline** + dimmed rest (tree view, near and mid zoom) | The selected node's link targets. Tree view shows every node once, so it cannot show links as children; it highlights them instead. Not drawn far out (see [Deliberate exceptions](#deliberate-exceptions)). |
| Panel, bottom left | The selection's full title and path; the route as a breadcrumb (hidden when the route is one node). |
| Status line | `Graph: links` / `Graph: tree` — also again once the `:` prompt closes. |
| Help line, bottom right | The keys. |

Zoom changes *what* is shown, never what it means: near shows title and file
name; mid, the title; far bundles leaves and keeps the remaining labels at a
readable size, cut to their column and thinned where they would collide. No
label is ever smaller than it is legible (≈14 px titles, 12 px file names,
15 px far labels, on screen).

## Review of the current system

Checked against the principles: the committed state (`b88528a`) and the
owner's feedback on 2026-09-18, a code review on 2026-09-19 (R17–R20), and
the pre-release review on 2026-09-19 (R21–R24). Rows without "Done" are open.

| # | Finding | Violates | Resolution |
|---|---|---|---|
| R1 | `h`/`l` fold in links view but only move in tree view. | P1 | Done: Folding gets its own gesture (`Space`, handle); `h` always moves; `l` enters (and so unfolds) a folded node. Folding works in both views. |
| R2 | Two objects for one behaviour: dashed **clusters** (`+23`) and nodes with a `+n` badge expand and fold alike but look different. | P2, P3 | Done: Remove clusters. A route node's other links are its children, shown as siblings (unfolded by default, per the owner); folding the route node hides them behind its own handle. |
| R3 | Dashed means both "cluster" and "jump". | P3 | Done: Follows from R2: dashed is left meaning only "jump". |
| R4 | No mouse way to fold again ("unexpand"). | P5 | Done: The handle toggles: `+n` unfolds, `−` folds. |
| R5 | The hover preview works only on the tiny badge of a node, but anywhere on a cluster; and it shows a list, not the fan. | P1, P2 | Done: Hover anywhere on a folded node peeks at its fan in a floating layer (after a short delay, so a passing pointer does not flash fans). |
| R6 | In tree view a muted `+n` counts links cut by the node budget — the same glyph as the fold handle, but not openable. | P3, P5 | Done: Budget-cut links are said in the panel ("12 more links not walked") for the selected node; `+n` is only ever a handle. |
| R7 | `+`/`-` zoom about the viewport centre, `Ctrl`+wheel about the cursor. | P1 | Done: One rule: zoom about where your attention is — the cursor for the mouse, the selection for the keys. |
| R8 | `Ctrl`+wheel shifts the view instead of zooming at the cursor (owner). | P4, P7 | Done: the overlay tracks the pointer in page px (WebKit renders at its own 2× here, so the shell's pointer was off by that factor). |
| R9 | Labels too small at mid zoom; titles sit low in the pill (owner). | P6 | Done: minimum on-screen sizes at every level; titles centred when the file name hides; labels cut to the drawn size. |
| R10 | Resizing the window moves the graph (owner). | P4 | Done: resize keeps the world point at the viewport centre fixed. |
| R11 | Zoomed out, bundles are dead ends: their nodes cannot be reached without zooming by hand. | P5 | Done: A bundle is a handle too: clicking it zooms in on it. |
| R12 | The view was invisible after `v` (owner). | P6 | Done: the status line names it. |
| R13 | "You are here", link counts and a one-node breadcrumb restated the picture (owner). | P5 | Done: removed. |
| R14 | Dashed cross-link curves across the tree (owner: "ugly"). | P3, P4 | Done: replaced by outline-and-dim. |
| R15 | Wheel zoomed in an earlier cut (owner preferred the reader's convention). | P7 | Done: wheel pans, `Ctrl`+wheel zooms. |
| R16 | When root → current → fan does not fit the window, the opening frame puts the current node a third across and cuts the root off (seen at 1200 CSS px on a 3-step route). | P6 | Done: Frame the route first: if root → current fits, keep it all in view and let the fan run off the right edge; only a route wider than the window falls back to the current node a third across. |
| R17 | Zoomed out, the peek fan scales with the scene and becomes unreadable (overlapping titles, file names out of their pills). | P1, P6 | Done: The peek is drawn at screen scale — full pills, 1:1 — next to the hovered node, at every zoom level. |
| R18 | Zoomed far out, handles disappear: folds are invisible and cannot be clicked. | P1, P5 | Done: Handles stay visible and clickable at every level (a small muted `+n` / `−` beside the far label) — except where far labels collide, which the owner accepted (R24). |
| R19 | Hover strokes edges and the pill in accent, which reads as the route or as a tree-view link target. | P3 | Done: Hover is a brighter fill only. |
| R20 | In tree view a node whose links are all cross-links has no tree children, so it has no handle and is bundled, though it has links. | P3 | Done: Wording, not code: the handle counts children in the current view, and a bundle holds nodes with nothing to unfold here — one rule for both views. |
| R21 | In links view a cycle (`a → b → a`, a self-link) unfolds without end: `l` with a large count froze the reader. | P5 | Done: An occurrence already on its own branch is a leaf without a handle; a branch holds no node twice. |
| R22 | `v` lost the selection when the new view had its node folded away, and fell back to the node that hid it. | P6 | Done: `v` unfolds the path to the selected document's place in the new view. |
| R23 | After a `:` command in the graph the status line kept the completion echo, not the view. | P6 | Done: closing the prompt restores the mode's resting status. |
| R24 | Far out, label culling can hide a handle, and tree view's link outlines are not drawn. | P1 | Kept, by the owner's decision: far zoom is an overview (see Deliberate exceptions). |

## Deliberate exceptions

- **`q` quits jumanji** from the graph, as it does from the page and the TOC.
  Consistency across the reader outweighs consistency within the overlay; `Esc`
  and `t` are the ways to leave the graph.
- **`h` in the graph never folds**, although `h` in the TOC collapses a
  heading (zathura's index). In the graph the route is unfolded by default, so
  walking back along it with a folding `h` would close every step you pass.
  Folding is one explicit gesture instead. (Open question 3.)
- **Keys do not pan.** The selection is the keyboard's camera: moving it pans
  when needed.
- **Tree view highlights links; links view shows them as children.** One
  question ("where does this node lead?"), answered in the form each view can
  offer (R14).
- **Far zoom is an overview, with deliberately limited interaction.** It
  bends P1 ("at every zoom level"): where far labels collide, a culled label
  takes its handle with it, so that fold cannot be seen or clicked until the
  node is hovered or selected; and far out the pills go, so tree view's
  outlines of the selection's link targets are not drawn (the rest still
  dims). The owner chose
  this on 2026-09-19: far out the picture is for orientation, and zooming in
  (a click on a bundle, `Ctrl`+wheel, `+`) brings every gesture back. The
  selection, the route and the current node stay legible (P6).

## Open questions

1. **Search in the graph** (`/`, `n`/`N` over titles): the reader's search
   convention would carry over (P7). Not needed yet; worth it once trees grow.
2. **Clicks inside a peek fan** do nothing today. Selecting (or unfolding and
   selecting) the clicked child would be the natural reading; unspecified until
   someone reaches for it.
3. **Should the TOC adopt `Space` for fold** and keep `h` for move, so both
   overlays share one rule? That changes zathura muscle memory in the TOC.
