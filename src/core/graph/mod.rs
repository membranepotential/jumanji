//! The document graph (DESIGN D14): the notes reachable by links from where the
//! reading session started, laid out around the route the reader took.
//!
//! Pure. Three stages, each testable alone:
//!
//! 1. [`scan`] — one note's title and outgoing links, from its markdown.
//! 2. [`build`] — the breadth-first walk from the trail's root that turns the
//!    (cyclic) link graph into a spanning tree, with the trail pinned into it
//!    where a link explains a step. File access is the caller's closure, so the
//!    walk is tested over a map.
//! 3. [`Scene`] — what is on screen for one [`View`] and set of expanded
//!    items: the route as a straight spine, everything else packed above and
//!    below it ([`scene`]), written out as inline SVG ([`svg`]).
//!    Deterministic: the same files and the same expansions always give the
//!    same picture.

mod scene;
mod svg;

pub use scene::{Expanded, Fold, Item, ItemKey, ItemKind, Move, Scene, Side, Step, View};

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use comrak::nodes::NodeValue;
use comrak::{Arena, parse_document};

use super::frontmatter::{self, Value};
use super::obsidian::{self, RefKind, percent_decode};
use super::pipeline::comrak_options;
use super::vault::{Target, VaultIndex};

/// How many notes the walk places at most. Route notes are always placed, even
/// past the budget; the tree view marks a note whose links were cut with `+n`.
pub const NODE_BUDGET: usize = 300;

/// How many bytes of a note the walk reads. Links and titles sit in the text a
/// person wrote; a note larger than this is data, not a hub.
pub const READ_CAP: u64 = 1 << 20;

/// What the walk needs to know about one note.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    /// Frontmatter `title`, else the first `# H1`. `None` falls back to the
    /// file stem at placement.
    pub title: Option<String>,
    /// Outgoing links to markdown notes, in source order, without duplicates.
    /// [`scan`] leaves them as written (joined, not canonical); the caller that
    /// hands notes to [`build`] canonicalises them and drops missing files.
    pub links: Vec<PathBuf>,
}

/// Read one note's title and outgoing markdown links.
///
/// Links follow the reader's own routing (`Controller::on_navigate`): a
/// markdown link is a path relative to the note's directory, a wikilink
/// resolves through the vault index, and only `.md` / `.markdown` targets count
/// — a directory, an image or a web page is not a note.
pub fn scan(md: &str, source: &Path, index: &VaultIndex) -> Note {
    let arena = Arena::new();
    let root = parse_document(&arena, md, &comrak_options());
    let dir = source.parent().unwrap_or(Path::new(""));

    let mut front_title = None;
    let mut h1 = None;
    let mut links: Vec<PathBuf> = Vec::new();
    for node in root.descendants() {
        let target = match &node.data.borrow().value {
            NodeValue::FrontMatter(block) => {
                front_title = title_property(block);
                None
            }
            NodeValue::Heading(h) if h.level == 1 && h1.is_none() => {
                h1 = Some(node.collect_text());
                None
            }
            NodeValue::Link(link) => markdown_target(&link.url, dir),
            NodeValue::WikiLink(link) => {
                let reference = obsidian::parse(&percent_decode(&link.url), RefKind::Link);
                match index.resolve(&reference, source) {
                    Target::Note { path, .. } => Some(path),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(path) = target.filter(|p| is_markdown(p))
            && !links.contains(&path)
        {
            links.push(path);
        }
    }
    let title = front_title
        .or(h1)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    Note { title, links }
}

/// The frontmatter `title`, if it is a plain non-empty value.
fn title_property(block: &str) -> Option<String> {
    frontmatter::parse(block)
        .into_iter()
        .find(|p| p.key.eq_ignore_ascii_case("title"))
        .and_then(|p| match p.value {
            Value::Scalar(s) if !s.trim().is_empty() => Some(s),
            _ => None,
        })
}

/// The file a markdown link's `url` names, or `None` for a web link, a
/// same-document fragment or anything else that is not a local path.
fn markdown_target(url: &str, dir: &Path) -> Option<PathBuf> {
    let url = url.split(['#', '?']).next().unwrap_or("");
    if url.is_empty() {
        return None;
    }
    let path = match url.strip_prefix("file://") {
        Some(rest) => PathBuf::from(percent_decode(rest)),
        None if has_scheme(url) => return None,
        None => PathBuf::from(percent_decode(url)),
    };
    Some(if path.is_absolute() {
        path
    } else {
        dir.join(path)
    })
}

/// Whether `url` starts with a URI scheme (`https:`, `mailto:`). A single
/// letter before the colon is not one — that is a Windows drive, never a
/// scheme anyone links with.
fn has_scheme(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() > 1
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

/// How a node hangs under its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// The parent links to the node.
    Link,
    /// No link explains the edge: a route note the walk could not reach, hung
    /// under the note the reader came from.
    Jump,
}

/// A node's place in the tree: its parent's index and the kind of edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parent {
    pub index: usize,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub path: PathBuf,
    pub title: String,
    /// `None` only for the root, index 0.
    pub parent: Option<Parent>,
    pub children: Vec<usize>,
    /// Link distance from the root along the tree.
    pub depth: usize,
    /// Links cut by [`NODE_BUDGET`]. Every other link is in `targets`.
    pub hidden: usize,
    /// Every placed note this note links to, in source order — its tree
    /// children and the cross-links the tree does not draw. The links view
    /// fans these out; the tree view draws them for the selected note.
    pub targets: Vec<usize>,
}

/// The document tree: node 0 is the root, and `route` indexes the trail from
/// the root to the current document.
#[derive(Debug, Clone, PartialEq)]
pub struct Graph {
    nodes: Vec<Node>,
    route: Vec<usize>,
}

/// Build the tree for `trail` (root first, current document last; paths in
/// the same canonical form `read` returns links in). `None` for an empty trail.
///
/// `read` is called at most once per note and must return links already
/// canonical and existing (see [`Note::links`]).
pub fn build(trail: &[PathBuf], budget: usize, read: impl FnMut(&Path) -> Note) -> Option<Graph> {
    let route = loop_free(trail);
    let root = route.first()?.clone();
    let mut walk = Walk {
        read,
        notes: HashMap::new(),
        nodes: Vec::new(),
        index: HashMap::new(),
        on_route: route.iter().cloned().collect(),
        budget,
    };

    let first = walk.place(root, None);
    walk.expand(first);
    // Route notes the walk could not reach hang under the note the reader
    // came from — in trail order, so that predecessor is always placed.
    for pair in route.windows(2) {
        if walk.index.contains_key(&pair[1]) {
            continue;
        }
        let from = walk.index[&pair[0]];
        let kind = if walk.note(&pair[0]).links.contains(&pair[1]) {
            EdgeKind::Link
        } else {
            EdgeKind::Jump
        };
        let placed = walk.place(pair[1].clone(), Some(Parent { index: from, kind }));
        walk.expand(placed);
    }

    // Pin the route where a link explains a step: a note the reader reached by
    // following a link hangs under the note they followed it from, taking its
    // subtree along. Done on the finished tree rather than during the walk, so
    // a pin can never make a note unreachable — and one that would make a node
    // its own ancestor (the reader went back up a chain) is simply skipped.
    for pair in route.windows(2) {
        let (from, to) = (walk.index[&pair[0]], walk.index[&pair[1]]);
        let linked = walk.note(&pair[0]).links.contains(&pair[1]);
        if linked && walk.nodes[to].parent.map(|p| p.index) != Some(from) {
            walk.reparent(to, from);
        }
    }
    walk.assign_depths();

    // Every placed note was expanded, so its links are in hand.
    for node in &mut walk.nodes {
        node.targets = walk.notes[&node.path]
            .links
            .iter()
            .filter_map(|link| walk.index.get(link).copied())
            .collect();
    }
    let route = route.iter().map(|p| walk.index[p]).collect();
    Some(Graph {
        nodes: walk.nodes,
        route,
    })
}

/// Cut every revisit back to the first visit, so the route is a path:
/// `A > B > C > B` is `A > B`.
fn loop_free(trail: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for doc in trail {
        match out.iter().position(|d| d == doc) {
            Some(i) => out.truncate(i + 1),
            None => out.push(doc.clone()),
        }
    }
    out
}

struct Walk<R> {
    read: R,
    notes: HashMap<PathBuf, Note>,
    nodes: Vec<Node>,
    index: HashMap<PathBuf, usize>,
    on_route: HashSet<PathBuf>,
    budget: usize,
}

impl<R: FnMut(&Path) -> Note> Walk<R> {
    fn note(&mut self, path: &Path) -> &Note {
        if !self.notes.contains_key(path) {
            let note = (self.read)(path);
            self.notes.insert(path.to_path_buf(), note);
        }
        &self.notes[path]
    }

    fn place(&mut self, path: PathBuf, parent: Option<Parent>) -> usize {
        let title = self
            .note(&path)
            .title
            .clone()
            .unwrap_or_else(|| stem(&path));
        let at = self.nodes.len();
        let depth = parent.map_or(0, |p| self.nodes[p.index].depth + 1);
        if let Some(p) = parent {
            self.nodes[p.index].children.push(at);
        }
        self.index.insert(path.clone(), at);
        self.nodes.push(Node {
            path,
            title,
            parent,
            children: Vec::new(),
            depth,
            hidden: 0,
            targets: Vec::new(),
        });
        at
    }

    /// Breadth-first from `start`: each unplaced link target becomes a child of
    /// the first note that reaches it.
    fn expand(&mut self, start: usize) {
        let mut queue = VecDeque::from([start]);
        while let Some(at) = queue.pop_front() {
            let path = self.nodes[at].path.clone();
            let links = self.note(&path).links.clone();
            for target in links {
                if self.index.contains_key(&target) {
                    continue;
                }
                if self.nodes.len() >= self.budget && !self.on_route.contains(&target) {
                    self.nodes[at].hidden += 1;
                    continue;
                }
                let child = self.place(
                    target,
                    Some(Parent {
                        index: at,
                        kind: EdgeKind::Link,
                    }),
                );
                queue.push_back(child);
            }
        }
    }
}

impl<R: FnMut(&Path) -> Note> Walk<R> {
    /// Move `node` (with its subtree) under `parent`, in `parent`'s link order.
    /// A no-op when `node` is `parent` or one of its ancestors.
    fn reparent(&mut self, node: usize, parent: usize) {
        let mut at = Some(parent);
        while let Some(i) = at {
            if i == node {
                return;
            }
            at = self.nodes[i].parent.map(|p| p.index);
        }
        if let Some(old) = self.nodes[node].parent {
            self.nodes[old.index].children.retain(|&c| c != node);
        }
        self.nodes[node].parent = Some(Parent {
            index: parent,
            kind: EdgeKind::Link,
        });
        let links = &self.notes[&self.nodes[parent].path].links;
        let rank = |i: usize| {
            links
                .iter()
                .position(|l| *l == self.nodes[i].path)
                .unwrap_or(usize::MAX)
        };
        let children = &self.nodes[parent].children;
        let slot = children
            .iter()
            .position(|&c| rank(c) > rank(node))
            .unwrap_or(children.len());
        self.nodes[parent].children.insert(slot, node);
    }

    /// Depths from the root down, after re-parenting moved subtrees.
    fn assign_depths(&mut self) {
        let mut stack = vec![(0usize, 0usize)];
        while let Some((at, depth)) = stack.pop() {
            self.nodes[at].depth = depth;
            stack.extend(self.nodes[at].children.iter().map(|&c| (c, depth + 1)));
        }
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

impl Graph {
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn node(&self, index: usize) -> &Node {
        &self.nodes[index]
    }

    /// Node indices from the root to the current document.
    pub fn route(&self) -> &[usize] {
        &self.route
    }

    /// The node of the document being read: the end of the route.
    pub fn current(&self) -> usize {
        *self.route.last().expect("a graph has at least its root")
    }
}

/// `path` as the reader should see it: relative to `base` when inside it.
pub fn display_path(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn p(s: &str) -> PathBuf {
        PathBuf::from(format!("/v/{s}.md"))
    }

    /// A reader over a fixed link table: `("a", &["b", "c"])` means `a.md`
    /// links to `b.md` then `c.md`.
    pub(super) fn table(links: &[(&str, &[&str])]) -> impl FnMut(&Path) -> Note + use<> {
        let map: HashMap<PathBuf, Note> = links
            .iter()
            .map(|(from, to)| {
                (
                    p(from),
                    Note {
                        title: None,
                        links: to.iter().map(|t| p(t)).collect(),
                    },
                )
            })
            .collect();
        move |path| map.get(path).cloned().unwrap_or_default()
    }

    fn titles(g: &Graph, of: &[usize]) -> Vec<String> {
        of.iter().map(|&i| g.node(i).title.clone()).collect()
    }

    fn parent_of(g: &Graph, name: &str) -> Option<(String, EdgeKind)> {
        let node = g.nodes().iter().find(|n| n.title == name)?;
        node.parent.map(|p| (g.node(p.index).title.clone(), p.kind))
    }

    #[test]
    fn a_note_hangs_under_the_first_note_that_reaches_it() {
        let read = table(&[("a", &["b", "c"]), ("b", &["c", "d"]), ("c", &["a", "d"])]);
        let g = build(&[p("a")], NODE_BUDGET, read).unwrap();
        assert_eq!(titles(&g, &g.node(0).children), ["b", "c"]);
        assert_eq!(parent_of(&g, "d"), Some(("b".into(), EdgeKind::Link)));
        assert_eq!(g.nodes().len(), 4, "a cycle back to the root adds nothing");
        let c = g.nodes().iter().position(|n| n.title == "c").unwrap();
        assert_eq!(
            titles(&g, &g.node(c).targets),
            ["a", "d"],
            "cross-links are kept"
        );
        assert_eq!(g.node(0).targets.len(), 2);
    }

    #[test]
    fn a_followed_link_pins_the_note_a_layer_deeper() {
        // a links to c directly, but the reader went a → b → c.
        let read = table(&[("a", &["b", "c"]), ("b", &["c"])]);
        let g = build(&[p("a"), p("b"), p("c")], NODE_BUDGET, read).unwrap();
        assert_eq!(parent_of(&g, "c"), Some(("b".into(), EdgeKind::Link)));
        assert_eq!(titles(&g, g.route()), ["a", "b", "c"]);
        assert_eq!(g.node(g.current()).title, "c");
    }

    #[test]
    fn a_pin_never_makes_a_note_its_own_ancestor() {
        // x → b → a, and a links back to b. The reader jumped x ⇢ a (from the
        // graph), then followed a → b: pinning b under a would need a under b.
        let read = table(&[("x", &["b"]), ("b", &["a"]), ("a", &["b"])]);
        let g = build(&[p("x"), p("a"), p("b")], NODE_BUDGET, read).unwrap();
        assert_eq!(parent_of(&g, "b"), Some(("x".into(), EdgeKind::Link)));
        assert_eq!(parent_of(&g, "a"), Some(("b".into(), EdgeKind::Link)));
    }

    #[test]
    fn a_moved_subtree_keeps_its_depths_and_link_order() {
        // a → [b, c], b → [d, c], c → [e]. The reader went a → b → c.
        let read = table(&[("a", &["b", "c"]), ("b", &["d", "c"]), ("c", &["e"])]);
        let g = build(&[p("a"), p("b"), p("c")], NODE_BUDGET, read).unwrap();
        let b = g.nodes().iter().position(|n| n.title == "b").unwrap();
        assert_eq!(titles(&g, &g.node(b).children), ["d", "c"]);
        let e = g.nodes().iter().find(|n| n.title == "e").unwrap();
        assert_eq!(e.depth, 3);
    }

    #[test]
    fn a_jump_no_link_explains_leaves_the_tree_alone() {
        // The reader jumped from c to b (say, from the graph); c does not link b.
        let read = table(&[("a", &["b", "c"])]);
        let g = build(&[p("a"), p("c"), p("b")], NODE_BUDGET, read).unwrap();
        assert_eq!(parent_of(&g, "b"), Some(("a".into(), EdgeKind::Link)));
    }

    #[test]
    fn an_unreachable_route_note_hangs_under_where_the_reader_came_from() {
        let read = table(&[("a", &["b"])]);
        let g = build(&[p("a"), p("b"), p("z")], NODE_BUDGET, read).unwrap();
        assert_eq!(parent_of(&g, "z"), Some(("b".into(), EdgeKind::Jump)));
        assert_eq!(g.node(g.current()).depth, 2);
    }

    #[test]
    fn a_revisit_cuts_the_route_back() {
        let trail = [p("a"), p("b"), p("c"), p("b")];
        assert_eq!(loop_free(&trail), [p("a"), p("b")]);
    }

    #[test]
    fn the_budget_cuts_links_but_never_the_route() {
        let read = table(&[("a", &["b", "c", "d", "e"])]);
        let g = build(&[p("a"), p("e")], 3, read).unwrap();
        assert_eq!(titles(&g, &g.node(0).children), ["b", "c", "e"]);
        assert_eq!(g.node(0).hidden, 1);
    }

    #[test]
    fn scan_reads_markdown_links_relative_to_the_note() {
        let md = "# Guide\n\n[a](./sub/a.md) [b](../b.markdown#part) [web](https://x.org/c.md) \
                  [pic](img.png) [self](#top) [dup](sub/a.md) [dir](sub/)\n";
        let note = scan(
            md,
            Path::new("/v/docs/guide.md"),
            &VaultIndex::build(PathBuf::from("/v"), Vec::new()),
        );
        assert_eq!(note.title.as_deref(), Some("Guide"));
        assert_eq!(
            note.links,
            [
                PathBuf::from("/v/docs/./sub/a.md"),
                PathBuf::from("/v/docs/../b.markdown"),
            ],
            "`sub/a.md` is `./sub/a.md` again"
        );
    }

    #[test]
    fn scan_resolves_wikilinks_through_the_vault() {
        let index = VaultIndex::build(
            PathBuf::from("/v"),
            vec![super::super::vault::Entry {
                rel_path: PathBuf::from("notes/Other.md"),
                aliases: Vec::new(),
            }],
        );
        let note = scan(
            "see [[Other]] and [[Missing]]",
            Path::new("/v/a.md"),
            &index,
        );
        assert_eq!(note.links, [PathBuf::from("/v/notes/Other.md")]);
        assert_eq!(note.title, None);
    }

    #[test]
    fn a_frontmatter_title_outranks_the_first_heading() {
        let md = "---\ntitle: \"Case 12\"\n---\n# Heading\n";
        let note = scan(
            md,
            Path::new("/v/a.md"),
            &VaultIndex::build(PathBuf::from("/v"), Vec::new()),
        );
        assert_eq!(note.title.as_deref(), Some("Case 12"));
    }
}
