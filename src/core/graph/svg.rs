//! The scene as one inline `<svg>` (DESIGN D14).
//!
//! Everything the overlay needs to draw every zoom level is in the markup;
//! the overlay only swaps classes. Items are `<g class="jg-node">` with
//! `data-i` (the item index), `data-key` (its [`ItemKey`](super::ItemKey)),
//! `data-parent`, the panel's text in `data-title` / `data-path`, and — on a
//! collapsed item — the hover preview's titles in `data-members`. In the
//! tree view a note also carries `data-to`, the items it links to, which the
//! overlay highlights on selection. The `<svg>` carries `data-route`, the spine's
//! items root first, for the panel's breadcrumb, and `data-column`, the column
//! pitch the far level cuts labels to.

use std::fmt::Write as _;
use std::path::Path;

use super::scene::{Fold, Item, ItemKind, Scene, View};
use super::{EdgeKind, Graph, display_path};
use crate::core::highlight::escape_html;

/// Horizontal distance between columns.
const COLUMN: f64 = 300.0;
/// Vertical distance between rows.
const ROW: f64 = 54.0;
/// Note pill size.
const NODE_W: f64 = 232.0;
const NODE_H: f64 = 42.0;
/// Cluster pill width.
const CLUSTER_W: f64 = 76.0;
/// Margin around the drawing.
const PAD: f64 = 48.0;
/// Longest title / file name drawn on a note, in characters; the panel shows
/// the whole of both.
const TITLE_CHARS: usize = 27;
const NAME_CHARS: usize = 32;
/// How many titles a collapsed item's hover preview lists before "and k more".
const PREVIEW_TITLES: usize = 15;

fn width(item: &Item) -> f64 {
    match item.kind {
        ItemKind::Note(_) => NODE_W,
        ItemKind::Cluster { .. } => CLUSTER_W,
    }
}

/// A tree edge from a parent's right side `(x1, y1)` to a child's left side
/// `(x2, y2)`: out, along a vertical trunk midway between the columns, and in,
/// with rounded corners. Every child of one parent shares the trunk, so a fan
/// of fifty notes draws as one line with fifty branches.
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

/// The hover preview of a collapsed item: the titles of `nodes`, one per
/// line, at most [`PREVIEW_TITLES`] and then `and k more`. Empty unless the
/// item is collapsed — an expanded one already shows its children.
fn preview(graph: &Graph, fold: Fold, nodes: &[usize]) -> String {
    if !matches!(fold, Fold::Collapsed(_)) {
        return String::new();
    }
    let mut lines: Vec<String> = nodes
        .iter()
        .take(PREVIEW_TITLES)
        .map(|&n| graph.node(n).title.clone())
        .collect();
    if nodes.len() > PREVIEW_TITLES {
        lines.push(format!("and {} more", nodes.len() - PREVIEW_TITLES));
    }
    escape_html(&lines.join("\n"))
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

        let (mut edges, mut spine) = (String::new(), String::new());
        for (i, it) in items.iter().enumerate() {
            let Some(p) = it.parent else { continue };
            let (px, py) = anchor(&items[p]);
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
                    px + width(&items[p]),
                );
            } else {
                let d = elbow(px + width(&items[p]), py, x, y);
                let class = if bundled[i] { " bundled" } else { "" };
                let _ = write!(
                    edges,
                    r#"<path class="jg-edge{class}{jump}" data-a="{p}" data-b="{i}" d="{d}"/>"#
                );
            }
        }

        let mut bundles = String::new();
        for bundle in self.bundles() {
            let first = &items[bundle.members[0]];
            let last = &items[*bundle.members.last().expect("a bundle has members")];
            let (x, y1) = anchor(first);
            let (_, y2) = anchor(last);
            let (px, py) = anchor(&items[bundle.parent]);
            let (top, bottom) = (y1 - NODE_H / 2.0, y2 + NODE_H / 2.0);
            let mid = (top + bottom) / 2.0;
            let _ = write!(
                bundles,
                r#"<g class="jg-bundle" data-parent="{parent}"><path class="jg-bundle-edge" d="{d}"/><rect x="{x:.1}" y="{top:.1}" width="6" height="{h:.1}" rx="3"/><text class="t" x="{tx:.1}" y="{mid:.1}">{n} notes</text></g>"#,
                parent = bundle.parent,
                d = elbow(px + width(&items[bundle.parent]), py, x, mid),
                h = bottom - top,
                tx = x + 16.0,
                n = bundle.members.len(),
            );
        }

        // In the tree view each note is one item, so its links are items too.
        let item_of: Vec<Option<usize>> = {
            let mut of = vec![None; graph.nodes().len()];
            if self.view() == View::Tree {
                for (i, it) in items.iter().enumerate() {
                    if let ItemKind::Note(n) = it.kind {
                        of[n] = Some(i);
                    }
                }
            }
            of
        };

        let mut nodes = String::new();
        for (i, it) in items.iter().enumerate() {
            let (x, y) = anchor(it);
            let mut class = String::from("jg-node");
            if i == 0 {
                class.push_str(" root");
            }
            if matches!(it.kind, ItemKind::Note(n) if on_route(n)) {
                class.push_str(" route");
            }
            if i == self.current() {
                class.push_str(" current");
            }
            if self.is_spine(i) {
                class.push_str(" spine");
            }
            if matches!(it.kind, ItemKind::Cluster { .. }) {
                class.push_str(" cluster");
            }
            match it.fold {
                Fold::Collapsed(_) => class.push_str(" collapsed"),
                Fold::Expanded => class.push_str(" expanded"),
                Fold::Leaf | Fold::Fixed => {}
            }
            if !self.children(i).is_empty() {
                class.push_str(" inner");
            }
            if bundled[i] {
                class.push_str(" bundled");
            }
            let parent = it.parent.map_or(String::new(), |p| p.to_string());
            let w = width(it);
            match &it.kind {
                ItemKind::Note(n) => {
                    let node = graph.node(*n);
                    let name = node
                        .path
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let to = joined(node.targets.iter().filter_map(|&t| item_of[t]));
                    let _ = write!(
                        nodes,
                        r#"<g class="{class}" data-i="{i}" data-key="{key}" data-parent="{parent}" data-title="{title_attr}" data-path="{path}" data-to="{to}" data-members="{members}" transform="translate({x:.1} {top:.1})"><rect width="{w}" height="{NODE_H}" rx="8"/><circle class="dot" cy="{half}" r="{r}"/><text class="t" x="16" y="18">{title}</text><text class="f" x="16" y="33">{name}</text>"#,
                        key = it.key,
                        members = preview(graph, it.fold, &node.targets),
                        title_attr = escape_html(&node.title),
                        path = escape_html(&display_path(&node.path, base)),
                        top = y - NODE_H / 2.0,
                        half = NODE_H / 2.0,
                        r = if i == 0 { 5.5 } else { 3.5 },
                        title = escape_html(&truncate(&node.title, TITLE_CHARS)),
                        name = escape_html(&truncate(&name, NAME_CHARS)),
                    );
                    // What `l` would show, or — in the tree view, where every
                    // placed link is drawn — how many the budget cut.
                    let more = match it.fold {
                        Fold::Collapsed(n) => Some(("jg-badge", n)),
                        _ if self.view() == View::Tree && node.hidden > 0 => {
                            Some(("jg-more", node.hidden))
                        }
                        _ => None,
                    };
                    if let Some((badge, n)) = more {
                        let _ = write!(
                            nodes,
                            r#"<text class="{badge}" x="{bx}" y="{by}">+{n}</text>"#,
                            bx = w + 10.0,
                            by = NODE_H / 2.0 + 4.0,
                        );
                    }
                }
                ItemKind::Cluster { members, .. } => {
                    let n = members.len();
                    let links = if n == 1 { "link" } else { "links" };
                    let _ = write!(
                        nodes,
                        r#"<g class="{class}" data-i="{i}" data-key="{key}" data-parent="{parent}" data-title="{n} more {links}" data-path="" data-members="{preview}" transform="translate({x:.1} {top:.1})"><rect width="{w}" height="{NODE_H}" rx="21"/><text class="t" x="{cx}" y="26" text-anchor="middle">+{n}</text>"#,
                        key = it.key,
                        preview = preview(graph, it.fold, members),
                        top = y - NODE_H / 2.0,
                        cx = w / 2.0,
                    );
                }
            }
            nodes.push_str("</g>");
        }

        let cols = items.iter().map(|it| it.col).max().unwrap_or(0);
        let bottom = items.iter().map(|it| it.row).fold(0.0, f64::max);
        let width = 2.0 * PAD + cols as f64 * COLUMN + NODE_W + 40.0;
        let height = 2.0 * PAD + (bottom - top + 1.0) * ROW;
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" class="jg" data-view="{view}" data-route="{route}" data-column="{COLUMN}" width="{width:.0}" height="{height:.0}"><g class="jg-edges">{edges}</g><g class="jg-route">{spine}</g><g class="jg-bundles">{bundles}</g><g class="jg-nodes">{nodes}</g></svg>"#,
            view = self.view().name(),
            route = joined(self.spine()),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::tests::{p, table};
    use super::super::{Expanded, NODE_BUDGET, Note, build};
    use super::*;

    #[test]
    fn the_svg_marks_root_route_and_current_and_escapes_titles() {
        let mut read = table(&[("a", &["b"])]);
        let g = build(&[p("a"), p("b")], NODE_BUDGET, move |path: &Path| {
            let mut note: Note = read(path);
            if path == p("b") {
                note.title = Some("<Tom & Jerry>".into());
            }
            note
        })
        .unwrap();
        let svg = Scene::new(&g, View::Links, &Expanded::new()).svg(&g, Path::new("/v"));
        assert!(svg.contains(r#"class="jg-node root route spine inner" data-i="0""#));
        assert!(svg.contains(r#"class="jg-node route current spine" data-i="1""#));
        assert!(svg.contains("&lt;Tom &amp; Jerry&gt;"));
        assert!(svg.contains(r#"data-path="b.md""#));
        assert!(svg.contains(r#"<path class="jg-route-edge" "#));
        assert!(svg.contains(r#"data-route="0 1""#));
        assert!(!svg.contains("You are here"), "no restated facts");
    }

    #[test]
    fn collapsed_items_carry_badges_and_bundles_are_emitted() {
        let read = table(&[("a", &["b", "c", "d"]), ("b", &["e", "f"])]);
        let g = build(&[p("a")], NODE_BUDGET, read).unwrap();
        let links = Scene::new(&g, View::Links, &Expanded::new());
        let svg = links.svg(&g, &PathBuf::from("/v"));
        assert!(svg.contains(r#"<text class="jg-badge""#) && svg.contains(">+2</text>"));
        assert!(
            svg.contains(">2 notes</text>") && !svg.contains(">3 notes</text>"),
            "c and d bundle; b has links of its own"
        );
        assert!(
            !svg.contains(r#"data-to="1"#),
            "cross-links are a tree-view thing"
        );

        let tree = Scene::new(&g, View::Tree, &Expanded::new());
        let svg = tree.svg(&g, &PathBuf::from("/v"));
        assert!(svg.contains(r#"data-view="tree""#));
        // Items in placement order: a, b (with e, f under it), c, d.
        assert!(svg.contains(r#"data-to="1 4 5""#), "a links b, c, d");
    }

    #[test]
    fn a_collapsed_item_lists_what_it_holds_for_the_hover_preview() {
        let many: Vec<String> = (0..20).map(|i| format!("n{i:02}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut links: Vec<&str> = vec!["x"];
        links.push("b");
        links.extend(&many);
        let read = table(&[("a", links.as_slice())]);
        let g = build(&[p("a"), p("b")], NODE_BUDGET, read).unwrap();
        let s = Scene::new(&g, View::Links, &Expanded::new());
        let svg = s.svg(&g, Path::new("/v"));
        assert!(svg.contains(r#"data-members="x""#), "the cluster above");
        let below: Vec<String> = many[..15].iter().map(|t| t.to_string()).collect();
        let expected = format!("data-members=\"{}\nand 5 more\"", below.join("\n"));
        assert!(svg.contains(&expected), "capped at 15");

        let mut open = Expanded::new();
        open.insert(s.item(s.children(0)[2]).key.clone());
        let svg = Scene::new(&g, View::Links, &open).svg(&g, Path::new("/v"));
        assert!(
            !svg.contains("and 5 more"),
            "an expanded cluster shows its notes"
        );
    }

    #[test]
    fn a_cluster_reads_as_a_count() {
        let read = table(&[("a", &["x", "b", "y", "z"])]);
        let g = build(&[p("a"), p("b")], NODE_BUDGET, read).unwrap();
        let svg = Scene::new(&g, View::Links, &Expanded::new()).svg(&g, Path::new("/v"));
        assert!(svg.contains(r#"data-key="0.a""#));
        assert!(svg.contains(r#"data-title="2 more links""#));
        assert!(svg.contains(">+2</text>"));
    }
}
