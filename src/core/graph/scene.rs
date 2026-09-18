//! The scene (DESIGN D14, docs/graph/interaction.md): which items are on
//! screen and where.
//!
//! A [`Scene`] is derived, never edited: it is a function of the [`Graph`],
//! the [`View`] and the reader's [`Folds`]. Folding, unfolding or switching
//! the view builds a new one; an item's [`ItemKey`] is what carries the
//! selection across.
//!
//! One object: every item is a node, with one fold state. Its **children** are
//! the nodes it links to — every outgoing link in the links view, its spanning
//! tree children in the tree view — shown when it is unfolded. The route nodes
//! and the current node start unfolded; in the links view every other node
//! starts folded, in the tree view unfolded. A route node's children are the
//! next route step (always shown: the route cannot be folded away) and its
//! other links, which sit in the next step's column as its **siblings**.
//!
//! The layout is a grid: an item sits in a column (link steps from the root
//! along the displayed tree) and on a row (in item heights, `0` is the spine,
//! negative is above). Three passes:
//!
//! 1. **The displayed tree.** The route root → current takes items `0..spine`,
//!    one column per step; everything unfolded hangs off it.
//! 2. **Packing.** The current node's children are one block centred on the
//!    spine row. Every other route node's siblings form a block above and a
//!    block below, split by link order around the route child. Blocks are tidy
//!    forests (leaves on consecutive rows, a parent midway between its first
//!    and last child), placed deepest route node first against a per-column
//!    skyline, each as close to the spine as the columns it spans allow.
//! 3. **Bundles.** Runs of two or more sibling leaves (nodes with nothing to
//!    unfold in this view), which the far zoom level draws as one bar.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use super::{EdgeKind, Graph};

/// What a node's children are (option `graph-view`, `v`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum View {
    /// Every outgoing link, so a node can appear more than once.
    #[default]
    Links,
    /// The spanning tree, so every node appears exactly once.
    Tree,
}

impl View {
    /// The option spelling, which [`View::parse`] round-trips.
    pub fn name(self) -> &'static str {
        match self {
            View::Links => "links",
            View::Tree => "tree",
        }
    }

    /// Parse the option value, case-insensitively.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "links" => Ok(View::Links),
            "tree" => Ok(View::Tree),
            other => Err(format!(
                "unknown graph view {other:?}: expected \"links\" or \"tree\""
            )),
        }
    }

    /// The other view.
    pub fn toggled(self) -> Self {
        match self {
            View::Links => View::Tree,
            View::Tree => View::Links,
        }
    }

    /// How a node that is not on the route starts out.
    fn default_fold(self) -> Fold {
        match self {
            View::Links => Fold::Folded,
            View::Tree => Fold::Unfolded,
        }
    }
}

/// An item's identity across re-layouts: the node indices from the root to
/// it along the displayed tree. A route node's sibling is `route … / sibling`
/// in both views, so a key means the same item after `v` wherever both views
/// show it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemKey(Vec<usize>);

impl ItemKey {
    fn child(&self, node: usize) -> Self {
        let mut path = self.0.clone();
        path.push(node);
        Self(path)
    }

    /// The node this key ends on.
    fn node(&self) -> Option<usize> {
        self.0.last().copied()
    }
}

impl fmt::Display for ItemKey {
    /// Dot-separated node indices: `0.4.17`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, node) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            write!(f, "{node}")?;
        }
        Ok(())
    }
}

impl FromStr for ItemKey {
    type Err = String;

    /// The [`Display`](fmt::Display) spelling back: what the overlay posts.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.split('.')
            .map(|n| n.parse().map_err(|_| format!("not an item key: {s:?}")))
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }
}

/// A fold state the reader chose for one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Folded,
    Unfolded,
}

/// The reader's folds: only the items whose state they set. Every other item
/// has its view's default. A choice holds in both views, so a node folded in
/// one is folded in the other.
pub type Folds = BTreeMap<ItemKey, Fold>;

/// An item's fold handle, as drawn at its right edge. It counts the item's
/// children *in this view*: in the tree view a node whose links all point at
/// nodes placed elsewhere has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handle {
    /// No children in this view: nothing to unfold here.
    None,
    /// `+n`: the children it hides, by node — what a peek shows.
    Folded(Vec<usize>),
    /// `−`: children shown.
    Unfolded,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub key: ItemKey,
    /// The node it shows. In the links view a node can be on screen more than
    /// once.
    pub node: usize,
    pub col: usize,
    /// In item heights; `0` is the spine, negative is above it.
    pub row: f64,
    /// `None` only for the root, item 0.
    pub parent: Option<usize>,
    /// How the item hangs under its parent. Only a route step can be a jump.
    pub edge: EdgeKind,
    pub handle: Handle,
}

/// Two or more sibling leaves on consecutive rows, drawn as one bar when
/// zoomed far out.
#[derive(Debug, Clone, PartialEq)]
pub struct Bundle {
    pub parent: usize,
    /// Item indices, top to bottom.
    pub members: Vec<usize>,
}

/// A move of the selection, in the scene's own geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The item above in the same column.
    Up,
    /// The item below in the same column.
    Down,
    Parent,
    /// The child on the spine, else the first child.
    Child,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    view: View,
    items: Vec<Item>,
    /// Items `0..spine` are the route, root first.
    spine: usize,
    /// Each item's children, in link order.
    children: Vec<Vec<usize>>,
    bundles: Vec<Bundle>,
}

impl Scene {
    /// Lay out `graph` for `view`, with the reader's `folds`.
    pub fn new(graph: &Graph, view: View, folds: &Folds) -> Self {
        let mut builder = Builder {
            graph,
            view,
            folds,
            on_route: {
                let mut on = vec![false; graph.nodes().len()];
                for &n in graph.route() {
                    on[n] = true;
                }
                on
            },
            items: Vec::new(),
            children: Vec::new(),
        };
        let blocks = builder.build();
        let spine = graph.route().len();
        let Builder {
            mut items,
            children,
            ..
        } = builder;
        pack(&mut items, &children, spine, &blocks);
        let bundles = bundles(&items, &children, spine);
        Self {
            view,
            items,
            spine,
            children,
            bundles,
        }
    }

    pub fn view(&self) -> View {
        self.view
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn item(&self, index: usize) -> &Item {
        &self.items[index]
    }

    /// The spine's items, root first: always `0..len`.
    pub fn spine(&self) -> std::ops::Range<usize> {
        0..self.spine
    }

    pub fn is_spine(&self, index: usize) -> bool {
        index < self.spine
    }

    /// The current node's item: the end of the spine.
    pub fn current(&self) -> usize {
        self.spine - 1
    }

    pub fn children(&self, index: usize) -> &[usize] {
        &self.children[index]
    }

    pub fn bundles(&self) -> &[Bundle] {
        &self.bundles
    }

    /// The item with exactly this key, if it is on screen.
    pub fn exact(&self, key: &ItemKey) -> Option<usize> {
        self.items.iter().position(|it| it.key == *key)
    }

    /// The item for `key`, or — when a re-layout removed it — the nearest
    /// stand-in: the first item showing the same node, else the deepest item
    /// on the key's path (the folded node that hides it), else the current
    /// node.
    pub fn find(&self, key: &ItemKey) -> usize {
        self.exact(key)
            .or_else(|| {
                let node = key.node()?;
                self.items.iter().position(|it| it.node == node)
            })
            .or_else(|| {
                (1..key.0.len())
                    .rev()
                    .find_map(|len| self.exact(&ItemKey(key.0[..len].to_vec())))
            })
            .unwrap_or(self.current())
    }

    /// Where `step` takes the selection from `from`; `from` itself when there
    /// is nowhere to go.
    pub fn step(&self, from: usize, step: Step) -> usize {
        let item = &self.items[from];
        match step {
            Step::Parent => item.parent.unwrap_or(from),
            Step::Child => {
                let children = &self.children[from];
                children
                    .iter()
                    .copied()
                    .find(|&c| self.is_spine(c))
                    .or_else(|| children.first().copied())
                    .unwrap_or(from)
            }
            Step::Up | Step::Down => {
                let column = self
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| it.col == item.col);
                let pick = if step == Step::Down {
                    column
                        .filter(|(_, it)| it.row > item.row)
                        .min_by(|a, b| a.1.row.total_cmp(&b.1.row))
                } else {
                    column
                        .filter(|(_, it)| it.row < item.row)
                        .max_by(|a, b| a.1.row.total_cmp(&b.1.row))
                };
                pick.map_or(from, |(i, _)| i)
            }
        }
    }

    /// The fold that flips item `index` — for [`Folds`] — or `None` when it
    /// has no handle.
    pub fn toggle(&self, index: usize) -> Option<(ItemKey, Fold)> {
        let item = &self.items[index];
        let flipped = match &item.handle {
            Handle::None => return None,
            Handle::Folded(_) => Fold::Unfolded,
            Handle::Unfolded => Fold::Folded,
        };
        Some((item.key.clone(), flipped))
    }
}

// ---------------------------------------------------------------------------
// The displayed tree
// ---------------------------------------------------------------------------

/// Which side of the spine a block hangs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Above,
    Below,
}

/// A route node's shown children other than the next route step, grouped the
/// way they are packed.
enum Block {
    /// Above and below the spine (every route node but the current one).
    Split {
        above: Vec<usize>,
        below: Vec<usize>,
    },
    /// Centred on the spine row: the current node's children.
    Centred(Vec<usize>),
}

struct Builder<'a> {
    graph: &'a Graph,
    view: View,
    folds: &'a Folds,
    /// Per node: whether it is on the route.
    on_route: Vec<bool>,
    items: Vec<Item>,
    children: Vec<Vec<usize>>,
}

impl Builder<'_> {
    fn push(&mut self, key: ItemKey, node: usize, parent: Option<usize>, edge: EdgeKind) -> usize {
        let at = self.items.len();
        let col = parent.map_or(0, |p| self.items[p].col + 1);
        if let Some(p) = parent {
            self.children[p].push(at);
        }
        self.children.push(Vec::new());
        self.items.push(Item {
            key,
            node,
            col,
            row: 0.0,
            parent,
            edge,
            handle: Handle::None,
        });
        at
    }

    /// `node`'s children in this view, the route excepted: a route node's
    /// next step is placed by the spine, and in the tree view the route has
    /// its own places.
    fn child_nodes(&self, node: usize, next: Option<usize>) -> Vec<usize> {
        match self.view {
            View::Links => self
                .graph
                .node(node)
                .targets
                .iter()
                .copied()
                .filter(|&t| Some(t) != next)
                .collect(),
            View::Tree => self
                .graph
                .node(node)
                .children
                .iter()
                .copied()
                .filter(|&c| !self.on_route[c])
                .collect(),
        }
    }

    /// Set item `at`'s handle for its `kids` and its state; whether they are
    /// shown.
    fn fold(&mut self, at: usize, kids: &[usize], default: Fold) -> bool {
        let fold = self
            .folds
            .get(&self.items[at].key)
            .copied()
            .unwrap_or(default);
        self.items[at].handle = match fold {
            _ if kids.is_empty() => Handle::None,
            Fold::Folded => Handle::Folded(kids.to_vec()),
            Fold::Unfolded => Handle::Unfolded,
        };
        self.items[at].handle == Handle::Unfolded
    }

    fn build(&mut self) -> Vec<Block> {
        let route = self.graph.route().to_vec();
        // Items `0..route.len()`: the route as one straight line.
        let mut key = ItemKey(Vec::new());
        for (i, &node) in route.iter().enumerate() {
            key = key.child(node);
            let edge = match i {
                0 => EdgeKind::Link,
                _ if self.graph.node(route[i - 1]).targets.contains(&node) => EdgeKind::Link,
                _ => EdgeKind::Jump,
            };
            self.push(key.clone(), node, i.checked_sub(1), edge);
        }
        let mut blocks = Vec::with_capacity(route.len());
        for (i, &node) in route.iter().enumerate() {
            let next = route.get(i + 1).copied();
            let kids = self.child_nodes(node, next);
            if !self.fold(i, &kids, Fold::Unfolded) {
                blocks.push(Block::Split {
                    above: Vec::new(),
                    below: Vec::new(),
                });
                continue;
            }
            let key = self.items[i].key.clone();
            let placed: Vec<(usize, usize)> = kids
                .into_iter()
                .map(|k| (k, self.node(i, key.child(k), k)))
                .collect();
            let Some(next) = next else {
                blocks.push(Block::Centred(
                    placed.into_iter().map(|(_, at)| at).collect(),
                ));
                continue;
            };
            // Siblings the source lists before the route child go above, those
            // after it below; a step no link explains puts them all below.
            let targets = &self.graph.node(node).targets;
            let rank = |n: usize| targets.iter().position(|&t| t == n);
            let (above, below): (Vec<_>, Vec<_>) = match rank(next) {
                Some(split) => placed
                    .into_iter()
                    .partition(|&(k, _)| rank(k).is_some_and(|r| r < split)),
                None => (Vec::new(), placed),
            };
            blocks.push(Block::Split {
                above: above.into_iter().map(|(_, at)| at).collect(),
                below: below.into_iter().map(|(_, at)| at).collect(),
            });
        }
        blocks
    }

    /// A node off the route, with its children when it is unfolded.
    fn node(&mut self, parent: usize, key: ItemKey, node: usize) -> usize {
        let at = self.push(key.clone(), node, Some(parent), EdgeKind::Link);
        let kids = self.child_nodes(node, None);
        if self.fold(at, &kids, self.view.default_fold()) {
            for k in kids {
                self.node(at, key.child(k), k);
            }
        }
        at
    }
}

// ---------------------------------------------------------------------------
// Packing
// ---------------------------------------------------------------------------

/// Per column, how far from the spine a side is taken: the lowest row used
/// below it, or the highest used above it.
#[derive(Default)]
struct Skyline(Vec<Option<f64>>);

impl Skyline {
    fn get(&self, col: usize) -> Option<f64> {
        self.0.get(col).copied().flatten()
    }

    fn raise(&mut self, col: usize, row: f64, keep: fn(f64, f64) -> f64) {
        if self.0.len() <= col {
            self.0.resize(col + 1, None);
        }
        let slot = &mut self.0[col];
        *slot = Some(slot.map_or(row, |old| keep(old, row)));
    }
}

fn pack(items: &mut [Item], children: &[Vec<usize>], spine: usize, blocks: &[Block]) {
    let (mut below, mut above) = (Skyline::default(), Skyline::default());
    for col in 0..spine {
        below.raise(col, 0.0, f64::max);
        above.raise(col, 0.0, f64::min);
    }
    // Deepest route node first, so the branches nearest the current node sit
    // nearest the line.
    for block in blocks.iter().rev() {
        match block {
            Block::Centred(roots) => {
                let rows = tidy(children, roots);
                let (Some(first), Some(last)) = (roots.first(), roots.last()) else {
                    continue;
                };
                let offset = -(rows[first] + rows[last]) / 2.0;
                for (&at, &row) in &rows {
                    items[at].row = row + offset;
                    below.raise(items[at].col, items[at].row, f64::max);
                    above.raise(items[at].col, items[at].row, f64::min);
                }
            }
            Block::Split {
                above: up,
                below: down,
            } => {
                place(items, children, down, Side::Below, &mut below);
                place(items, children, up, Side::Above, &mut above);
            }
        }
    }
}

/// Place one side's block as close to the spine as the skyline allows.
fn place(
    items: &mut [Item],
    children: &[Vec<usize>],
    roots: &[usize],
    side: Side,
    sky: &mut Skyline,
) {
    if roots.is_empty() {
        return;
    }
    let rows = tidy(children, roots);
    // The block's extent per column, relative to its own first leaf.
    let mut extent: Vec<(usize, f64, f64)> = Vec::new();
    for (&at, &row) in &rows {
        let col = items[at].col;
        match extent.iter_mut().find(|e| e.0 == col) {
            Some(e) => {
                e.1 = e.1.min(row);
                e.2 = e.2.max(row);
            }
            None => extent.push((col, row, row)),
        }
    }
    // One row clear of what is already there, and never on the spine row.
    let offset = match side {
        Side::Below => extent
            .iter()
            .map(|&(col, lo, _)| sky.get(col).unwrap_or(0.0).max(0.0) + 1.0 - lo)
            .fold(f64::NEG_INFINITY, f64::max),
        Side::Above => extent
            .iter()
            .map(|&(col, _, hi)| sky.get(col).unwrap_or(0.0).min(0.0) - 1.0 - hi)
            .fold(f64::INFINITY, f64::min),
    };
    for (&at, &row) in &rows {
        items[at].row = row + offset;
    }
    for (col, lo, hi) in extent {
        match side {
            Side::Below => sky.raise(col, hi + offset, f64::max),
            Side::Above => sky.raise(col, lo + offset, f64::min),
        }
    }
}

/// Tidy-forest rows for `roots` and everything under them: leaves take
/// consecutive rows in depth-first order, a parent sits midway between its
/// first and last child. Rows start at 0.
fn tidy(children: &[Vec<usize>], roots: &[usize]) -> std::collections::BTreeMap<usize, f64> {
    let mut rows = std::collections::BTreeMap::new();
    let mut next = 0.0;
    // Iterative post-order, so a deep chain cannot overflow the stack.
    let mut stack: Vec<(usize, bool)> = roots.iter().rev().map(|&r| (r, false)).collect();
    while let Some((at, done)) = stack.pop() {
        let kids = &children[at];
        if kids.is_empty() {
            rows.insert(at, next);
            next += 1.0;
        } else if done {
            let first = rows[&kids[0]];
            let last = rows[kids.last().expect("not empty")];
            rows.insert(at, (first + last) / 2.0);
        } else {
            stack.push((at, true));
            stack.extend(kids.iter().rev().map(|&c| (c, false)));
        }
    }
    rows
}

/// Runs of two or more sibling nodes with nothing to unfold in this view, off
/// the spine.
fn bundles(items: &[Item], children: &[Vec<usize>], spine: usize) -> Vec<Bundle> {
    // A leaf has no children in this view (no handle): a folded node (`+n`)
    // stays itself.
    let is_leaf = |i: usize| i >= spine && items[i].handle == Handle::None;
    let mut out = Vec::new();
    for (parent, kids) in children.iter().enumerate() {
        let mut kids = kids.clone();
        kids.sort_by(|&a, &b| items[a].row.total_cmp(&items[b].row));
        let mut run: Vec<usize> = Vec::new();
        for k in kids.into_iter().map(Some).chain([None]) {
            match k {
                Some(k) if is_leaf(k) => run.push(k),
                _ => {
                    if run.len() >= 2 {
                        out.push(Bundle {
                            parent,
                            members: std::mem::take(&mut run),
                        });
                    }
                    run.clear();
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{p, table};
    use super::super::{NODE_BUDGET, build};
    use super::*;

    fn at(scene: &Scene, graph: &Graph, title: &str) -> Vec<usize> {
        (0..scene.items().len())
            .filter(|&i| graph.node(scene.item(i).node).title == title)
            .collect()
    }

    fn one(scene: &Scene, graph: &Graph, title: &str) -> usize {
        let found = at(scene, graph, title);
        assert_eq!(found.len(), 1, "{title} is on screen once: {found:?}");
        found[0]
    }

    fn titles(scene: &Scene, graph: &Graph, items: &[usize]) -> Vec<String> {
        items
            .iter()
            .map(|&i| graph.node(scene.item(i).node).title.clone())
            .collect()
    }

    /// Items in one column closer than a row would overlap.
    fn assert_no_overlap(scene: &Scene) {
        for (i, a) in scene.items().iter().enumerate() {
            for b in &scene.items()[i + 1..] {
                if a.col == b.col {
                    assert!(
                        (a.row - b.row).abs() >= 1.0 - 1e-9,
                        "{} and {} share column {} at rows {} / {}",
                        a.key,
                        b.key,
                        a.col,
                        a.row,
                        b.row
                    );
                }
            }
        }
    }

    fn assert_straight_spine(scene: &Scene) {
        for i in scene.spine() {
            let item = scene.item(i);
            assert_eq!(item.row, 0.0, "spine item {i} is on row 0");
            assert_eq!(item.col, i, "spine item {i} is in column {i}");
        }
        for i in scene.spine().end..scene.items().len() {
            let item = scene.item(i);
            assert!(
                !(item.col < scene.spine().end && item.row == 0.0),
                "{} sits on the spine",
                item.key
            );
        }
    }

    /// A route a → b → c through a docs tree with links on both sides of
    /// every step, plus a hub the current node links to.
    fn docs() -> Graph {
        let read = table(&[
            ("a", &["x1", "x2", "b", "y1", "y2", "y3"]),
            ("b", &["u", "c", "w"]),
            ("c", &["d", "e", "a", "f"]),
            ("d", &["g", "h"]),
            ("x1", &["x3"]),
            ("y1", &["y4", "y5"]),
        ]);
        build(&[p("a"), p("b"), p("c")], NODE_BUDGET, read).unwrap()
    }

    /// How many children item `i`'s `+n` hides (0 unless folded).
    fn hidden(scene: &Scene, i: usize) -> usize {
        match &scene.item(i).handle {
            Handle::Folded(kids) => kids.len(),
            Handle::None | Handle::Unfolded => 0,
        }
    }

    fn titles_of(graph: &Graph, handle: &Handle) -> Vec<String> {
        match handle {
            Handle::Folded(kids) => kids.iter().map(|&n| graph.node(n).title.clone()).collect(),
            Handle::None | Handle::Unfolded => Vec::new(),
        }
    }

    fn fold(scene: &Scene, item: usize, folds: &mut Folds) {
        let (key, state) = scene.toggle(item).expect("the item has a handle");
        folds.insert(key, state);
    }

    #[test]
    fn the_route_and_the_current_node_open_unfolded_with_their_siblings() {
        let g = docs();
        let s = Scene::new(&g, View::Links, &Folds::new());
        for i in s.spine() {
            assert_eq!(s.item(i).handle, Handle::Unfolded, "route item {i}");
        }
        // a's other links are b's siblings, in b's column, split around b.
        let x1 = one(&s, &g, "x1");
        let y1 = one(&s, &g, "y1");
        assert_eq!((s.item(x1).col, s.item(y1).col), (1, 1));
        assert!(s.item(x1).row < 0.0 && s.item(y1).row > 0.0);
        // Everything else starts folded in the links view, with its count.
        assert_eq!(hidden(&s, x1), 1);
        assert_eq!(titles_of(&g, &s.item(y1).handle), ["y4", "y5"]);
        assert_eq!(s.item(one(&s, &g, "x2")).handle, Handle::None);
        assert!(at(&s, &g, "x3").is_empty());
    }

    #[test]
    fn the_tree_view_opens_every_node_unfolded_and_once() {
        let g = docs();
        let s = Scene::new(&g, View::Tree, &Folds::new());
        assert_eq!(s.items().len(), g.nodes().len());
        for node in g.nodes() {
            one(&s, &g, &node.title);
        }
        assert!(
            s.items()
                .iter()
                .all(|it| !matches!(it.handle, Handle::Folded(_))),
            "nothing starts folded in the tree view"
        );
        assert!(s.item(one(&s, &g, "x1")).row < 0.0);
        assert!(s.item(one(&s, &g, "y1")).row > 0.0);
    }

    #[test]
    fn folding_a_route_node_hides_its_siblings_but_never_the_route() {
        let g = docs();
        for view in [View::Links, View::Tree] {
            let open = Scene::new(&g, view, &Folds::new());
            let mut folds = Folds::new();
            fold(&open, 0, &mut folds);
            let s = Scene::new(&g, view, &folds);
            assert_eq!(hidden(&s, 0), 5, "{view:?}");
            assert_eq!(s.children(0), [1], "only the next route step");
            assert_eq!(
                titles(&s, &g, &s.spine().collect::<Vec<_>>()),
                ["a", "b", "c"]
            );
            assert!(at(&s, &g, "x1").is_empty() && at(&s, &g, "y1").is_empty());
            assert_straight_spine(&s);
            // Folding the current node closes its fan.
            let mut folds = Folds::new();
            fold(&open, open.current(), &mut folds);
            let s = Scene::new(&g, view, &folds);
            assert!(s.children(s.current()).is_empty());
            assert!(matches!(s.item(s.current()).handle, Handle::Folded(_)));
        }
    }

    #[test]
    fn a_fold_means_the_same_in_both_views() {
        let g = docs();
        let links = Scene::new(&g, View::Links, &Folds::new());
        let mut folds = Folds::new();
        // Unfold x1 in the links view: x3 shows, and still shows in the tree.
        fold(&links, one(&links, &g, "x1"), &mut folds);
        // Fold b, a route node, in the links view: u and w hide in both.
        fold(&links, 1, &mut folds);
        for view in [View::Links, View::Tree] {
            let s = Scene::new(&g, view, &folds);
            assert_eq!(at(&s, &g, "x3").len(), 1, "{view:?}");
            assert!(at(&s, &g, "u").is_empty() && at(&s, &g, "w").is_empty());
        }
        // Fold y1 in the tree view: y4 and y5 hide in both.
        let tree = Scene::new(&g, View::Tree, &folds);
        fold(&tree, one(&tree, &g, "y1"), &mut folds);
        for view in [View::Links, View::Tree] {
            let s = Scene::new(&g, view, &folds);
            assert!(at(&s, &g, "y4").is_empty(), "{view:?}");
            assert_eq!(hidden(&s, one(&s, &g, "y1")), 2);
        }
    }

    #[test]
    fn the_spine_is_straight_and_nothing_overlaps_in_either_view() {
        let g = docs();
        let mut all = Folds::new();
        // Everything a reader could open one level deep in the links view.
        let links = Scene::new(&g, View::Links, &Folds::new());
        for it in links.items() {
            all.insert(it.key.clone(), Fold::Unfolded);
        }
        let mut route_folded = Folds::new();
        for i in links.spine() {
            route_folded.insert(links.item(i).key.clone(), Fold::Folded);
        }
        for view in [View::Links, View::Tree] {
            for folds in [Folds::new(), all.clone(), route_folded.clone()] {
                let s = Scene::new(&g, view, &folds);
                assert_straight_spine(&s);
                assert_no_overlap(&s);
            }
        }
    }

    #[test]
    fn fifty_siblings_keep_the_spine_straight_and_the_fan_near_it() {
        let many: Vec<String> = (0..50).map(|i| format!("s{i:02}")).collect();
        let mut root: Vec<&str> = many[..25].iter().map(String::as_str).collect();
        root.push("b");
        root.extend(many[25..].iter().map(String::as_str));
        let read = table(&[("a", &root), ("b", &["f1", "f2", "f3"])]);
        let g = build(&[p("a"), p("b")], NODE_BUDGET, read).unwrap();
        let s = Scene::new(&g, View::Links, &Folds::new());
        assert_straight_spine(&s);
        assert_no_overlap(&s);
        let fan: Vec<f64> = s.children(1).iter().map(|&i| s.item(i).row).collect();
        assert_eq!(
            fan,
            [-1.0, 0.0, 1.0],
            "the current node's fan stays centred"
        );
    }

    #[test]
    fn links_fans_out_the_current_node_with_duplicates() {
        let g = docs();
        let s = Scene::new(&g, View::Links, &Folds::new());
        let c = s.current();
        assert_eq!(titles(&s, &g, s.children(c)), ["d", "e", "a", "f"]);
        assert_eq!(
            at(&s, &g, "a").len(),
            2,
            "the root, and the link back to it"
        );
        let rows: Vec<f64> = s.children(c).iter().map(|&i| s.item(i).row).collect();
        assert_eq!(rows, [-1.5, -0.5, 0.5, 1.5]);
    }

    #[test]
    fn keys_carry_the_selection_across_views() {
        let g = docs();
        let links = Scene::new(&g, View::Links, &Folds::new());
        let tree = Scene::new(&g, View::Tree, &Folds::new());
        // A fan item and a sibling are the same item in the tree.
        for title in ["d", "y1"] {
            let i = one(&links, &g, title);
            assert_eq!(
                tree.item(tree.find(&links.item(i).key)).key,
                links.item(i).key
            );
        }
        // A tree node the links view keeps folded away falls back to the
        // folded node that hides it: x3 is under x1.
        let x3 = one(&tree, &g, "x3");
        assert_eq!(links.find(&tree.item(x3).key), one(&links, &g, "x1"));
        for i in links.spine() {
            assert_eq!(tree.find(&links.item(i).key), i);
        }
        for it in links.items() {
            assert_eq!(it.key.to_string().parse::<ItemKey>(), Ok(it.key.clone()));
        }
        assert!("0.x".parse::<ItemKey>().is_err());
    }

    #[test]
    fn steps_move_through_columns_and_rows() {
        let g = docs();
        let s = Scene::new(&g, View::Tree, &Folds::new());
        let at = |t: &str| one(&s, &g, t);
        assert_eq!(s.step(0, Step::Child), 1, "the spine child wins");
        assert_eq!(s.step(at("x1"), Step::Child), at("x3"));
        assert_eq!(s.step(at("x1"), Step::Down), at("x2"));
        assert_eq!(s.step(at("x2"), Step::Up), at("x1"));
        assert_eq!(s.step(at("d"), Step::Parent), s.current());
        assert_eq!(s.step(0, Step::Parent), 0);
        assert_eq!(s.step(s.current(), Step::Child), at("d"));
    }

    #[test]
    fn a_handle_toggles_and_a_leaf_has_none() {
        let g = docs();
        let s = Scene::new(&g, View::Links, &Folds::new());
        let x1 = one(&s, &g, "x1");
        assert_eq!(s.toggle(x1), Some((s.item(x1).key.clone(), Fold::Unfolded)));
        assert_eq!(s.toggle(0), Some((s.item(0).key.clone(), Fold::Folded)));
        assert_eq!(s.toggle(one(&s, &g, "x2")), None);
    }

    #[test]
    fn a_jump_on_the_route_is_a_dashed_spine_step() {
        // The reader jumped from c to b; c does not link b.
        let read = table(&[("a", &["b", "c"])]);
        let g = build(&[p("a"), p("c"), p("b")], NODE_BUDGET, read).unwrap();
        for view in [View::Links, View::Tree] {
            let s = Scene::new(&g, view, &Folds::new());
            assert_eq!(s.item(1).edge, EdgeKind::Link);
            assert_eq!(s.item(2).edge, EdgeKind::Jump);
            assert_straight_spine(&s);
        }
    }

    #[test]
    fn sibling_leaves_bundle_and_nodes_with_children_break_the_run() {
        let read = table(&[("a", &["b", "c", "d", "e", "f"]), ("d", &["g"])]);
        let g = build(&[p("a")], NODE_BUDGET, read).unwrap();
        for view in [View::Links, View::Tree] {
            let s = Scene::new(&g, view, &Folds::new());
            let runs: Vec<usize> = s.bundles().iter().map(|b| b.members.len()).collect();
            assert_eq!(runs, [2, 2], "b c | d | e f ({view:?})");
        }
    }

    #[test]
    fn views_parse_and_round_trip() {
        for v in [View::Links, View::Tree] {
            assert_eq!(View::parse(v.name()), Ok(v));
            assert_eq!(v.toggled().toggled(), v);
        }
        assert_eq!(View::parse(" Tree "), Ok(View::Tree));
        assert!(View::parse("forest").is_err());
    }
}
