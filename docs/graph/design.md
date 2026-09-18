# The document graph — design

This page records how the document graph is built: the walk, the layout, the
overlay and what was rejected. The feature's front page is the
[README](README.md); how it behaves is specified in
[interaction.md](interaction.md), which wins where the two disagree.

Decisions on this page:

- [D14: The document graph — a route spine with its links fanned out (2026-09-18)](#d14-the-document-graph--a-route-spine-with-its-links-fanned-out-2026-09-18)

Code: [`src/core/graph/`](../../src/core/graph/mod.rs),
[`src/controller/assets/graph.js`](../../src/controller/assets/graph.js),
[`src/controller/session.rs`](../../src/controller/session.rs).

## D14: The document graph — a route spine with its links fanned out (2026-09-18)

Documentation trees (a `docs/` folder, an eval-case collection, a vault) are
graphs of documents that link to each other. The breadcrumb ([D10](../jumplist/README.md#d10-cross-document-jumplist-navigation-post-10)) shows the one
route you took; nothing showed where else you could go. `t` opens a **document
graph** over the page: the route you took drawn as a straight line, the nodes
around it, and where the current node leads.

**The feature is documented in [`docs/graph/`](README.md); its interaction model lives in [`docs/graph/interaction.md`](interaction.md)** — principles, objects,
states, every gesture and every visual cue, and the review against them. This
entry records how the graph is built; where the two disagree on behaviour,
`interaction.md` wins.

**Maxim: consistent, simple, intuitive** (the owner's words). Every choice
answers to it: one gesture means one thing everywhere (`Space` or a click on
the handle folds and unfolds, `h`/`l` move, in both views, on every node);
nothing moves unless the reader moved it; nothing is shown twice. The
principles and the gesture table are in [`interaction.md`](interaction.md).

**The walk (data).** Unchanged from the first cut, and still the only I/O:

- **Root = the first document of the jumplist trail** (`Jumplist::trail`,
  [D10](../jumplist/README.md#d10-cross-document-jumplist-navigation-post-10)); a trail that revisits a node is cut back to the first visit (`A > B >
  A` is `A`), so the route is a path. Recomputed on every open.
- **A spanning tree of the link graph.** Breadth-first from the root, each node
  placed once under the first node that reached it, children in source link
  order. Deterministic: the same files give the same picture, no simulation.
- **The route pins only what a link explains.** A trail step `A → B` where `A`
  links to `B` moves `B`'s subtree under `A` — a pass over the finished walk,
  skipped when `A` lies inside that subtree (the reader went back up a chain).
  A step no link explains (`:open`, a jump from the graph) pins nothing; a
  route node the walk cannot reach at all hangs under its trail predecessor on
  a dashed edge.
- **Bounded**: `NODE_BUDGET` nodes, `READ_CAP` bytes per document, on the `Host`
  worker (like the vault scan, [D11](../obsidian/README.md#d11-obsidian-dialect--vault-resolution-post-10-implemented)). **Same link semantics as the reader**:
  path links against the document's directory, wikilinks via the vault index,
  only existing `.md`/`.markdown` targets. **Titles**: frontmatter `title`, else
  the first `# H1`, else the file stem.
- Each node keeps `targets`: every placed node it links to, tree child or not.

**The scene (layout) — rewritten after the owner's first look.** The first cut
was a plain left-to-right tidy tree. On a real tree (scribetech-assistant
`docs/`, a README linking 52 documents) three things were wrong: the route zig-zagged
through the tree instead of reading as a line; a node's links were "all over
the place" (dashed curves to wherever its targets happened to sit); and zoomed
out, nothing was readable. So the graph is now laid out *around the reader*,
in the manner of TheBrain and ExcaliBrain (current node as the layout anchor)
with SpaceTree-style collapsed branches:

- **The spine.** The route root → … → current sits on one horizontal row,
  one column per step, joined by a straight accent line. Everything else hangs
  above or below it. A spine node's other children split by link order: those
  its source lists *before* the route child go above, those after go below.
- **One kind of object.** Every item is a node with at most one fold handle
  (`+n` folded, `−` unfolded); the full gesture set and visual vocabulary are
  in [interaction.md](interaction.md). The first cut had a second object, the
  `+23` cluster holding a route step's siblings; it behaved like a folded node
  but looked different, and the owner read it as siblings hidden for no reason.
  Clusters were removed: a route node's other links are its children, drawn as
  **siblings** in the route child's column, unfolded by default, and folding the
  route node hides them (never the next route step).
- **Two views, `v` toggles** (option `graph-view`, default `links`). The view
  decides one thing — which links count as children — and the default folds:
  - **`links`** — every outgoing link, **duplicates allowed** (a link back to
    the root is a node in the fan too, marked as on the route). Route nodes and
    the current node start unfolded, every other node folded.
  - **`tree`** — the spanning tree, every node once, everything unfolded.
    Selecting a node outlines its link targets and dims the rest off the route
    (Obsidian's hover highlight): tree view cannot show links as children, so
    it highlights them. No lines are drawn across the tree — dashed cross-link
    curves were tried twice and read as clutter both times.
  - Folds are the reader's explicit choices over the view's defaults, keyed by
    item, and mean the same in both views. The status line names the view
    (`Graph: links`, `Graph: tree`).
- **Folding is its own gesture** (`Space`, or a click on the handle); `h` only
  moves. zathura's TOC folds with `h`, but here the route starts unfolded, so a
  folding `h` would close every step the reader walks back past.
- **Hover peeks, it does not re-lay out.** Hovering a folded node draws its
  children as a temporary fan in a floating layer over the scene, so nothing
  else moves; a re-laying-out preview would move every item under the pointer
  as it travels. Automatic unfolding on zoom was left out for the same reason:
  a layout that changes shape as you zoom is the jumping-around this design
  exists to avoid.
- **Packing.** Hand-written, not a crate: `dugong` (the dagre port merman
  already pulls in) is free but lays out DAGs by crossing minimisation, which
  cannot pin a spine and re-orders on every change; `tidy-tree` is one stale
  2023 release that would need patching for the spine anyway. Off-spine
  subtrees are classic tidy trees (leaves in consecutive rows, a parent midway
  between its first and last child). They are packed against a per-column
  **skyline** above and below the spine, deepest spine node first, so the
  branches nearest the current node sit nearest the line; each block goes as
  close to the spine as the columns it spans allow. No two items share a row
  in a column, and the spine never bends. Tree edges are elbows sharing one
  trunk per parent (a fan of fifty is one line with fifty branches).
- **Level of detail (semantic zoom).** The scene is one SVG; the overlay sets
  a zoom-level class and a `--k` scale variable, and CSS does the rest — the
  representation changes with scale, not just its size:
  - *near* (scale ≥ 0.7): pills with title and file name;
  - *mid* (0.4–0.7): title only;
  - *far* (< 0.4): leaves disappear into **bundles** — core emits, for every
    parent with two or more leaf children, a bar spanning their rows labelled
    `12 nodes` (click one to zoom in on it) — and the labels that remain
    (spine, inner nodes, bundles) are counter-scaled by `1/k` so they stay readable on screen.
  Levels fade into each other (opacity), after Obsidian's text-fade threshold.
  Zoomed out, **columns shrink less than rows** (horizontal scale
  `max(k, KX_MIN)`, text counter-scaled so glyphs never distort), so a far
  label gets more characters before its ellipsis. Far labels sit at 15 px on
  screen.
  Counter-scaled labels are wider than a column once zoomed out, so the overlay
  also **culls labels by collision**: after each zoom it walks the visible
  labels in priority order (current, spine, selection, bundles,
  nodes that lead further, plain leaves) and hides any whose on-screen box
  would overlap one already kept; far labels are also cut to one column's
  width. Only true leaves — nodes with no links of their own — are bundled; a
  node with a `+n` handle stays visible, because it is where the reader can go
  next.
- **Selection is an item, not a node**: with duplicates a node can be on screen
  twice. Items carry a stable key (the path of node indices from the root in
  the displayed tree); the page posts keys, never indices, so a click racing a
  re-layout cannot land on the wrong item, and a node that disappears under a
  fold leaves the selection on the folded node. A re-layout — fold, unfold,
  `v`, `:set graph-view` — keeps the selection and keeps the item acted on
  still on screen (the camera compensates).

**The overlay (interaction).**

- **In-page, not a widget**: core writes the SVG, the controller draws it
  through `Viewport::eval` like the hint overlay; no new trait method ([D2a](../architecture/README.md#d2a-three-layers--core-controller-toolkit-shell-2026-09-02)).
- **Mouse**: the wheel pans (`Shift` for sideways), drag pans, `Ctrl`+wheel
  zooms toward the cursor — the reader's own convention ([D5a](../reading/README.md#d5a-two-axis-zoom)), and the owner's
  pick after trying wheel-zooms-like-Obsidian. GTK takes `Ctrl`+wheel first
  ([D4](../keys/README.md#d4-keybindings--gtk-capture-phase-zathura-semantics)), so the controller routes it. Hover highlights the
  item and its edges. Click selects, a click on the handle folds or unfolds,
  double-click opens.
- **Keys** (mode `graph`, `[keys.graph]`): listed in
  [interaction.md](interaction.md#gestures). `:` opens the command line so
  `:set graph-view` reaches an open graph; every other unbound key is consumed
  while the overlay is up. The view is session state, like `s`: the next `t`
  reopens in the view last chosen. Opening a TOC (`Tab`, `:toggle toc`) closes
  the graph first — graph mode holds exactly while the graph is open. Zoom
  anchors where attention is: `Ctrl`+wheel at the pointer (tracked by the page
  itself, in page pixels — WebKit renders at its own scale, so the shell's
  pointer is off by that factor), `+`/`-` at the selection.
- **Opening frame**: the route comes first. Root → current → the fan's first
  column, centred, when all of it fits the window at 1:1; else the whole route
  with the fan running off the right edge; only a route wider than the window
  puts the current node a third of the way across. Losing the root off the
  left edge hid where the reader started
  ([R16](interaction.md#review-of-the-current-system)).
- **Panel (bottom left)**: the selected node's title and path, and the route as
  a clickable breadcrumb (click a segment to select that spine node). No
  restated facts — "you are here" is what the tint already says, and a link
  count is what the fan or handle already shows. Links the walk's budget cut
  are said in words for the selected node ("12 more links not walked"), so
  `+n` is only ever a handle. A one-node route shows no
  breadcrumb (it would repeat the title). **Help line (bottom right)**:
  the keys. Background plain.
- **Lifecycle**: a walk's landing is drawn only if it is still the newest one
  asked for (a generation number), the document has finished loading, and the
  reader is still in Normal mode. Opening a node goes through `open_file`, so
  it lands on the jumplist and `Backspace` returns.

**A core part, not an add-on.** The same surfaces as every other feature:
`:graph` / `:toggle graph` and every graph action through `:` exec and D-Bus
`ExecuteAction`; the `graph-view` option in the config file, in `:set` (with
completion) and applied live to an open graph; `GetState` reports
`graph_view` (`""` when closed), `graph_selected` (the selected node's path,
or `""`) and `graph_items`; `[keys.graph]` remaps.

**Look.** The document's theme variables, so it follows `Ctrl-r`. Quiet pills;
the one strong element is the spine in the accent colour.

**Key.** `g` prefixes `gg`, so the default is `t` (it draws a tree).

- Rejected — **D3.js** or any bundled JS library: the layout is the part worth
  testing, and in core it is unit-tested; the overlay JS only pans, zooms and
  swaps classes. Rejected — **a force layout** (Obsidian's): positions that
  depend on a simulation are exactly the "nodes jump around" the owner ruled
  out. Rejected — **a second WebView or a native widget**: a cold web process
  costs ~700 ms ([Risks](../architecture/README.md#risks--mitigations)) and a widget is per-shell work.
