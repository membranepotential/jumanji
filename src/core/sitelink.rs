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
//!
//! **Raw HTML is the second half.** A note's own `<a href>` / `<img src>`, an
//! HTML comment, a diagram's `click` link: none of them is a markdown node, so
//! the AST pass cannot see them. [`scrub`] runs over the finished page body and
//! enforces the invariant for every URL attribute, whoever wrote it: a URL is
//! a web URL, a fragment, or a relative URL to a published file; anything
//! else loses its attribute. HTML comments are removed.

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

/// The attributes whose value is a URL (or, for `srcset`, a list of them).
const URL_ATTRIBUTES: [&str; 10] = [
    "href",
    "src",
    "srcset",
    "xlink:href",
    "poster",
    "data",
    "action",
    "formaction",
    "cite",
    "background",
];

/// A URL [`scrub`] removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    /// The source line of the nearest element before it that carries one
    /// (`data-sourcepos`); 0 when there is none.
    pub line: usize,
    /// The URL as written.
    pub url: String,
    /// Outside the vault (or `file:`), rather than inside it but unpublished.
    pub outside: bool,
}

/// Enforce the site's URL invariant on `html`, the body of the page for the
/// note `source` (absolute) in the vault `root`.
///
/// `find` answers for a vault-relative path: the absolute path of the
/// published file it names (a note's source, an asset, or a page the site
/// writes), or `None`. A URL `find` knows is rewritten to its site URL, which
/// leaves an already-correct one as it was; a web URL or a fragment is kept;
/// every other URL attribute is dropped and reported. HTML comments are
/// removed: rendered text escapes `<`, so every `<!--` here is a real comment.
pub fn scrub(
    html: &str,
    source: &Path,
    root: &Path,
    find: impl Fn(&Path) -> Option<PathBuf>,
) -> (String, Vec<Removed>) {
    let style = LinkStyle::Site {
        root: root.to_path_buf(),
    };
    let mut removed = Vec::new();
    let mut line = 0;
    // `Some(new)` keeps the URL as `new`; `None` drops it.
    let mut site_url = |url: &str, line: usize| -> Option<String> {
        let (outside, fragment, target) = match classify(url, source, root) {
            Url::Web => return Some(url.to_string()),
            Url::Outside => (true, None, None),
            Url::File { rel, fragment } => (false, fragment, find(&rel)),
        };
        match target {
            Some(path) => {
                let fragment = fragment.map(|f| format!("#{f}")).unwrap_or_default();
                Some(format!("{}{fragment}", style.href(&path, source)))
            }
            None => {
                removed.push(Removed {
                    line,
                    url: url.to_string(),
                    outside,
                });
                None
            }
        }
    };

    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = comment.find("-->").map_or("", |end| &comment[end + 3..]);
            continue;
        }
        if !rest[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            out.push('<');
            rest = &rest[1..];
            continue;
        }
        let tag = Tag::parse(rest);
        if let Some(at) = tag.sourcepos_line() {
            line = at;
        }
        out.push_str(&tag.rewrite(|url| site_url(url, line)));
        rest = &rest[tag.len..];
    }
    out.push_str(rest);
    (out, removed)
}

/// One start tag, split into its attributes.
struct Tag<'a> {
    /// `<name`.
    open: &'a str,
    attributes: Vec<Attribute<'a>>,
    /// `>` or `/>`, with any whitespace before it; empty for a tag cut off
    /// by the end of the input.
    close: &'a str,
    /// Bytes of input the tag spans.
    len: usize,
}

struct Attribute<'a> {
    /// The attribute as written, leading whitespace included.
    text: &'a str,
    name: &'a str,
    /// The raw value, without its quotes.
    value: Option<&'a str>,
}

impl<'a> Tag<'a> {
    /// Parse the start tag `html` begins with (`<` and a letter).
    fn parse(html: &'a str) -> Self {
        let bytes = html.as_bytes();
        let stop = |b: u8| b.is_ascii_whitespace() || matches!(b, b'>' | b'/');
        let mut i = 1;
        while i < bytes.len() && !stop(bytes[i]) {
            i += 1;
        }
        let open = &html[..i];
        let mut attributes = Vec::new();
        loop {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] == b'>' || html[i..].starts_with("/>") {
                let end = if i >= bytes.len() {
                    i
                } else if bytes[i] == b'>' {
                    i + 1
                } else {
                    i + 2
                };
                return Self {
                    open,
                    attributes,
                    close: &html[start..end],
                    len: end,
                };
            }
            if bytes[i] == b'/' {
                i += 1;
                continue;
            }
            let name_start = i;
            while i < bytes.len() && !stop(bytes[i]) && bytes[i] != b'=' {
                i += 1;
            }
            let name = &html[name_start..i];
            let mut value = None;
            if i < bytes.len() && bytes[i] == b'=' {
                i += 1;
                match bytes.get(i) {
                    Some(&quote @ (b'"' | b'\'')) => {
                        let end = html[i + 1..]
                            .find(quote as char)
                            .map_or(bytes.len(), |e| i + 1 + e);
                        value = Some(&html[i + 1..end]);
                        i = (end + 1).min(bytes.len());
                    }
                    _ => {
                        let begin = i;
                        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>'
                        {
                            i += 1;
                        }
                        value = Some(&html[begin..i]);
                    }
                }
            }
            attributes.push(Attribute {
                text: &html[start..i],
                name,
                value,
            });
        }
    }

    /// The source line a `data-sourcepos="L:…"` on this tag names.
    fn sourcepos_line(&self) -> Option<usize> {
        self.attributes
            .iter()
            .find(|a| a.name == "data-sourcepos")?
            .value?
            .split(':')
            .next()?
            .parse()
            .ok()
    }

    /// The tag again, each URL attribute passed through `url` (`None` drops
    /// the attribute); everything else exactly as written.
    fn rewrite(&self, mut url: impl FnMut(&str) -> Option<String>) -> String {
        let mut out = self.open.to_string();
        for attribute in &self.attributes {
            let name = attribute.name.to_ascii_lowercase();
            let Some(value) = attribute
                .value
                .filter(|_| URL_ATTRIBUTES.contains(&name.as_str()))
            else {
                out.push_str(attribute.text);
                continue;
            };
            let value = unescape(value);
            let kept = if name == "srcset" {
                let candidates: Vec<String> = value
                    .split(',')
                    .filter_map(|candidate| {
                        let candidate = candidate.trim();
                        let (link, descriptor) = candidate
                            .split_once(char::is_whitespace)
                            .unwrap_or((candidate, ""));
                        url(link).map(|link| format!("{link} {descriptor}").trim_end().to_string())
                    })
                    .collect();
                (!candidates.is_empty()).then(|| candidates.join(", "))
            } else {
                url(&value)
            };
            if let Some(kept) = kept {
                out.push_str(
                    &format!(" {} =\"{}\"", attribute.name, escape_html(&kept))
                        .replacen(" =", "=", 1),
                );
            }
        }
        out.push_str(self.close);
        out
    }
}

/// Undo the character references an attribute value may use.
fn unescape(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let decoded = match &rest[1..end] {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            entity => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .map(|hex| u32::from_str_radix(hex, 16))
                .or_else(|| entity.strip_prefix('#').map(str::parse))
                .and_then(Result::ok)
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
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

    // --- scrub ---------------------------------------------------------------

    /// Scrub `html` in `topic/doc.md`, where `topic/a.md` (page `a.html`) and
    /// `topic/fig.png` are published and nothing else is.
    fn scrubbed(html: &str) -> (String, Vec<Removed>) {
        let published = ["topic/a.md", "topic/a.html", "topic/fig.png"];
        scrub(html, Path::new("/v/topic/doc.md"), Path::new(ROOT), |rel| {
            published
                .iter()
                .any(|p| Path::new(p) == rel)
                .then(|| Path::new(ROOT).join(rel))
        })
    }

    #[test]
    fn scrub_points_raw_html_at_published_files_and_keeps_web_urls() {
        let html = "<a href=\"a.md#x\">A</a> <a class='c' href='a.html'>B</a> \
                    <img src=fig.png alt=\"f\"> <a href=\"https://x.org/?a=1&amp;b=2\">w</a> \
                    <a href=\"#top\">t</a>";
        let (out, removed) = scrubbed(html);
        assert_eq!(
            out,
            "<a href=\"a.html#x\">A</a> <a class='c' href=\"a.html\">B</a> \
             <img src=\"fig.png\" alt=\"f\"> <a href=\"https://x.org/?a=1&amp;b=2\">w</a> \
             <a href=\"#top\">t</a>"
        );
        assert!(removed.is_empty(), "{removed:?}");
    }

    #[test]
    fn scrub_drops_every_other_url_and_reports_it_with_its_line() {
        let html = "<p data-sourcepos=\"7:1-7:9\"><a href=\"draft.md\">d</a>\
                    <img src=\"file:///etc/passwd\"/></p>\
                    <svg><a xlink:href=\"../../secret.md\"><text>x</text></a></svg>";
        let (out, removed) = scrubbed(html);
        assert_eq!(
            out,
            "<p data-sourcepos=\"7:1-7:9\"><a>d</a><img/></p><svg><a><text>x</text></a></svg>"
        );
        let removed: Vec<(usize, &str, bool)> = removed
            .iter()
            .map(|r| (r.line, r.url.as_str(), r.outside))
            .collect();
        assert_eq!(
            removed,
            [
                (7, "draft.md", false),
                (7, "file:///etc/passwd", true),
                (7, "../../secret.md", true),
            ]
        );
    }

    #[test]
    fn scrub_keeps_the_published_candidates_of_a_srcset() {
        let (out, removed) = scrubbed("<img srcset=\"fig.png 1x, private.png 2x\">");
        assert_eq!(out, "<img srcset=\"fig.png 1x\">");
        assert_eq!(removed[0].url, "private.png");
    }

    #[test]
    fn scrub_removes_comments_and_leaves_text_alone() {
        let (out, _) = scrubbed("a <!-- private\nnote --> b < c &lt;!-- not one --&gt;");
        assert_eq!(out, "a  b < c &lt;!-- not one --&gt;");
    }
}
