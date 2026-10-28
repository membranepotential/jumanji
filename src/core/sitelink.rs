//! Markdown links and images on a published site (DESIGN D15).
//!
//! The reader leaves a relative `[x](a.md)` or `![x](fig.svg)` as written: the
//! webview resolves it against the document's directory and the shell routes
//! the click. A site has no shell, so in [`LinkStyle::Site`] this pass rewrites
//! each one to the published file's URL — `a.md#h` to `a.html#h` — or makes it
//! dead. A link is dead when its target lies outside the vault root, is not
//! published, or is a `file://` URL. Like an unresolved wikilink, a dead link
//! has no `href`, and its URL appears nowhere in the page: the published set is
//! decided by links, so a dead link must not reveal the name of an unpublished
//! file.
//!
//! "Published" is the render-time vault: the publisher builds it from the
//! published notes and assets alone, so the lookup here and the wikilink
//! resolution answer from the same set.
//!
//! [`classify`] is shared with `core::site`, whose link walk decides what is
//! published. Both read a URL the same way, so the walk and the page agree.

use std::path::{Path, PathBuf};

use comrak::Arena;
use comrak::nodes::{AstNode, NodeValue};

use super::graph;
use super::highlight::escape_html;
use super::obsidian::{self, percent_decode};
use super::pipeline::{LinkStyle, html_inline};
use super::vault::Vault;

/// What the URL of a markdown link or image names, for a site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Url {
    /// Not a file: a web link (any scheme but `file:`), a same-page fragment,
    /// or nothing. Left exactly as written.
    Web,
    /// A file in the vault: its path relative to the root, and the fragment
    /// as written (without the `#`).
    File {
        rel: PathBuf,
        fragment: Option<String>,
    },
    /// A file the site can never serve: outside the root, or a `file://` URL.
    Outside,
}

/// Read `url`, written in the document `source`, against the vault `root`.
/// Both paths are absolute. A relative path joins onto the document's
/// directory, with `.` and `..` resolved lexically, as a browser does.
pub fn classify(url: &str, source: &Path, root: &Path) -> Url {
    let (path, fragment) = match url.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment.to_string())),
        None => (url, None),
    };
    let path = path.split('?').next().unwrap_or("");
    if path.is_empty() || obsidian::file_uri_to_path(path).is_some() {
        return if path.is_empty() {
            Url::Web
        } else {
            Url::Outside
        };
    }
    if graph::has_scheme(path) {
        return Url::Web;
    }
    let dir = source.parent().unwrap_or(Path::new("/"));
    let absolute = graph::lexical(&dir.join(percent_decode(path)));
    match absolute.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => Url::Outside,
        Ok(rel) => Url::File {
            rel: rel.to_path_buf(),
            fragment,
        },
        Err(_) => Url::Outside,
    }
}

/// AST pass: point every markdown link and image at its published file, or
/// make it dead. Runs only for a site; the reader keeps links as written.
pub fn transform_links<'a>(
    arena: &'a Arena<'a>,
    root: &'a AstNode<'a>,
    vault: &Vault,
    site: &Path,
) {
    let style = LinkStyle::Site {
        root: site.to_path_buf(),
    };
    let links: Vec<&'a AstNode<'a>> = root
        .descendants()
        .filter(|n| {
            matches!(
                n.data.borrow().value,
                NodeValue::Link(_) | NodeValue::Image(_)
            )
        })
        .collect();
    for node in links {
        // `None` makes the link dead. The borrow ends before the tree changes.
        let (href, image) = {
            let data = node.data.borrow();
            let (NodeValue::Link(link) | NodeValue::Image(link)) = &data.value else {
                continue;
            };
            let href = match classify(&link.url, vault.source(), site) {
                Url::Web => continue,
                Url::Outside => None,
                Url::File { rel, fragment } => vault.index().file(&rel).map(|path| {
                    let fragment = fragment.map(|f| format!("#{f}")).unwrap_or_default();
                    format!("{}{fragment}", style.href(path, vault.source()))
                }),
            };
            (href, matches!(data.value, NodeValue::Image(_)))
        };
        match href {
            Some(href) => {
                if let NodeValue::Link(link) | NodeValue::Image(link) =
                    &mut node.data.borrow_mut().value
                {
                    link.url = href;
                }
            }
            None => make_dead(arena, node, image),
        }
    }
}

/// Turn the link or image `node` into a dead link in place: the unresolved-wikilink markup around the link's own text,
/// so a heading's anchor (which reads the text, not the markup) is unchanged.
/// An image has no visible text, so it becomes an inert link-card naming its
/// alt text, as an unresolved `![[…]]` does.
fn make_dead<'a>(arena: &'a Arena<'a>, node: &'a AstNode<'a>, image: bool) {
    if image {
        let label = escape_html(&node.collect_text());
        for child in node.children().collect::<Vec<_>>() {
            child.detach();
        }
        node.insert_before(html_inline(
            arena,
            &format!(
                "<a class=\"internal-embed embed-card is-unresolved\" data-embed=\"image\">{label}</a>"
            ),
        ));
        node.detach();
        return;
    }
    node.insert_before(html_inline(
        arena,
        "<a class=\"internal-link is-unresolved\">",
    ));
    for child in node.children().collect::<Vec<_>>() {
        node.insert_before(child);
    }
    node.insert_before(html_inline(arena, "</a>"));
    node.detach();
}

#[cfg(test)]
mod tests {
    use comrak::{format_html, parse_document};

    use super::*;
    use crate::core::pipeline::comrak_options;
    use crate::core::vault::{Entry, VaultIndex};

    const ROOT: &str = "/v";

    fn url(url: &str) -> Url {
        classify(url, Path::new("/v/topic/doc.md"), Path::new(ROOT))
    }

    fn file(rel: &str, fragment: Option<&str>) -> Url {
        Url::File {
            rel: PathBuf::from(rel),
            fragment: fragment.map(str::to_string),
        }
    }

    #[test]
    fn a_url_is_read_against_the_documents_directory() {
        assert_eq!(url("a.md"), file("topic/a.md", None));
        assert_eq!(url("a.md#h"), file("topic/a.md", Some("h")));
        assert_eq!(url("../b%20c.md"), file("b c.md", None));
        assert_eq!(url("./fig.svg?v=2"), file("topic/fig.svg", None));
        assert_eq!(url("/v/x.png"), file("x.png", None));
    }

    #[test]
    fn web_links_and_fragments_are_left_alone() {
        assert_eq!(url("https://example.com/a.md"), Url::Web);
        assert_eq!(url("mailto:a@b.c"), Url::Web);
        assert_eq!(url("#section"), Url::Web);
        assert_eq!(url(""), Url::Web);
    }

    #[test]
    fn anything_outside_the_root_and_every_file_uri_is_outside() {
        assert_eq!(url("../../papers/p.pdf"), Url::Outside);
        assert_eq!(url("/etc/passwd"), Url::Outside);
        assert_eq!(url("file:///v/topic/a.md"), Url::Outside);
        assert_eq!(url("../.."), Url::Outside);
        assert_eq!(url(".."), Url::Outside);
    }

    /// Render `md` (written in `topic/doc.md`) against a vault that publishes
    /// exactly `published`.
    fn render(md: &str, published: &[&str]) -> String {
        let entries = published
            .iter()
            .map(|rel| Entry {
                rel_path: PathBuf::from(rel),
                aliases: Vec::new(),
            })
            .collect();
        let vault = Vault::new(
            VaultIndex::build(PathBuf::from(ROOT), entries),
            Path::new("/v/topic/doc.md"),
        );
        let opts = comrak_options();
        let arena = Arena::new();
        let root = parse_document(&arena, md, &opts);
        transform_links(&arena, root, &vault, Path::new(ROOT));
        let mut html = String::new();
        format_html(root, &opts, &mut html).unwrap();
        html
    }

    #[test]
    fn a_published_note_link_points_at_its_page() {
        let html = render(
            "[A](a.md#intro) [B](../My%20B.md)\n",
            &["topic/a.md", "My B.md"],
        );
        assert!(html.contains("href=\"a.html#intro\""), "{html}");
        assert!(html.contains("href=\"../My%20B.html\""), "{html}");
    }

    #[test]
    fn a_published_asset_keeps_its_path() {
        let html = render(
            "![fig](fig%201.svg) [pdf](p.pdf)\n",
            &["topic/fig 1.svg", "topic/p.pdf"],
        );
        assert!(html.contains("src=\"fig%201.svg\""), "{html}");
        assert!(html.contains("href=\"p.pdf\""), "{html}");
    }

    #[test]
    fn unpublished_outside_and_file_uri_targets_are_dead_and_say_nothing() {
        let html = render(
            "[draft](draft.md) [paper](../../papers/p.pdf) [abs](file:///v/topic/a.md) \
             ![gone](gone.png)\n",
            &["topic/a.md"],
        );
        assert_eq!(html.matches("is-unresolved").count(), 4, "{html}");
        assert!(!html.contains("href"), "{html}");
        for leak in ["draft.md", "papers", "file:", "gone.png"] {
            assert!(!html.contains(leak), "{leak} leaked: {html}");
        }
        assert!(html.contains(">draft</a>"), "{html}");
        assert!(html.contains("data-embed=\"image\">gone</a>"), "{html}");
    }

    #[test]
    fn web_links_and_fragments_pass_through_unchanged() {
        let html = render("[w](https://example.com) [s](#top)\n", &[]);
        assert!(html.contains("href=\"https://example.com\""), "{html}");
        assert!(html.contains("href=\"#top\""), "{html}");
    }

    #[test]
    fn a_dead_link_in_a_heading_keeps_the_headings_anchor() {
        let html = render("## See [the draft](draft.md) now\n", &[]);
        assert!(html.contains("id=\"see-the-draft-now\""), "{html}");
    }
}
