//! The inline text-run scanner: embeds, comments and block ids (DESIGN D11).
//!
//! Three Obsidian constructs never reach comrak's parser as nodes. `![[…]]`
//! survives as literal text (the `!` consumes the following `[` as an image
//! bracket, so the wikilink branch never fires — research §5); `%%comments%%`
//! and a trailing `^block-id` are not syntax comrak knows at all. All three are
//! therefore found by scanning **text**, and the tricky part — splicing a match
//! back into an inline sequence — is written once here, with three handlers.
//!
//! **Where it scans.** Each block's maximal sequence of consecutive `Text` and
//! `SoftBreak` siblings, with a soft break contributing `"\n"` (which is what
//! comrak renders it as anyway, and what makes a multi-line `%%` comment inside
//! one paragraph work). A hard `LineBreak` ends a run. Fence- and inline-code
//! safety is *structural*, not a guard: `CodeBlock`, `Code` and `HtmlInline`
//! are never `Text` nodes, so a construct written inside them is never seen.
//!
//! **A run with no construct is left untouched** — not rebuilt. Rebuilding
//! would churn the inline `data-sourcepos` of every paragraph in every
//! document for nothing.
//!
//! Honest limits, both inherent to an AST-level treatment: a `%%` block-comment
//! region straddling a list boundary is not handled (the markers are siblings
//! only within one container), and a block id inside a table cell is ignored
//! (Obsidian does not support block refs into tables either).

use std::ops::Range;

use comrak::Arena;
use comrak::html::collect_text;
use comrak::nodes::{AstNode, NodeValue};

use super::highlight::escape_html;
use super::obsidian::{self, RefKind, WikiRef};
use super::pipeline::{html_block, html_inline, text_inline};
use super::vault::{AssetKind, Target, Vault};

/// A construct found in a text run, with its byte range in that run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Construct {
    Embed(WikiRef),
    Comment,
    /// A trailing `^block-id`; carries the id without its `^`.
    BlockId(String),
}

/// What a handler puts back in place of a run.
enum Piece {
    Text(String),
    Html(String),
}

/// Find every construct in `run`, leftmost-first and non-overlapping.
///
/// Pure string in, spans out — the primary test surface for this module, since
/// it is the only new machinery that rewrites arbitrary prose. Unterminated
/// openers match nothing, so a lone `%%` or `![[` in prose survives verbatim.
///
/// `at_block_end` says whether this run reaches the end of its block. A
/// trailing `^block-id` is only a block id there: a run can end mid-paragraph
/// (the next sibling is an emphasis, a link, …), and `Note the marker ^abc
/// *see below* end.` must stay prose.
fn spans(run: &str, at_block_end: bool) -> Vec<(Range<usize>, Construct)> {
    let mut out = Vec::new();
    // The block id is anchored at the end, so it is found first and bounds the
    // leftmost scan — an `^id` cannot sit inside an embed or a comment.
    let tail = at_block_end.then(|| trailing_block_id(run)).flatten();
    let limit = tail.as_ref().map_or(run.len(), |(range, _)| range.start);

    let bytes = run.as_bytes();
    let mut i = 0;
    while i < limit {
        if bytes[i..].starts_with(b"![[")
            && let Some(rel) = run[i + 3..limit].find("]]")
        {
            let target = &run[i + 3..i + 3 + rel];
            let end = i + 3 + rel + 2;
            out.push((
                i..end,
                Construct::Embed(obsidian::parse(target, RefKind::Embed)),
            ));
            i = end;
            continue;
        }
        if bytes[i..].starts_with(b"%%")
            && let Some(rel) = run[i + 2..limit].find("%%")
        {
            let end = i + 2 + rel + 2;
            out.push((i..end, Construct::Comment));
            i = end;
            continue;
        }
        i += 1;
    }
    out.extend(tail.map(|(range, id)| (range, Construct::BlockId(id))));
    out
}

/// A `^id` at the very end of the run, preceded by whitespace. The whitespace
/// is part of the range so removing the id does not leave a dangling space.
fn trailing_block_id(run: &str) -> Option<(Range<usize>, String)> {
    let trimmed = run.trim_end();
    let caret = trimmed.rfind('^')?;
    let id = &trimmed[caret + 1..];
    let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let space = trimmed[..caret].chars().next_back()?;
    (valid && space.is_whitespace())
        .then(|| ((caret - space.len_utf8())..run.len(), id.to_string()))
}

/// One inline text run, as a handler sees it.
struct Run<'a> {
    /// The run's nodes joined into source-shaped text.
    text: String,
    /// The block the run lives in.
    container: &'a AstNode<'a>,
    /// Whether the run's last node is the container's last child — i.e. the run
    /// reaches the end of the block. Only then can a trailing `^id` be a block
    /// identifier.
    at_block_end: bool,
}

/// Rewrite every inline text run under `root` through `handler`. A `None`
/// answer leaves the run's nodes exactly as they were.
fn splice_runs<'a>(
    arena: &'a Arena<'a>,
    root: &'a AstNode<'a>,
    handler: &dyn Fn(&Run<'a>) -> Option<Vec<Piece>>,
) {
    for container in root.descendants().collect::<Vec<_>>() {
        let last_child = container.last_child();
        for run in text_runs(container) {
            let text: String = run
                .iter()
                .map(|node| match &node.data.borrow().value {
                    NodeValue::Text(literal) => literal.to_string(),
                    // A soft break renders as a newline, so the scanner sees
                    // what the source looked like.
                    _ => "\n".to_string(),
                })
                .collect();
            let at_block_end = match (run.last(), last_child) {
                (Some(last), Some(child)) => std::ptr::eq(*last, child),
                _ => false,
            };
            let run_view = Run {
                text,
                container,
                at_block_end,
            };
            let Some(pieces) = handler(&run_view) else {
                continue;
            };
            let first = run[0];
            for piece in pieces {
                let node = match piece {
                    Piece::Text(text) if text.is_empty() => continue,
                    Piece::Text(text) => text_inline(arena, &text),
                    Piece::Html(html) => html_inline(arena, &html),
                };
                first.insert_before(node);
            }
            for node in run {
                node.detach();
            }
        }
    }
}

/// `container`'s children grouped into maximal runs of consecutive `Text` /
/// `SoftBreak` siblings.
fn text_runs<'a>(container: &'a AstNode<'a>) -> Vec<Vec<&'a AstNode<'a>>> {
    let mut runs = Vec::new();
    let mut current: Vec<&'a AstNode<'a>> = Vec::new();
    for child in container.children() {
        let in_run = matches!(
            child.data.borrow().value,
            NodeValue::Text(_) | NodeValue::SoftBreak
        );
        if in_run {
            current.push(child);
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

/// Cut the ranges `keep` says to drop out of `run`, replacing each with what
/// `render` returns (`None` = delete). `None` overall when nothing matched.
fn rewrite(
    run: &str,
    matches: impl Iterator<Item = (Range<usize>, Option<String>)>,
) -> Option<Vec<Piece>> {
    let mut pieces = Vec::new();
    let mut cursor = 0;
    let mut matched = false;
    for (range, html) in matches {
        matched = true;
        pieces.push(Piece::Text(run[cursor..range.start].to_string()));
        if let Some(html) = html {
            pieces.push(Piece::Html(html));
        }
        cursor = range.end;
    }
    matched.then(|| {
        pieces.push(Piece::Text(run[cursor..].to_string()));
        pieces
    })
}

// --- comments --------------------------------------------------------------

/// AST pass: delete `%%comments%%`. Runs **first** in the whole pipeline, so a
/// commented-out fence never reaches a renderer.
pub fn strip_comments<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>) {
    strip_comment_regions(root);
    splice_runs(arena, root, &|run| {
        rewrite(
            &run.text,
            spans(&run.text, run.at_block_end)
                .into_iter()
                .filter(|(_, c)| matches!(c, Construct::Comment))
                .map(|(range, _)| (range, None)),
        )
    });
}

/// The block form: a paragraph that is nothing but `%%` opens a region running
/// to the next such paragraph among the *same* siblings; everything between
/// (markers included) is detached. An unterminated opener is left alone.
fn strip_comment_regions<'a>(root: &'a AstNode<'a>) {
    for container in root.descendants().collect::<Vec<_>>() {
        let children: Vec<&'a AstNode<'a>> = container.children().collect();
        let markers: Vec<usize> = children
            .iter()
            .enumerate()
            .filter(|(_, node)| is_comment_marker(node))
            .map(|(i, _)| i)
            .collect();
        for [open, close] in markers.as_chunks::<2>().0 {
            for node in &children[*open..=*close] {
                node.detach();
            }
        }
    }
}

fn is_comment_marker<'a>(node: &'a AstNode<'a>) -> bool {
    matches!(node.data.borrow().value, NodeValue::Paragraph) && collect_text(node).trim() == "%%"
}

// --- embeds ----------------------------------------------------------------

/// AST pass: resolve `![[…]]` against `vault` and splice in an image or a
/// link-card. Runs before the wikilink pass, because `!` consumes the `[` and
/// the two must not race on the same text.
pub fn transform_embeds<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>, vault: &Vault) {
    splice_runs(arena, root, &|run| {
        rewrite(
            &run.text,
            spans(&run.text, run.at_block_end)
                .into_iter()
                .filter_map(|(range, construct)| match construct {
                    Construct::Embed(reference) => Some((
                        range,
                        Some(embed_html(&vault.resolve(&reference), &reference)),
                    )),
                    _ => None,
                }),
        )
    });
}

/// An image embed becomes an `<img>`; **everything else becomes a link-card**.
/// Note/heading/block transclusion is deferred (it needs recursive rendering
/// with cycle detection), and a card is the honest degradation: a real link, so
/// click, `f` hints and the jumplist all work, and it reads as a door rather
/// than as the content.
fn embed_html(target: &Target, reference: &WikiRef) -> String {
    let label = escape_html(&obsidian::display_label(reference));
    match target {
        Target::Asset {
            path,
            kind: AssetKind::Image,
        } => {
            // `max-width:100%` in the stylesheet caps an oversized `|W`, so a
            // huge declared width scrolls nothing (D5a).
            let size = reference
                .pipe
                .as_deref()
                .and_then(obsidian::parse_dimensions)
                .map(|d| match d.h {
                    Some(h) => format!(" width=\"{}\" height=\"{h}\"", d.w),
                    None => format!(" width=\"{}\"", d.w),
                })
                .unwrap_or_default();
            format!(
                "<img class=\"internal-embed\" src=\"{}\"{size} alt=\"{label}\">",
                obsidian::file_uri(path)
            )
        }
        Target::Asset { path, kind } => card(
            kind.token(),
            Some(&obsidian::file_uri(path)),
            &label,
            reference,
        ),
        Target::Note { path, anchor } => {
            let fragment = anchor
                .as_deref()
                .map(|a| format!("#{}", obsidian::percent_encode(a)))
                .unwrap_or_default();
            card(
                "note",
                Some(&format!("{}{fragment}", obsidian::file_uri(path))),
                &label,
                reference,
            )
        }
        Target::Unresolved => card("note", None, &label, reference),
    }
}

/// The link-card. Without an `href` it is inert and unhintable, exactly like an
/// unresolved wikilink.
fn card(kind: &str, href: Option<&str>, label: &str, reference: &WikiRef) -> String {
    match href {
        Some(href) => format!(
            "<a class=\"internal-embed embed-card\" data-embed=\"{kind}\" href=\"{href}\">{label}</a>"
        ),
        None => {
            let raw = escape_html(reference.note.as_deref().unwrap_or_default());
            format!(
                "<a class=\"internal-embed embed-card is-unresolved\" data-embed=\"{kind}\" \
                 title=\"unresolved: {raw}\">{label}</a>"
            )
        }
    }
}

// --- block ids -------------------------------------------------------------

/// AST pass: strip a trailing `^block-id` from display and leave an anchor
/// before its block, so `[[Note#^37066d]]` has somewhere to land. Runs after
/// the wikilink pass, so an `^id` inside a link label is not mistaken for one.
///
/// The anchor is a **synthetic** (line 0) HTML block, so
/// `annotate_html_block_lines` skips it and the monotonic-source-line invariant
/// (D7) is untouched.
pub fn extract_block_ids<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>) {
    attach_standalone_ids(arena, root);
    splice_runs(arena, root, &|run| {
        // Only a paragraph or a heading owns a block id; a table cell cannot
        // host a block-level anchor, and Obsidian does not support the form.
        if !matches!(
            run.container.data.borrow().value,
            NodeValue::Paragraph | NodeValue::Heading(_)
        ) {
            return None;
        }
        let (range, id) =
            spans(&run.text, run.at_block_end)
                .into_iter()
                .find_map(|(range, construct)| match construct {
                    Construct::BlockId(id) => Some((range, id)),
                    _ => None,
                })?;
        run.container
            .insert_before(html_block(arena, &block_anchor(&id)));
        rewrite(&run.text, std::iter::once((range, None)))
    });
}

/// A paragraph that is nothing but `^id` belongs to the block above it: the
/// anchor goes before *that* block, and the paragraph disappears.
fn attach_standalone_ids<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>) {
    for node in root.descendants().collect::<Vec<_>>() {
        if !matches!(node.data.borrow().value, NodeValue::Paragraph) {
            continue;
        }
        let text = collect_text(node);
        let Some(id) = text.trim().strip_prefix('^') else {
            continue;
        };
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            continue;
        }
        let owner = node.previous_sibling().unwrap_or(node);
        owner.insert_before(html_block(arena, &block_anchor(id)));
        node.detach();
    }
}

fn block_anchor(id: &str) -> String {
    format!(
        "<span class=\"block-anchor\" id=\"^{}\"></span>",
        escape_html(id)
    )
}

#[cfg(test)]
mod tests {
    use comrak::{Options, format_html, parse_document};

    use super::*;
    use crate::core::vault::testing::TempTree;

    // --- `spans`: the pure scanner, tested hardest ------------------------

    /// `spans` for a run that reaches the end of its block (the common case).
    fn kinds(run: &str) -> Vec<(Range<usize>, &'static str)> {
        kinds_at(run, true)
    }

    fn kinds_at(run: &str, at_block_end: bool) -> Vec<(Range<usize>, &'static str)> {
        spans(run, at_block_end)
            .into_iter()
            .map(|(range, construct)| {
                let kind = match construct {
                    Construct::Embed(_) => "embed",
                    Construct::Comment => "comment",
                    Construct::BlockId(_) => "blockid",
                };
                (range, kind)
            })
            .collect()
    }

    #[test]
    fn finds_a_bare_embed() {
        assert_eq!(kinds("![[img.png]]"), vec![(0..12, "embed")]);
    }

    #[test]
    fn embed_target_is_parsed() {
        let (_, construct) = spans("![[img.png|100x145]]", true).pop().unwrap();
        match construct {
            Construct::Embed(r) => {
                assert_eq!(r.note.as_deref(), Some("img.png"));
                assert_eq!(r.pipe.as_deref(), Some("100x145"));
                assert_eq!(r.kind, RefKind::Embed);
            }
            other => panic!("expected an embed, got {other:?}"),
        }
    }

    #[test]
    fn finds_note_heading_and_block_embeds() {
        assert_eq!(kinds("![[Note#Heading]]"), vec![(0..17, "embed")]);
        assert_eq!(kinds("![[Note#^id]]"), vec![(0..13, "embed")]);
    }

    #[test]
    fn finds_several_embeds_with_text_around_them() {
        assert_eq!(
            kinds("a ![[one.png]] b ![[two.png]] c"),
            vec![(2..14, "embed"), (17..29, "embed")]
        );
    }

    #[test]
    fn finds_an_inline_comment_mid_sentence() {
        assert_eq!(kinds("before %%hidden%% after"), vec![(7..17, "comment")]);
    }

    #[test]
    fn a_comment_spans_soft_breaks() {
        // The run joins soft breaks as `\n`, which is what makes the common
        // single-paragraph multi-line comment work.
        assert_eq!(kinds("a %%\nhidden\n%% b"), vec![(2..14, "comment")]);
    }

    #[test]
    fn unterminated_openers_match_nothing() {
        assert!(kinds("![[img.png").is_empty());
        assert!(kinds("50%% off, honest").is_empty());
        assert!(kinds("a lone % sign").is_empty());
        assert!(kinds("a lone ^ caret").is_empty());
    }

    #[test]
    fn a_block_id_matches_only_at_the_very_end() {
        assert_eq!(kinds("A happier place. ^37066d"), vec![(16..24, "blockid")]);
        assert!(kinds("^abc mid sentence here").is_empty());
        assert!(kinds("see ^abc123 in the middle").is_empty());
        // Trailing whitespace after the id is still the end.
        assert_eq!(kinds("text ^abc123  "), vec![(4..14, "blockid")]);
        // No preceding whitespace: not a trailing id (that is the standalone
        // paragraph form, handled at block level).
        assert!(kinds("caret^abc").is_empty());
    }

    #[test]
    fn a_block_id_needs_the_end_of_the_block_not_just_the_run() {
        // A run can end mid-paragraph when the next sibling is an inline node.
        // `Note the marker ^abc *see below* end.` splits into three runs, and
        // the first one's trailing `^abc` is prose, not a block identifier.
        assert!(kinds_at("Note the marker ^abc ", false).is_empty());
        // An embed after the pseudo-id is still found: the rejected tail must
        // not bound the leftmost scan.
        assert_eq!(
            kinds_at("^abc ![[img.png]] ^def", false),
            vec![(5..17, "embed")]
        );
    }

    #[test]
    fn a_block_id_and_an_embed_coexist() {
        assert_eq!(
            kinds("![[a.png]] tail ^b15695"),
            vec![(0..10, "embed"), (15..23, "blockid")]
        );
    }

    // --- the passes -------------------------------------------------------

    fn options() -> Options<'static> {
        let mut opts = Options::default();
        opts.render.r#unsafe = true;
        opts.render.sourcepos = true;
        opts
    }

    fn fixture(name: &str) -> (TempTree, Vault) {
        let tree = TempTree::new(name);
        let source = tree.write("doc.md", "x\n");
        tree.write("diagram.png", "\u{89}PNG");
        tree.write("Note.md", "# H\n");
        tree.write("paper.pdf", "%PDF");
        let vault = Vault::rooted(&tree.0, &source);
        (tree, vault)
    }

    fn embeds(md: &str, vault: &Vault) -> String {
        let opts = options();
        let arena = Arena::new();
        let root = parse_document(&arena, md, &opts);
        transform_embeds(&arena, root, vault);
        let mut out = String::new();
        format_html(root, &opts, &mut out).unwrap();
        out
    }

    fn comments(md: &str) -> String {
        let opts = options();
        let arena = Arena::new();
        let root = parse_document(&arena, md, &opts);
        strip_comments(&arena, root);
        let mut out = String::new();
        format_html(root, &opts, &mut out).unwrap();
        out
    }

    fn block_ids(md: &str) -> String {
        let opts = options();
        let arena = Arena::new();
        let root = parse_document(&arena, md, &opts);
        extract_block_ids(&arena, root);
        let mut out = String::new();
        format_html(root, &opts, &mut out).unwrap();
        out
    }

    #[test]
    fn constructs_inside_code_survive_verbatim() {
        // `CodeBlock` / `Code` are never `Text` nodes, so this is structural.
        // Asserted anyway: it is the property a future refactor would break.
        let (_tree, vault) = fixture("ts-code");
        let fenced = "```\n![[img.png]] %%c%% ^abc123\n```\n";
        assert!(embeds(fenced, &vault).contains("![[img.png]]"));
        assert!(comments(fenced).contains("%%c%%"));
        assert!(block_ids(fenced).contains("^abc123"));

        let inline = "text `![[img.png]]` and `%%c%%` here\n";
        assert!(embeds(inline, &vault).contains("![[img.png]]"));
        assert!(comments(inline).contains("%%c%%"));
    }

    #[test]
    fn an_image_embed_becomes_an_img_with_its_dimensions() {
        let (_tree, vault) = fixture("ts-img");
        let html = embeds("![[diagram.png|320x200]]\n", &vault);
        assert!(html.contains("<img class=\"internal-embed\""), "{html}");
        assert!(html.contains("src=\"file:///"));
        assert!(html.contains("width=\"320\" height=\"200\""));

        let width_only = embeds("![[diagram.png|320]]\n", &vault);
        assert!(width_only.contains("width=\"320\""));
        assert!(!width_only.contains("height="));
    }

    #[test]
    fn a_note_embed_degrades_to_a_link_card() {
        let (_tree, vault) = fixture("ts-note");
        let html = embeds("![[Note]]\n", &vault);
        assert!(html.contains("class=\"internal-embed embed-card\""));
        assert!(html.contains("data-embed=\"note\""));
        assert!(html.contains(">Note</a>"));
    }

    #[test]
    fn a_pdf_embed_is_a_card_and_its_page_param_is_dropped() {
        let (_tree, vault) = fixture("ts-pdf");
        let html = embeds("![[paper.pdf#page=3]]\n", &vault);
        assert!(html.contains("data-embed=\"pdf\""), "{html}");
        assert!(!html.contains("#page=3"), "{html}");
        assert!(!html.contains("%23page"), "{html}");
    }

    #[test]
    fn an_unresolved_embed_is_an_inert_card() {
        let (_tree, vault) = fixture("ts-missing");
        let html = embeds("![[Nowhere]]\n", &vault);
        assert!(html.contains("is-unresolved"));
        assert!(html.contains("title=\"unresolved: Nowhere\""));
        assert!(!html.contains(" href="), "{html}");
    }

    #[test]
    fn comments_are_deleted_inline_and_as_a_block_region() {
        let inline = comments("before %%hidden%% after\n");
        assert!(!inline.contains("hidden"));
        assert!(inline.contains("before"));
        assert!(inline.contains("after"));

        let region = comments("keep\n\n%%\n\ngone entirely\n\n%%\n\nkeep too\n");
        assert!(!region.contains("gone entirely"), "{region}");
        assert!(region.contains("keep"));
        assert!(region.contains("keep too"));
    }

    #[test]
    fn an_unterminated_comment_region_is_left_alone() {
        let html = comments("keep\n\n%%\n\nstill visible\n");
        assert!(html.contains("still visible"), "{html}");
    }

    #[test]
    fn a_trailing_block_id_becomes_an_anchor_before_its_block() {
        let html = block_ids("A happier place. ^37066d\n");
        assert!(html.contains("id=\"^37066d\""), "{html}");
        assert!(html.contains("class=\"block-anchor\""));
        assert!(!html.contains("^37066d<"), "the id must not still display");
        assert!(html.contains("A happier place."));
        // The anchor sits before the paragraph it names.
        let anchor = html.find("block-anchor").unwrap();
        let para = html.find("A happier place").unwrap();
        assert!(anchor < para);
    }

    #[test]
    fn a_mid_paragraph_caret_word_is_left_as_prose() {
        // Regression: the run ends where the emphasis begins, so anchoring the
        // rule to the *run* fired here — emitting a bogus anchor and eating the
        // whitespace on both sides ("Note the markersee below end").
        let html = block_ids("Note the marker ^abc *see below* end.\n");
        assert!(!html.contains("block-anchor"), "{html}");
        assert!(html.contains("Note the marker ^abc "), "{html}");
        assert!(html.contains("end."), "{html}");
    }

    #[test]
    fn a_block_id_after_an_earlier_inline_node_still_matches() {
        // The mirror case: an inline node earlier in the paragraph, with the
        // `^id` genuinely last. The final run *does* reach the block end.
        let html = block_ids("Some *emphasis* and a marker. ^abc123\n");
        assert!(html.contains("id=\"^abc123\""), "{html}");
        assert!(html.contains("<em"), "{html}");
        assert!(!html.contains("^abc123<"), "the id must not still display");
        assert!(html.contains("and a marker."), "{html}");
    }

    #[test]
    fn a_standalone_id_paragraph_attaches_to_the_block_above() {
        let html = block_ids("> A quotation.\n\n^quote-of-the-day\n");
        assert!(html.contains("id=\"^quote-of-the-day\""), "{html}");
        assert!(!html.contains("<p>^quote"), "{html}");
        let anchor = html.find("block-anchor").unwrap();
        let quote = html.find("<blockquote").unwrap();
        assert!(anchor < quote, "{html}");
    }

    #[test]
    fn a_construct_free_paragraph_keeps_its_original_inline_sourcepos() {
        // The guard against needless rebuilds: an untouched run must keep the
        // nodes (and therefore the `data-sourcepos`) comrak produced.
        let (_tree, vault) = fixture("ts-untouched");
        let md = "Plain *emphasised* prose with no constructs.\n";
        let opts = options();
        let arena = Arena::new();
        let baseline = parse_document(&arena, md, &opts);
        let mut before = String::new();
        format_html(baseline, &opts, &mut before).unwrap();

        assert_eq!(embeds(md, &vault), before);
        assert_eq!(comments(md), before);
        assert_eq!(block_ids(md), before);
    }
}
