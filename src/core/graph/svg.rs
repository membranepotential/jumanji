//! The scene as one inline `<svg>` (DESIGN D14, docs/graph/interaction.md).
//!
//! Everything the overlay needs to draw every zoom level is in the markup;
//! the overlay only swaps classes. Every item is a node, `<g class="jg-node">`,
//! with `data-i` (the item index), `data-key` (its [`ItemKey`](super::ItemKey)),
//! `data-parent`, the panel's text in `data-title` / `data-path` / `data-cut`
//! (links the walk's budget cut), and — when it is folded — what a peek shows
//! in `data-peek` (a JSON array of `{title, file}`, one per hidden child,
//! capped) and `data-peek-more`. Its fold handle is a `<g class="jg-handle">` reading `+n`
//! or `−`. In the tree view a node also carries `data-to`, the items it links
//! to, which the overlay highlights on selection. The `<svg>` carries
//! `data-route` (the spine's items, root first, for the breadcrumb) and the
//! grid's pitch, `data-column` and `data-row`.

use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;

use super::scene::{Handle, Item, Scene, View};
use super::{EdgeKind, Graph, display_path};
use crate::core::highlight::escape_html;

/// Horizontal distance between columns.
const COLUMN: f64 = 300.0;
/// Vertical distance between rows.
const ROW: f64 = 54.0;
/// Pill size.
const NODE_W: f64 = 232.0;
const NODE_H: f64 = 42.0;
/// The fold handle's slot: the pill's last 38 units, behind a hairline.
const HANDLE_W: f64 = 38.0;
/// Margin around the drawing.
const PAD: f64 = 48.0;
/// Longest title / file name written into a pill, in characters; the overlay
/// cuts them to what fits, and the panel shows the whole of both.
const TITLE_CHARS: usize = 27;
const NAME_CHARS: usize = 32;
/// How many hidden children a peek draws before an "and k more" pill.
const PEEK_NODES: usize = 30;

/// A tree edge from a parent's right side `(x1, y1)` to a child's left side
/// `(x2, y2)`: out, along a vertical trunk midway between the columns, and in,
/// with rounded corners. Every child of one parent shares the trunk, so a fan
/// of fifty nodes draws as one line with fifty branches.
fn elbow(x1: f64, y1: f64, x2: f64, y2: f64) -> String {
    let mx = (x1 + x2) / 2.0;
    let dy = y2 - y1;
    if dy.abs() < 0.5 {
        return format!("M{x1:.1} {y1:.1}H{x2:.1}");
    }
    let r = (dy.abs() / 2.0).min(10.0);
    let s = dy.signum();
    format!(
        "M{x1:.1} {y1:.1}H{:.1}Q{mx:.1} {y1:.1} {mx:.1} {:.1}V{:.1}Q{mx:.1} {y2:.1} {:.1} {y2:.1}H{x2:.1}",
        mx - r,
        y1 + s * r,
        y2 - s * r,
        mx + r,
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// One node a peek draws.
#[derive(Serialize)]
struct PeekNode {
    title: String,
    file: String,
}

/// What a peek at a folded item draws: its hidden children as a JSON array of
/// [`PeekNode`], at most [`PEEK_NODES`], escaped for an attribute — and how
/// many more there are.
fn peek(graph: &Graph, hidden: &[usize]) -> (String, usize) {
    let nodes: Vec<PeekNode> = hidden
        .iter()
        .take(PEEK_NODES)
        .map(|&n| {
            let node = graph.node(n);
            PeekNode {
                title: node.title.clone(),
                file: file_name(&node.path),
            }
        })
        .collect();
    let json = serde_json::to_string(&nodes).expect("strings serialize");
    (escape_html(&json), hidden.len().saturating_sub(PEEK_NODES))
}

/// The panel's line for links the walk's budget cut: `""` when none were.
fn cut(hidden: usize) -> String {
    match hidden {
        0 => String::new(),
        1 => "1 more link not walked".to_string(),
        n => format!("{n} more links not walked"),
    }
}

fn joined(items: impl Iterator<Item = usize>) -> String {
    items.map(|i| i.to_string()).collect::<Vec<_>>().join(" ")
}

impl Scene {
    /// The scene as one `<svg>` element; paths display relative to `base`.
    pub fn svg(&self, graph: &Graph, base: &Path) -> String {
        let items = self.items();
        let top = items.iter().map(|it| it.row).fold(0.0, f64::min);
        // An item's pill: its left edge and vertical centre.
        let anchor = |it: &Item| {
            (
                PAD + it.col as f64 * COLUMN,
                PAD + (it.row - top) * ROW + NODE_H / 2.0,
            )
        };
        let bundled: Vec<bool> = {
            let mut b = vec![false; items.len()];
            for bundle in self.bundles() {
                for &m in &bundle.members {
                    b[m] = true;
                }
            }
            b
        };
        let on_route = |n: usize| graph.route().contains(&n);
        // Where an item's outgoing edges start: its pill's right edge.
        let out = |it: &Item| anchor(it).0 + NODE_W;

        let (mut edges, mut spine) = (String::new(), String::new());
        for (i, it) in items.iter().enumerate() {
            let Some(p) = it.parent else { continue };
            let (_, py) = anchor(&items[p]);
            let (x, y) = anchor(it);
            let jump = if it.edge == EdgeKind::Jump {
                " jg-jump"
            } else {
                ""
            };
            if self.is_spine(i) {
                let _ = write!(
                    spine,
                    r#"<path class="jg-route-edge{jump}" data-a="{p}" data-b="{i}" d="M{:.1} {py:.1}H{x:.1}"/>"#,
                    out(&items[p]),
                );
            } else {
                let d = elbow(out(&items[p]), py, x, y);
                let class = if bundled[i] { " bundled" } else { "" };
                let _ = write!(
                    edges,
                    r#"<path class="jg-edge{class}{jump}" data-a="{p}" data-b="{i}" d="{d}"/>"#
                );
            }
        }

        // The label sits in its own translated group: text is counter-scaled
        // about its local origin (graph.css `--sx`), so it must start there.
        let mut bundles = String::new();
        for bundle in self.bundles() {
            let first = &items[bundle.members[0]];
            let last = &items[*bundle.members.last().expect("a bundle has members")];
            let (x, y1) = anchor(first);
            let (_, y2) = anchor(last);
            let (_, py) = anchor(&items[bundle.parent]);
            let (top, bottom) = (y1 - NODE_H / 2.0, y2 + NODE_H / 2.0);
            let mid = (top + bottom) / 2.0;
            let _ = write!(
                bundles,
                r#"<g class="jg-bundle" data-parent="{parent}"><path class="jg-bundle-edge" d="{d}"/><rect x="{x:.1}" y="{top:.1}" width="6" height="{h:.1}" rx="3"/><g transform="translate({tx:.1} {mid:.1})"><text class="t">{n} nodes</text></g></g>"#,
                parent = bundle.parent,
                d = elbow(out(&items[bundle.parent]), py, x, mid),
                h = bottom - top,
                tx = x + 16.0,
                n = bundle.members.len(),
            );
        }

        // In the tree view each node is one item, so its links are items too.
        let item_of: Vec<Option<usize>> = {
            let mut of = vec![None; graph.nodes().len()];
            if self.view() == View::Tree {
                for (i, it) in items.iter().enumerate() {
                    of[it.node] = Some(i);
                }
            }
            of
        };

        let mut nodes = String::new();
        for (i, it) in items.iter().enumerate() {
            let (x, y) = anchor(it);
            let node = graph.node(it.node);
            let mut class = String::from("jg-node");
            if i == 0 {
                class.push_str(" root");
            }
            if on_route(it.node) {
                class.push_str(" route");
            }
            if i == self.current() {
                class.push_str(" current");
            }
            if self.is_spine(i) {
                class.push_str(" spine");
            }
            match it.handle {
                Handle::Folded(_) => class.push_str(" folded"),
                Handle::Unfolded => class.push_str(" unfolded"),
                Handle::None => {}
            }
            if bundled[i] {
                class.push_str(" bundled");
            }
            let (peek_lines, peek_more) = match &it.handle {
                Handle::Folded(hidden) => peek(graph, hidden),
                Handle::None | Handle::Unfolded => (String::new(), 0),
            };
            let _ = write!(
                nodes,
                r#"<g class="{class}" data-i="{i}" data-key="{key}" data-parent="{parent}" data-title="{title_attr}" data-path="{path}" data-cut="{cut}" data-to="{to}" data-peek="{peek_lines}" data-peek-more="{peek_more}" transform="translate({x:.1} {top:.1})"><rect width="{NODE_W}" height="{NODE_H}" rx="8"/><circle class="dot" cy="{half}" r="{r}"/><text class="t" x="16" y="18">{title}</text><text class="f" x="16" y="33">{name}</text>"#,
                key = it.key,
                parent = it.parent.map_or(String::new(), |p| p.to_string()),
                title_attr = escape_html(&node.title),
                path = escape_html(&display_path(&node.path, base)),
                cut = cut(node.hidden),
                to = joined(node.targets.iter().filter_map(|&t| item_of[t])),
                top = y - NODE_H / 2.0,
                half = NODE_H / 2.0,
                r = if i == 0 { 5.5 } else { 3.5 },
                title = escape_html(&truncate(&node.title, TITLE_CHARS)),
                name = escape_html(&truncate(&file_name(&node.path), NAME_CHARS)),
            );
            // The fold handle: `+n` folded, `−` unfolded, none when there is
            // nothing to unfold in this view —
            // the pill's last slot, behind a hairline, and its own click
            // target, so a click on the rest of the pill only ever selects.
            // The label sits in a translated group: text is counter-scaled
            // about its local origin (graph.css `--sx`).
            let label = match &it.handle {
                Handle::Folded(hidden) => Some(format!("+{}", hidden.len())),
                Handle::Unfolded => Some("\u{2212}".to_string()),
                Handle::None => None,
            };
            if let Some(label) = label {
                let _ = write!(
                    nodes,
                    r#"<g class="jg-handle"><rect class="hit" x="{hx}" width="{HANDLE_W}" height="{NODE_H}"/><line class="hair" x1="{hx}" y1="9" x2="{hx}" y2="{hy}"/><g transform="translate({tx} {ty})"><text text-anchor="middle">{label}</text></g></g>"#,
                    hx = NODE_W - HANDLE_W,
                    hy = NODE_H - 9.0,
                    tx = NODE_W - HANDLE_W / 2.0,
                    ty = NODE_H / 2.0,
                );
            }
            nodes.push_str("</g>");
        }

        let cols = items.iter().map(|it| it.col).max().unwrap_or(0);
        let bottom = items.iter().map(|it| it.row).fold(0.0, f64::max);
        let width = 2.0 * PAD + cols as f64 * COLUMN + NODE_W + 40.0;
        let height = 2.0 * PAD + (bottom - top + 1.0) * ROW;
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" class="jg" data-view="{view}" data-route="{route}" data-column="{COLUMN}" data-row="{ROW}" width="{width:.0}" height="{height:.0}"><g class="jg-edges">{edges}</g><g class="jg-route">{spine}</g><g class="jg-bundles">{bundles}</g><g class="jg-nodes">{nodes}</g></svg>"#,
            view = self.view().name(),
            route = joined(self.spine()),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::tests::{p, table};
    use super::super::{Folds, NODE_BUDGET, Scan, build};
    use super::*;

    #[test]
    fn the_svg_marks_root_route_and_current_and_escapes_titles() {
        let mut read = table(&[("a", &["b"])]);
        let g = build(&[p("a"), p("b")], NODE_BUDGET, move |path: &Path| {
            let mut scan: Scan = read(path);
            if path == p("b") {
                scan.title = Some("<Tom & Jerry>".into());
            }
            scan
        })
        .unwrap();
        let svg = Scene::new(&g, View::Links, &Folds::new()).svg(&g, Path::new("/v"));
        assert!(svg.contains(r#"class="jg-node root route spine" data-i="0""#));
        assert!(svg.contains(r#"class="jg-node route current spine" data-i="1""#));
        assert!(svg.contains("&lt;Tom &amp; Jerry&gt;"));
        assert!(svg.contains(r#"data-path="b.md""#));
        assert!(svg.contains(r#"<path class="jg-route-edge" "#));
        assert!(svg.contains(r#"data-route="0 1""#));
        assert!(!svg.contains("You are here"), "no restated facts");
    }

    #[test]
    fn a_handle_reads_plus_n_folded_and_minus_unfolded() {
        let read = table(&[("a", &["b", "c", "d"]), ("b", &["e", "f"])]);
        let g = build(&[p("a")], NODE_BUDGET, read).unwrap();
        let links = Scene::new(&g, View::Links, &Folds::new());
        let svg = links.svg(&g, &PathBuf::from("/v"));
        assert!(svg.contains(">+2</text>"), "b folds e and f");
        assert!(
            svg.contains(">\u{2212}</text>"),
            "the current node is unfolded"
        );
        assert!(svg.contains(r#"class="jg-node folded""#));
        assert!(
            svg.contains(">2 nodes</text>") && !svg.contains(">3 nodes</text>"),
            "c and d bundle; b has links of its own"
        );
        assert!(
            !svg.contains(r#"data-to="1"#),
            "link highlighting is a tree-view thing"
        );

        let tree = Scene::new(&g, View::Tree, &Folds::new());
        let svg = tree.svg(&g, &PathBuf::from("/v"));
        assert!(svg.contains(r#"data-view="tree""#));
        // Items in placement order: a, b (with e, f under it), c, d.
        assert!(svg.contains(r#"data-to="1 4 5""#), "a links b, c, d");
        assert!(
            !svg.contains(">+"),
            "nothing starts folded in the tree view"
        );
    }

    /// The `data-peek` attribute of the first folded item, unescaped and
    /// parsed, with its `data-peek-more`.
    fn peek_of(svg: &str) -> (serde_json::Value, usize) {
        let at = svg.find(" folded\"").expect("a folded item");
        let attr = |name: &str| {
            let key = format!("{name}=\"");
            let from = svg[at..].find(&key).unwrap() + at + key.len();
            let to = svg[from..].find('"').unwrap() + from;
            svg[from..to]
                .replace("&quot;", "\"")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&")
        };
        (
            serde_json::from_str(&attr("data-peek")).expect("data-peek is JSON"),
            attr("data-peek-more").parse().unwrap(),
        )
    }

    #[test]
    fn a_folded_node_carries_what_a_peek_shows_as_json() {
        let many: Vec<String> = (0..40).map(|i| format!("n{i:02}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let read = table(&[("a", &["hub"]), ("hub", many.as_slice())]);
        let g = build(&[p("a")], NODE_BUDGET, read).unwrap();
        let svg = Scene::new(&g, View::Links, &Folds::new()).svg(&g, Path::new("/v"));
        let (nodes, more) = peek_of(&svg);
        assert_eq!(nodes.as_array().unwrap().len(), 30, "capped at 30");
        assert_eq!(more, 10, "the rest counted");
        assert_eq!(nodes[0]["title"], "n00");
        assert_eq!(nodes[0]["file"], "n00.md");
        assert!(
            svg.contains(r#"data-peek="" data-peek-more="0""#),
            "an unfolded one peeks at nothing"
        );
    }

    #[test]
    fn a_peek_survives_tabs_newlines_and_quotes() {
        let mut read = table(&[("a", &["hub"]), ("hub", &[])]);
        let odd = PathBuf::from("/v/we\nird.md");
        let link = odd.clone();
        let g = build(&[p("a")], NODE_BUDGET, move |path: &Path| {
            let mut scan: Scan = read(path);
            if path == p("hub") {
                scan.links = vec![link.clone()];
            }
            if path == link {
                scan.title = Some("tab\there \"quoted\" <b>&".into());
            }
            scan
        })
        .unwrap();
        let svg = Scene::new(&g, View::Links, &Folds::new()).svg(&g, Path::new("/v"));
        let (nodes, _) = peek_of(&svg);
        assert_eq!(nodes[0]["title"], "tab\there \"quoted\" <b>&");
        assert_eq!(nodes[0]["file"], odd.file_name().unwrap().to_str().unwrap());
    }

    #[test]
    fn links_the_budget_cut_are_said_in_the_panel_not_as_a_handle() {
        let read = table(&[("a", &["b", "c", "d", "e"])]);
        let g = build(&[p("a")], 3, read).unwrap();
        for view in [View::Links, View::Tree] {
            let svg = Scene::new(&g, view, &Folds::new()).svg(&g, Path::new("/v"));
            assert!(
                svg.contains(r#"data-cut="2 more links not walked""#),
                "{view:?}"
            );
            assert!(!svg.contains(">+2</text>"));
        }
        assert_eq!(cut(1), "1 more link not walked");
        assert_eq!(cut(0), "");
    }
}
