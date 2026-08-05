//! `[[wikilink]]` resolution (DESIGN D11).
//!
//! comrak parses the construct (`wikilinks_title_after_pipe` — Obsidian is
//! url-first) but does no resolution: it emits the raw target as an href, keeps
//! the fragment inside the URL `clean_url`-encoded, and displays
//! `[[Note#Heading]]` as the literal `Note#Heading` (research §5). This pass
//! replaces each `WikiLink` node with a resolved anchor.
//!
//! **Three nodes, not one.** The label is emitted as a real `Text` node between
//! two `HtmlInline`s rather than folded into the opening tag, because
//! `comrak::html::collect_text` — which both `core::toc` and comrak's own
//! heading-id renderer use — skips `HtmlInline`. A heading containing a
//! wikilink would otherwise get a silently truncated anchor, and the TOC, the
//! emitted `id` and Obsidian's own slug would stop agreeing.
//!
//! **An unresolved link carries no `href`.** That makes it non-clickable,
//! non-focusable and invisible to the `f` hint overlay (which selects
//! `a[href]`) — dead by construction rather than by a guard in the router.
//! jumanji is a reader, so a dead link is never a note-creation affordance.

use comrak::Arena;
use comrak::html::collect_text;
use comrak::nodes::{AstNode, NodeValue};

use super::highlight::escape_html;
use super::obsidian::{self, RefKind, percent_decode};
use super::pipeline::html_inline;
use super::vault::{Target, Vault};

/// AST pass: resolve every `[[…]]` against `vault` and rewrite it as an anchor.
pub fn transform_wikilinks<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>, vault: &Vault) {
    let links: Vec<&'a AstNode<'a>> = root
        .descendants()
        .filter(|n| matches!(n.data.borrow().value, NodeValue::WikiLink(_)))
        .collect();

    for node in links {
        let url = match &node.data.borrow().value {
            NodeValue::WikiLink(link) => link.url.clone(),
            _ => continue,
        };
        let raw = percent_decode(&url);
        let reference = obsidian::parse(&raw, RefKind::Link);
        // comrak consumes the pipe itself: `[[Note|alias]]` arrives as
        // `url = "Note"` with the alias as the node's child inlines, and with no
        // pipe the children are just the raw target. So an alias is exactly "the
        // children say something other than the target". comrak keeps the alias
        // as literal text rather than parsing it as inlines, which is what
        // Obsidian does too — no markdown inside link text.
        let written = collect_text(node);
        let label = match written {
            alias if alias != raw && !alias.is_empty() => alias,
            _ => match obsidian::display_label(&reference) {
                label if label.is_empty() => raw.clone(),
                label => label,
            },
        };

        node.insert_before(html_inline(
            arena,
            &open_tag(&vault.resolve(&reference), &raw),
        ));
        node.insert_after(html_inline(arena, "</a>"));
        // The node itself *becomes* the label: comrak's default label is the raw
        // target, which is not what Obsidian displays, and an alias renders as
        // plain text (Obsidian parses no markdown in link text).
        for child in node.children().collect::<Vec<_>>() {
            child.detach();
        }
        node.data.borrow_mut().value = NodeValue::Text(label.into());
    }
}

/// The opening `<a …>` for a resolved (or unresolved) target. `raw` is the
/// decoded target as written, kept for the unresolved form's feedback.
fn open_tag(target: &Target, raw: &str) -> String {
    match target {
        Target::Note { path, anchor } => {
            let fragment = anchor
                .as_deref()
                .map(|a| format!("#{}", obsidian::percent_encode(a)))
                .unwrap_or_default();
            format!(
                "<a class=\"internal-link\" href=\"{}{fragment}\">",
                obsidian::file_uri(path)
            )
        }
        // A non-markdown target is still an ordinary link; the shell hands it
        // to the system opener.
        Target::Asset { path, .. } => format!(
            "<a class=\"internal-link\" href=\"{}\">",
            obsidian::file_uri(path)
        ),
        Target::Unresolved => {
            let raw = escape_html(raw);
            format!(
                "<a class=\"internal-link is-unresolved\" data-href=\"{raw}\" \
                 title=\"unresolved: {raw}\">"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use comrak::{Options, format_html, parse_document};

    use super::*;
    use crate::core::toc;
    use crate::core::vault::testing::TempTree;

    fn options() -> Options<'static> {
        let mut opts = Options::default();
        opts.render.r#unsafe = true;
        opts.extension.wikilinks_title_after_pipe = true;
        opts.extension.header_id_prefix = Some(String::new());
        opts
    }

    /// Parse → transform → format against `vault` (the `fence.rs` test idiom).
    fn transform(md: &str, vault: &Vault) -> String {
        let opts = options();
        let arena = Arena::new();
        let root = parse_document(&arena, md, &opts);
        transform_wikilinks(&arena, root, vault);
        let mut out = String::new();
        format_html(root, &opts, &mut out).unwrap();
        out
    }

    /// A loose vault whose directory holds `Note.md` — enough for the
    /// resolved-link shapes. Resolution itself is `core::vault`'s business.
    fn fixture(name: &str) -> (TempTree, Vault) {
        let tree = TempTree::new(name);
        let source = tree.write("doc.md", "x\n");
        tree.write("Note.md", "# Getting Started\n");
        tree.write("diagram.png", "\u{89}PNG");
        let vault = Vault::rooted(&tree.0, &source);
        (tree, vault)
    }

    #[test]
    fn a_resolved_link_gets_a_file_uri_and_the_internal_link_class() {
        let (_tree, vault) = fixture("wl-resolved");
        let html = transform("See [[Note]].\n", &vault);
        assert!(html.contains("class=\"internal-link\""));
        assert!(html.contains("href=\"file:///"));
        assert!(html.contains("Note.md\">Note</a>"), "{html}");
        assert!(!html.contains("data-wikilink"));
    }

    #[test]
    fn an_asset_target_is_an_ordinary_link() {
        let (_tree, vault) = fixture("wl-asset");
        let html = transform("[[diagram.png]]\n", &vault);
        assert!(html.contains("class=\"internal-link\""));
        assert!(html.contains("diagram.png\">diagram.png</a>"));
    }

    #[test]
    fn an_unresolved_link_carries_no_href() {
        let (_tree, vault) = fixture("wl-unresolved");
        let html = transform("[[Nowhere]]\n", &vault);
        assert!(html.contains("class=\"internal-link is-unresolved\""));
        assert!(html.contains("data-href=\"Nowhere\""));
        assert!(html.contains("title=\"unresolved: Nowhere\""));
        // No href at all: not clickable, not focusable, not hintable.
        assert!(!html.contains("href=\"file:"), "{html}");
        assert!(!html.contains(" href="), "{html}");
        // The label survives, so the reader still sees what was meant.
        assert!(html.contains(">Nowhere</a>"));
    }

    #[test]
    fn an_alias_renders_as_plain_text() {
        let (_tree, vault) = fixture("wl-alias");
        let html = transform("[[Note|the note]]\n", &vault);
        assert!(html.contains(">the note</a>"), "{html}");
        // Obsidian parses no markdown in link text, so neither do we.
        let marked = transform("[[Note|**not bold**]]\n", &vault);
        assert!(!marked.contains("<strong>"), "{marked}");
        assert!(marked.contains(">**not bold**</a>"), "{marked}");
    }

    #[test]
    fn a_heading_fragment_becomes_a_slug_and_a_nicer_label() {
        let (_tree, vault) = fixture("wl-fragment");
        let html = transform("[[Note#Getting Started]]\n", &vault);
        assert!(html.contains("#getting-started\">"), "{html}");
        assert!(html.contains(">Note &gt; Getting Started</a>"), "{html}");
    }

    #[test]
    fn a_block_fragment_keeps_its_caret() {
        let (_tree, vault) = fixture("wl-block");
        // comrak hands us `#%5Eabc123`; the decoded `^` is percent-encoded back
        // into the emitted href, and the shell decodes it before lookup.
        let html = transform("[[Note#^abc123]]\n", &vault);
        assert!(html.contains("#%5Eabc123\">"), "{html}");
    }

    #[test]
    fn a_same_file_reference_targets_the_source_document() {
        let (tree, vault) = fixture("wl-samefile");
        let html = transform("[[#Heading]]\n", &vault);
        let source = tree.0.join("doc.md").canonicalize().unwrap();
        assert!(html.contains(&format!("{}#heading\">", obsidian::file_uri(&source))));
        assert!(html.contains(">Heading</a>"));
    }

    #[test]
    fn a_wikilink_in_a_heading_keeps_the_toc_anchor_in_agreement() {
        // The reason the label is a real `Text` node: `collect_text` skips
        // `HtmlInline`, so folding it into the raw HTML would truncate the
        // anchor of any heading containing a wikilink.
        let (_tree, vault) = fixture("wl-heading");
        let opts = options();
        let arena = Arena::new();
        let root = parse_document(&arena, "## See [[Note|the note]] now\n", &opts);
        transform_wikilinks(&arena, root, &vault);
        let headings = toc::extract(root);
        let mut html = String::new();
        format_html(root, &opts, &mut html).unwrap();

        assert_eq!(headings.len(), 1);
        assert!(headings[0].text.contains("the note"), "{:?}", headings[0]);
        assert_eq!(headings[0].anchor, "#see-the-note-now");
        assert!(html.contains("id=\"see-the-note-now\""), "{html}");
    }

    #[test]
    fn fenced_and_inline_code_never_become_wikilinks() {
        // comrak never makes a `WikiLink` node inside `CodeBlock`/`Code`, so
        // this pass is code-safe structurally, not by a guard. Encoded as
        // documentation — it is the property a future refactor would break.
        let (_tree, vault) = fixture("wl-code");
        let html = transform("```\n[[Note]]\n```\n\nand `[[Note]]` inline\n", &vault);
        assert_eq!(html.matches("internal-link").count(), 0, "{html}");
        assert_eq!(html.matches("[[Note]]").count(), 2);
    }

    #[test]
    fn an_empty_target_is_a_labelless_self_link() {
        // `[[]]` has no note, so it resolves to the source document itself and
        // has nothing to display: a real but empty (and therefore invisible)
        // anchor. Pinned because it is the one shape with no visible output.
        let (tree, vault) = fixture("wl-empty");
        let source = tree.0.join("doc.md").canonicalize().unwrap();
        let html = transform("[[]]\n", &vault);
        assert!(
            html.contains(&format!(
                "<a class=\"internal-link\" href=\"{}\"></a>",
                obsidian::file_uri(&source)
            )),
            "{html}"
        );
        assert!(!html.contains("is-unresolved"), "{html}");
    }
}
