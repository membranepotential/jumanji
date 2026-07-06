//! Table-of-contents extraction.
//!
//! Walks the AST in document order and produces one [`Heading`] per heading.
//! Anchors are computed with the *same* [`comrak::Anchorizer`] algorithm and
//! the *same* text-collection ([`comrak::html::collect_text`]) that comrak's
//! HTML renderer uses, so the anchors here are byte-for-byte the fragment ids
//! the rendered HTML actually contains — including duplicate-heading suffixes
//! (`-1`, `-2`, …).

use comrak::Anchorizer;
use comrak::html::collect_text;
use comrak::nodes::{AstNode, NodeValue};

use super::Heading;

/// Extract the document outline in reading order. Must be called on the same
/// (post-transform) AST that is later formatted to HTML.
pub fn extract<'a>(root: &'a AstNode<'a>) -> Vec<Heading> {
    let mut anchorizer = Anchorizer::new();
    let mut headings = Vec::new();

    for node in root.descendants() {
        let level = match &node.data.borrow().value {
            NodeValue::Heading(heading) => heading.level,
            _ => continue,
        };
        let text = collect_text(node);
        let slug = anchorizer.anchorize(&text);
        headings.push(Heading {
            level,
            text,
            anchor: format!("#{slug}"),
        });
    }

    headings
}

#[cfg(test)]
mod tests {
    use super::*;
    use comrak::{Arena, Options, parse_document};

    fn toc(md: &str) -> Vec<Heading> {
        let arena = Arena::new();
        let mut opts = Options::default();
        opts.extension.header_id_prefix = Some(String::new());
        let root = parse_document(&arena, md, &opts);
        extract(root)
    }

    #[test]
    fn extracts_levels_text_and_slugs_in_order() {
        let headings = toc("# Title\n\n## Getting Started\n\n### Deep Dive\n");
        assert_eq!(
            headings,
            vec![
                Heading {
                    level: 1,
                    text: "Title".into(),
                    anchor: "#title".into()
                },
                Heading {
                    level: 2,
                    text: "Getting Started".into(),
                    anchor: "#getting-started".into()
                },
                Heading {
                    level: 3,
                    text: "Deep Dive".into(),
                    anchor: "#deep-dive".into()
                },
            ]
        );
    }

    #[test]
    fn duplicate_headings_get_unique_suffixed_anchors() {
        let headings = toc("## Notes\n\n## Notes\n\n## Notes\n");
        let anchors: Vec<_> = headings.iter().map(|h| h.anchor.as_str()).collect();
        assert_eq!(anchors, vec!["#notes", "#notes-1", "#notes-2"]);
    }

    #[test]
    fn punctuation_is_stripped_like_github() {
        let headings = toc("# Ticks aren't in\n");
        assert_eq!(headings[0].anchor, "#ticks-arent-in");
    }

    #[test]
    fn inline_markup_contributes_only_its_text() {
        let headings = toc("# `code` and **bold** words\n");
        assert_eq!(headings[0].text, "code and bold words");
        assert_eq!(headings[0].anchor, "#code-and-bold-words");
    }
}
