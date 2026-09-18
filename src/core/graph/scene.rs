//! The scene (DESIGN D14): which items are on screen and where.
//!
//! A [`Scene`] is derived, never edited: it is a function of the [`Graph`],
//! the [`View`] and the set of [`Expanded`] items. Expanding, collapsing or
//! switching the view builds a new one; an item's [`ItemKey`] is what carries
//! the selection across.
//!
//! The layout is a grid: an item sits in a column (link steps from the root
//! along the displayed tree) and on a row (in item heights, `0` is the spine,
//! negative is above). Three passes:
//!
//! 1. **The displayed tree.** The route root → current takes items
//!    `0..spine`, one column per step. Around it, per view: *links* fans out
//!    the current note's links and folds every other spine note's remaining
//!    links into a cluster above and one below; *tree* hangs the spanning
//!    tree's other notes where the walk put them.
//! 2. **Packing.** The current note's children are one block centred on the
//!    spine row. Every other spine note's children form a block above and a
//!    block below, split by link order around the route child. Blocks are tidy
//!    forests (leaves on consecutive rows, a parent midway between its first
//!    and last child), placed deepest spine note first against a per-column
//!    skyline, each as close to the spine as the columns it spans allow.
//! 3. **Bundles.** Runs of two or more sibling leaves, which the far zoom
//!    level draws as one bar.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use super::{EdgeKind, Graph};

/// What the scene shows around the spine (option `graph-view`, `v`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum View {
    /// What the current note leads to: its links fanned out, one item per
    /// link; the other spine notes' links folded into clusters.
    #[default]
    Links,
    /// The whole spanning tree, every note once.
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
}

/// Which side of the spine an item hangs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    Above,
    Below,
}

/// One step of an [`ItemKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Segment {
    /// A note, by node index.
    Note(usize),
    /// A spine note's cluster on one side.
    Cluster(Side),
}

/// An item's identity across re-layouts: the node indices from the root to
/// it along the displayed tree. A cluster adds its side; the notes inside a
/// cluster do not name it, so a note keeps its key between the views (a
/// spine note's link is `spine … / note` in both).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemKey(Vec<Segment>);

impl ItemKey {
    fn child(&self, segment: Segment) -> Self {
        let mut path = self.0.clone();
        path.push(segment);
        Self(path)
    }

    /// The note this key ends on, if it ends on one.
    fn note(&self) -> Option<usize> {
        match self.0.last() {
            Some(Segment::Note(n)) => Some(*n),
            _ => None,
        }
    }
}

impl fmt::Display for ItemKey {
    /// Dot-separated, a cluster as `a` (above) or `b` (below): `0.4.b`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, segment) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            match segment {
                Segment::Note(n) => write!(f, "{n}")?,
                Segment::Cluster(Side::Above) => f.write_str("a")?,
                Segment::Cluster(Side::Below) => f.write_str("b")?,
            }
        }
        Ok(())
    }
}

impl FromStr for ItemKey {
    type Err = String;

    /// The [`Display`](fmt::Display) spelling back: what the overlay posts.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.split('.')
            .map(|segment| match segment {
                "a" => Ok(Segment::Cluster(Side::Above)),
                "b" => Ok(Segment::Cluster(Side::Below)),
                n => n
                    .parse()
                    .map(Segment::Note)
                    .map_err(|_| format!("not an item key: {s:?}")),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }
}

/// The items the reader has expanded. Keys that name nothing in the current
/// scene are kept: collapsing a cluster and opening it again restores what
/// was open inside it.
pub type Expanded = BTreeSet<ItemKey>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    /// A note, by node index. With duplicates (links view) a note can be on
    /// screen more than once.
    Note(usize),
    /// A spine note's remaining links on one side, folded into one item.
    Cluster { side: Side, members: Vec<usize> },
}

/// Whether an item's children are on screen, and whether that can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// Nothing to show.
    Leaf,
    /// This many children are hidden; `l` shows them.
    Collapsed(usize),
    /// Children shown; `h` hides them.
    Expanded,
    /// Children shown, always: the spine, and every note in the tree view.
    Fixed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub key: ItemKey,
    pub kind: ItemKind,
    pub col: usize,
    /// In item heights; `0` is the spine, negative is above it.
    pub row: f64,
    /// `None` only for the root, item 0.
    pub parent: Option<usize>,
    /// How the item hangs under its parent. Only a spine step can be a jump.
    pub edge: EdgeKind,
    pub fold: Fold,
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

/// What `h` or `l` does to the selected item (zathura's TOC semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Select(usize),
    Expand,
    Collapse,
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
    /// Lay out `graph` for `view` with `expanded` open.
    pub fn new(graph: &Graph, view: View, expanded: &Expanded) -> Self {
        let mut builder = Builder {
            graph,
            expanded,
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
        let blocks = match view {
            View::Links => builder.links(),
            View::Tree => builder.tree(),
        };
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

    /// The current note's item: the end of the spine.
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
    /// stand-in: the first item showing the same note, else the deepest item
    /// on the key's path (or the collapsed cluster below it that holds the
    /// path's next note), else the current note.
    pub fn find(&self, key: &ItemKey) -> usize {
        if let Some(i) = self.exact(key) {
            return i;
        }
        let same_note = key.note().and_then(|n| {
            self.items
                .iter()
                .position(|it| it.kind == ItemKind::Note(n))
        });
        same_note
            .or_else(|| {
                (1..key.0.len()).rev().find_map(|len| {
                    let at = self.exact(&ItemKey(key.0[..len].to_vec()))?;
                    let Segment::Note(next) = key.0[len] else {
                        return Some(at);
                    };
                    let cluster = self.children[at].iter().copied().find(|&c| {
                        matches!(&self.items[c].kind,
                            ItemKind::Cluster { members, .. } if members.contains(&next))
                    });
                    Some(cluster.unwrap_or(at))
                })
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

    /// `l`: expand a collapsed item, else descend.
    pub fn descend(&self, from: usize) -> Move {
        match self.items[from].fold {
            Fold::Collapsed(_) => Move::Expand,
            _ => Move::Select(self.step(from, Step::Child)),
        }
    }

    /// `h`: collapse an expanded item, else ascend.
    pub fn ascend(&self, from: usize) -> Move {
        match self.items[from].fold {
            Fold::Expanded => Move::Collapse,
            _ => Move::Select(self.step(from, Step::Parent)),
        }
    }
}

// ---------------------------------------------------------------------------
// The displayed tree
// ---------------------------------------------------------------------------

/// A spine note's off-spine children, grouped the way they are packed.
enum Block {
    /// Above and below the spine (every spine note but the current one).
    Split {
        above: Vec<usize>,
        below: Vec<usize>,
    },
    /// Centred on the spine row: the current note's children.
    Centred(Vec<usize>),
}

struct Builder<'a> {
    graph: &'a Graph,
    expanded: &'a Expanded,
    /// Per node: whether it is on the route.
    on_route: Vec<bool>,
    items: Vec<Item>,
    children: Vec<Vec<usize>>,
}

impl Builder<'_> {
    fn push(
        &mut self,
        key: ItemKey,
        kind: ItemKind,
        parent: Option<usize>,
        edge: EdgeKind,
        fold: Fold,
    ) -> usize {
        let at = self.items.len();
        let col = parent.map_or(0, |p| self.items[p].col + 1);
        if let Some(p) = parent {
            self.children[p].push(at);
        }
        self.children.push(Vec::new());
        self.items.push(Item {
            key,
            kind,
            col,
            row: 0.0,
            parent,
            edge,
            fold,
        });
        at
    }

    /// Items `0..route.len()`: the route as one straight line.
    fn spine(&mut self) {
        let route = self.graph.route();
        let mut key = ItemKey(Vec::new());
        for (i, &node) in route.iter().enumerate() {
            key = key.child(Segment::Note(node));
            let edge = match i {
                0 => EdgeKind::Link,
                _ if self.graph.node(route[i - 1]).targets.contains(&node) => EdgeKind::Link,
                _ => EdgeKind::Jump,
            };
            self.push(
                key.clone(),
                ItemKind::Note(node),
                i.checked_sub(1),
                edge,
                Fold::Fixed,
            );
        }
    }

    fn links(&mut self) -> Vec<Block> {
        self.spine();
        let route = self.graph.route();
        let mut blocks = Vec::with_capacity(route.len());
        for (i, &node) in route.iter().enumerate() {
            let targets = &self.graph.node(node).targets;
            let key = self.items[i].key.clone();
            let Some(&next) = route.get(i + 1) else {
                let fan = targets
                    .iter()
                    .map(|&t| self.link_note(i, key.child(Segment::Note(t)), t))
                    .collect();
                blocks.push(Block::Centred(fan));
                break;
            };
            // Links the source lists before the route child go above, those
            // after it below; a step no link explains puts them all below.
            let (above, below): (Vec<usize>, Vec<usize>) =
                match targets.iter().position(|&t| t == next) {
                    Some(at) => (targets[..at].to_vec(), targets[at + 1..].to_vec()),
                    None => (Vec::new(), targets.clone()),
                };
            let above = self.cluster(i, &key, Side::Above, above);
            let below = self.cluster(i, &key, Side::Below, below);
            blocks.push(Block::Split {
                above: above.into_iter().collect(),
                below: below.into_iter().collect(),
            });
        }
        blocks
    }

    /// Spine note `spine`'s links on one side as a cluster, with its members
    /// when expanded. `None` when there are none.
    fn cluster(
        &mut self,
        spine: usize,
        key: &ItemKey,
        side: Side,
        members: Vec<usize>,
    ) -> Option<usize> {
        if members.is_empty() {
            return None;
        }
        let cluster_key = key.child(Segment::Cluster(side));
        let open = self.expanded.contains(&cluster_key);
        let fold = if open {
            Fold::Expanded
        } else {
            Fold::Collapsed(members.len())
        };
        let at = self.push(
            cluster_key,
            ItemKind::Cluster {
                side,
                members: members.clone(),
            },
            Some(spine),
            EdgeKind::Link,
            fold,
        );
        if open {
            for m in members {
                self.link_note(at, key.child(Segment::Note(m)), m);
            }
        }
        Some(at)
    }

    /// A note in the links view: its own links shown only when expanded.
    fn link_note(&mut self, parent: usize, key: ItemKey, node: usize) -> usize {
        let targets = &self.graph.node(node).targets;
        let open = self.expanded.contains(&key);
        let fold = match targets.len() {
            0 => Fold::Leaf,
            _ if open => Fold::Expanded,
            n => Fold::Collapsed(n),
        };
        let at = self.push(
            key.clone(),
            ItemKind::Note(node),
            Some(parent),
            EdgeKind::Link,
            fold,
        );
        if open {
            for &t in targets {
                self.link_note(at, key.child(Segment::Note(t)), t);
            }
        }
        at
    }

    fn tree(&mut self) -> Vec<Block> {
        self.spine();
        let route = self.graph.route();
        let mut blocks = Vec::with_capacity(route.len());
        for (i, &node) in route.iter().enumerate() {
            let key = self.items[i].key.clone();
            let kids: Vec<usize> = self.off_spine_children(node).collect();
            let placed: Vec<(usize, usize)> = kids
                .into_iter()
                .map(|k| (k, self.tree_note(i, key.child(Segment::Note(k)), k)))
                .collect();
            let Some(&next) = route.get(i + 1) else {
                blocks.push(Block::Centred(
                    placed.into_iter().map(|(_, at)| at).collect(),
                ));
                break;
            };
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

    /// A note in the tree view, with its whole subtree.
    fn tree_note(&mut self, parent: usize, key: ItemKey, node: usize) -> usize {
        let kids: Vec<usize> = self.off_spine_children(node).collect();
        let fold = if kids.is_empty() {
            Fold::Leaf
        } else {
            Fold::Fixed
        };
        let at = self.push(
            key.clone(),
            ItemKind::Note(node),
            Some(parent),
            EdgeKind::Link,
            fold,
        );
        for k in kids {
            self.tree_note(at, key.child(Segment::Note(k)), k);
        }
        at
    }

    /// A node's tree children that are not on the route: the route has its
    /// own places on the spine.
    fn off_spine_children(&self, node: usize) -> impl Iterator<Item = usize> + '_ {
        self.graph
            .node(node)
            .children
            .iter()
            .copied()
            .filter(|&c| !self.on_route[c])
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
    // Deepest spine note first, so the branches nearest the current note sit
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

/// Runs of two or more sibling notes with no links of their own, off the
/// spine.
fn bundles(items: &[Item], children: &[Vec<usize>], spine: usize) -> Vec<Bundle> {
    // A leaf has no links of its own: a collapsed note (`+n`) stays itself.
    let is_leaf = |i: usize| {
        i >= spine && items[i].fold == Fold::Leaf && matches!(items[i].kind, ItemKind::Note(_))
    };
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
            .filter(|&i| {
                matches!(scene.item(i).kind, ItemKind::Note(n) if graph.node(n).title == title)
            })
            .collect()
    }

    fn one(scene: &Scene, graph: &Graph, title: &str) -> usize {
        let found = at(scene, graph, title);
        assert_eq!(found.len(), 1, "{title} is on screen once: {found:?}");
        found[0]
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
    /// every step, plus a hub the current note links to.
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

    #[test]
    fn the_spine_is_straight_and_nothing_overlaps_in_either_view() {
        let g = docs();
        let mut all = Expanded::new();
        // Everything a reader could open: both clusters on every spine note,
        // and the fan's notes.
        let links = Scene::new(&g, View::Links, &all);
        for it in links.items() {
            all.insert(it.key.clone());
        }
        for view in [View::Links, View::Tree] {
            for expanded in [Expanded::new(), all.clone()] {
                let s = Scene::new(&g, view, &expanded);
                assert_straight_spine(&s);
                assert_no_overlap(&s);
            }
        }
    }

    #[test]
    fn links_fans_out_the_current_note_with_duplicates() {
        let g = docs();
        let s = Scene::new(&g, View::Links, &Expanded::new());
        let c = s.current();
        let fan: Vec<&str> = s
            .children(c)
            .iter()
            .map(|&i| match s.item(i).kind {
                ItemKind::Note(n) => g.node(n).title.as_str(),
                _ => "cluster",
            })
            .collect();
        assert_eq!(
            fan,
            ["d", "e", "a", "f"],
            "one item per link, in link order"
        );
        assert_eq!(
            at(&s, &g, "a").len(),
            2,
            "the root, and the link back to it"
        );
        // The fan is centred on the spine row.
        let rows: Vec<f64> = s.children(c).iter().map(|&i| s.item(i).row).collect();
        assert_eq!(rows, [-1.5, -0.5, 0.5, 1.5]);
    }

    #[test]
    fn links_folds_the_other_spine_notes_into_counted_clusters() {
        let g = docs();
        let s = Scene::new(&g, View::Links, &Expanded::new());
        let clusters = |spine: usize| -> Vec<(Side, usize, Fold)> {
            s.children(spine)
                .iter()
                .filter_map(|&i| match &s.item(i).kind {
                    ItemKind::Cluster { side, members } => {
                        Some((*side, members.len(), s.item(i).fold))
                    }
                    ItemKind::Note(_) => None,
                })
                .collect()
        };
        assert_eq!(
            clusters(0),
            [
                (Side::Above, 2, Fold::Collapsed(2)),
                (Side::Below, 3, Fold::Collapsed(3))
            ]
        );
        assert_eq!(
            clusters(1),
            [
                (Side::Above, 1, Fold::Collapsed(1)),
                (Side::Below, 1, Fold::Collapsed(1))
            ]
        );
        for i in s
            .items()
            .iter()
            .filter(|it| matches!(it.kind, ItemKind::Cluster { .. }))
        {
            match &i.kind {
                ItemKind::Cluster {
                    side: Side::Above, ..
                } => assert!(i.row < 0.0),
                _ => assert!(i.row > 0.0),
            }
        }
    }

    #[test]
    fn tree_shows_every_note_exactly_once() {
        let g = docs();
        let s = Scene::new(&g, View::Tree, &Expanded::new());
        assert_eq!(s.items().len(), g.nodes().len());
        for node in g.nodes() {
            one(&s, &g, &node.title);
        }
        // Split by link order around the route child: a lists x1, x2 before b.
        assert!(s.item(one(&s, &g, "x1")).row < 0.0);
        assert!(s.item(one(&s, &g, "y1")).row > 0.0);
        assert!(
            s.items()
                .iter()
                .all(|it| !matches!(it.fold, Fold::Collapsed(_) | Fold::Expanded)),
            "nothing folds in the tree view"
        );
    }

    #[test]
    fn expanding_a_cluster_shows_its_members_and_keeps_the_rest() {
        let g = docs();
        let closed = Scene::new(&g, View::Links, &Expanded::new());
        let below = closed.children(0)[2];
        assert_eq!(closed.descend(below), Move::Expand);

        let mut expanded = Expanded::new();
        expanded.insert(closed.item(below).key.clone());
        let open = Scene::new(&g, View::Links, &expanded);
        let cluster = open.find(&closed.item(below).key);
        assert_eq!(open.item(cluster).fold, Fold::Expanded);
        assert_eq!(open.children(cluster).len(), 3);
        assert_eq!(open.ascend(cluster), Move::Collapse);
        // The members keep the tree view's keys, so `v` finds them there.
        let y1 = open.children(cluster)[0];
        let tree = Scene::new(&g, View::Tree, &expanded);
        assert_eq!(
            tree.item(tree.find(&open.item(y1).key)).key,
            open.item(y1).key
        );
        // Everything that was on screen still is.
        for it in closed.items() {
            assert_eq!(open.item(open.find(&it.key)).key, it.key);
        }
    }

    #[test]
    fn keys_carry_the_selection_across_views() {
        let g = docs();
        let links = Scene::new(&g, View::Links, &Expanded::new());
        let tree = Scene::new(&g, View::Tree, &Expanded::new());
        // A fan item is the same note in the tree, wherever the walk put it.
        let d = links.children(links.current())[0];
        let found = tree.find(&links.item(d).key);
        assert_eq!(tree.item(found).kind, links.item(d).kind);
        // A tree note the links view folded away lands on the cluster that
        // holds its branch: x3 hangs under x1, which is in a's cluster above.
        let x3 = one(&tree, &g, "x3");
        let above = links.find(&tree.item(x3).key);
        assert!(
            matches!(
                &links.item(above).kind,
                ItemKind::Cluster {
                    side: Side::Above,
                    ..
                }
            ),
            "{}",
            links.item(above).key
        );
        assert_eq!(links.item(above).parent, Some(0));
        // A key round-trips through its spelling, which the overlay posts.
        for it in links.items() {
            assert_eq!(it.key.to_string().parse::<ItemKey>(), Ok(it.key.clone()));
        }
        assert!("0.x".parse::<ItemKey>().is_err());
        // The spine is the spine in both.
        for i in links.spine() {
            assert_eq!(tree.find(&links.item(i).key), i);
        }
    }

    #[test]
    fn steps_move_through_columns_and_rows() {
        let g = docs();
        let s = Scene::new(&g, View::Tree, &Expanded::new());
        let at = |t: &str| one(&s, &g, t);
        assert_eq!(s.step(0, Step::Child), 1, "the spine child wins");
        assert_eq!(s.step(at("x1"), Step::Child), at("x3"));
        assert_eq!(s.step(at("x1"), Step::Down), at("x2"));
        assert_eq!(s.step(at("x2"), Step::Up), at("x1"));
        assert_eq!(s.step(at("d"), Step::Parent), s.current());
        assert_eq!(s.step(0, Step::Parent), 0);
        assert_eq!(
            s.descend(s.current()),
            Move::Select(at("d")),
            "the current note descends into its first link"
        );
        assert_eq!(s.ascend(at("d")), Move::Select(s.current()));
        assert_eq!(s.ascend(1), Move::Select(0), "the spine never collapses");
    }

    #[test]
    fn a_jump_on_the_route_is_a_dashed_spine_step() {
        // The reader jumped from c to b; c does not link b.
        let read = table(&[("a", &["b", "c"])]);
        let g = build(&[p("a"), p("c"), p("b")], NODE_BUDGET, read).unwrap();
        for view in [View::Links, View::Tree] {
            let s = Scene::new(&g, view, &Expanded::new());
            assert_eq!(s.item(1).edge, EdgeKind::Link);
            assert_eq!(s.item(2).edge, EdgeKind::Jump);
            assert_straight_spine(&s);
        }
    }

    #[test]
    fn sibling_leaves_bundle_and_inner_notes_break_the_run() {
        let read = table(&[("a", &["b", "c", "d", "e", "f"]), ("d", &["g"])]);
        let g = build(&[p("a")], NODE_BUDGET, read).unwrap();
        let s = Scene::new(&g, View::Tree, &Expanded::new());
        let runs: Vec<usize> = s.bundles().iter().map(|b| b.members.len()).collect();
        assert_eq!(runs, [2, 2], "b c | d | e f");
        for b in s.bundles() {
            let rows: Vec<f64> = b.members.iter().map(|&m| s.item(m).row).collect();
            assert!(rows.windows(2).all(|w| w[1] - w[0] == 1.0), "{rows:?}");
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
