//! Obsidian callouts (DESIGN D11).
//!
//! A callout is a blockquote whose first line is `[!type]`, optionally followed
//! by a fold marker (`+`/`-`) and a title. This pass covers all 27 spellings,
//! replacing comrak's `alerts` extension — which knows 5 of them, folds the
//! marker into the title, and leaves `> [!question]` rendering as visible
//! `[!question]` garbage (`docs/research/04-obsidian.md` §3).
//!
//! Emission mirrors Obsidian so themes keying off it work: `data-callout` keeps
//! the literal (lowercased) type, while the class carries the *canonical* one,
//! so the 27 → 13 alias table lives in typed Rust rather than in a CSS selector
//! list. A fold marker emits `<details>`/`<summary>`; **no marker emits a plain
//! `<div>`**, because Obsidian shows no disclosure affordance there and
//! `<details open>` would invent one. Folding therefore costs no JavaScript
//! (D3).
//!
//! The wrapping is the `wrap_tables` shape — raw-HTML sibling blocks around the
//! quote's own children — with the opening tag carrying the blockquote's source
//! line so D7 reverse click still lands. Nesting falls out for free: unwrapping
//! a blockquote re-parents its children, so a nested quote is still in the tree
//! when its turn comes.
//!
//! **Titles are plain text** (DESIGN D11 amendment): the pass drops the marker
//! line, and the title's inline markup goes with it — `> [!tip] **Bold** title`
//! titles as `Bold title`.

use comrak::Arena;
use comrak::html::collect_text_append;
use comrak::nodes::{AstNode, NodeValue};

use super::highlight::escape_html;
use super::pipeline::{html_block, html_block_at};

/// Obsidian's 27 spellings → the 13 canonical types (research §3). An unknown
/// type keeps its literal `data-callout` and falls back to the `note` style,
/// matching Obsidian.
const CANONICAL: &[(&str, &str)] = &[
    ("note", "note"),
    ("abstract", "abstract"),
    ("summary", "abstract"),
    ("tldr", "abstract"),
    ("info", "info"),
    ("todo", "todo"),
    ("tip", "tip"),
    ("hint", "tip"),
    // Obsidian makes `important` an alias of `tip`; GitHub gives it its own
    // colour. A deliberate Obsidian-over-GitHub divergence (research §3).
    ("important", "tip"),
    ("success", "success"),
    ("check", "success"),
    ("done", "success"),
    ("question", "question"),
    ("help", "question"),
    ("faq", "question"),
    ("warning", "warning"),
    ("caution", "warning"),
    ("attention", "warning"),
    ("failure", "failure"),
    ("fail", "failure"),
    ("missing", "failure"),
    ("danger", "danger"),
    ("error", "danger"),
    ("bug", "bug"),
    ("example", "example"),
    ("quote", "quote"),
    ("cite", "quote"),
];

/// The disclosure state a callout's fold marker asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fold {
    /// No marker: a plain box, no disclosure affordance.
    None,
    /// `+` — foldable, open.
    Open,
    /// `-` — foldable, collapsed.
    Closed,
}

/// The `[!type]±Title` header of one callout.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Marker {
    /// The type as written, lowercased.
    literal: String,
    fold: Fold,
    /// The title as written; empty means "use the default".
    title: String,
}

/// AST pass: turn every `[!type]` blockquote into a callout wrapper.
pub fn transform_callouts<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>) {
    // Collect before mutating (the `wrap_tables` idiom): the walk would
    // otherwise trip over nodes being detached under it.
    let quotes: Vec<&'a AstNode<'a>> = root
        .descendants()
        .filter(|n| matches!(n.data.borrow().value, NodeValue::BlockQuote))
        .collect();

    for quote in quotes {
        let Some(marker) = take_marker(quote) else {
            continue;
        };
        let line = quote.data.borrow().sourcepos.start.line;
        let (open, close) = wrappers(&marker);
        quote.insert_before(html_block_at(arena, &open, line));
        let close = html_block(arena, close);
        quote.insert_after(close);
        // Move the quote's children out between the wrappers, in order.
        for child in quote.children().collect::<Vec<_>>() {
            child.detach();
            close.insert_before(child);
        }
        quote.detach();
    }
}

/// Match the callout header on `quote`'s first line and, on a match, remove it
/// from the AST (returning what it said). The paragraph that held it is
/// detached when nothing survives it.
fn take_marker<'a>(quote: &'a AstNode<'a>) -> Option<Marker> {
    let paragraph = quote.first_child()?;
    if !matches!(paragraph.data.borrow().value, NodeValue::Paragraph) {
        return None;
    }

    // The first line is everything up to the first break.
    let first_line: Vec<&'a AstNode<'a>> = paragraph
        .children()
        .take_while(|n| {
            !matches!(
                n.data.borrow().value,
                NodeValue::SoftBreak | NodeValue::LineBreak
            )
        })
        .collect();

    // Matching whole nodes, not a slice of source text, is what makes this
    // robust to comrak splitting `[`, `!note`, `]` across sibling `Text`s.
    let mut text = String::new();
    let mut literal_len = None;
    for node in &first_line {
        let is_text = matches!(node.data.borrow().value, NodeValue::Text(_));
        if !is_text && literal_len.is_none() {
            // Everything from here on is markup, not plain source text.
            literal_len = Some(text.len());
        }
        collect_text_append(node, &mut text);
    }
    // The marker must lie entirely within the *leading run of `Text` nodes*, so
    // a quote opening with `` `[!note]` `` (a `Code` node) is not a callout.
    let marker = parse_marker(&text, literal_len.unwrap_or(text.len()))?;

    for node in first_line {
        node.detach();
    }
    // The break that ended the first line goes too, or the body would open with
    // a stray leading space.
    if let Some(first) = paragraph.first_child()
        && matches!(
            first.data.borrow().value,
            NodeValue::SoftBreak | NodeValue::LineBreak
        )
    {
        first.detach();
    }
    if paragraph.first_child().is_none() {
        paragraph.detach();
    }
    Some(marker)
}

/// Parse `[!type]` + optional fold marker + optional title out of a first line.
/// `literal_len` is how much of `line` came from plain `Text` nodes; the
/// `[!type]` part must fit inside it.
fn parse_marker(line: &str, literal_len: usize) -> Option<Marker> {
    let rest = line.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let literal = &rest[..close];
    let valid = !literal.is_empty()
        && literal.starts_with(|c: char| c.is_ascii_alphabetic())
        && literal
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid || "[!".len() + close + "]".len() > literal_len {
        return None;
    }

    let after = rest[close + 1..].trim_start();
    let (fold, title) = match after.strip_prefix('+') {
        Some(title) => (Fold::Open, title),
        None => match after.strip_prefix('-') {
            Some(title) => (Fold::Closed, title),
            None => (Fold::None, after),
        },
    };
    Some(Marker {
        literal: literal.to_ascii_lowercase(),
        fold,
        title: title.trim().to_string(),
    })
}

/// The opening (element + title) and closing HTML for one callout.
fn wrappers(marker: &Marker) -> (String, &'static str) {
    let canonical = CANONICAL
        .iter()
        .find(|(spelling, _)| *spelling == marker.literal)
        .map(|(_, canonical)| *canonical)
        // Unknown types keep their literal `data-callout` and take the `note`
        // style, matching Obsidian.
        .unwrap_or("note");
    let default_title = title_case(&marker.literal);
    let title = escape_html(if marker.title.is_empty() {
        &default_title
    } else {
        &marker.title
    });
    let class = format!("callout callout-{canonical}");
    // `escape_html` on the title also keeps `inject_sourcepos`'s precondition
    // true — no `>` inside the opening tag's attribute values.
    let data = escape_html(&marker.literal);
    match marker.fold {
        Fold::None => (
            format!(
                "<div class=\"{class}\" data-callout=\"{data}\">\
                 <p class=\"callout-title\">{title}</p>"
            ),
            "</div>",
        ),
        Fold::Open | Fold::Closed => {
            let open = if marker.fold == Fold::Open {
                " open"
            } else {
                ""
            };
            (
                format!(
                    "<details class=\"{class}\" data-callout=\"{data}\"{open}>\
                     <summary class=\"callout-title\">{title}</summary>"
                ),
                "</details>",
            )
        }
    }
}

/// The default title: the type as written, first letter capitalised (`faq` →
/// `Faq`), which is what Obsidian shows (research §3).
fn title_case(literal: &str) -> String {
    let mut chars = literal.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use comrak::{Options, format_html, parse_document};

    use super::*;

    /// Parse → transform → format, mirroring the pipeline's shape without
    /// pulling in the whole pipeline (the `fence.rs` test idiom).
    fn transform(md: &str) -> String {
        let mut opts = Options::default();
        opts.render.r#unsafe = true;
        let arena = Arena::new();
        let root = parse_document(&arena, md, &opts);
        transform_callouts(&arena, root);
        let mut out = String::new();
        format_html(root, &opts, &mut out).unwrap();
        out
    }

    #[test]
    fn every_spelling_maps_to_its_canonical_class() {
        for (spelling, canonical) in CANONICAL {
            let html = transform(&format!("> [!{spelling}]\n> body\n"));
            assert!(
                html.contains(&format!("class=\"callout callout-{canonical}\"")),
                "[!{spelling}] should be callout-{canonical}: {html}"
            );
            // The literal spelling survives for themes keying off it.
            assert!(html.contains(&format!("data-callout=\"{spelling}\"")));
            assert!(html.contains("body"));
        }
        assert_eq!(CANONICAL.len(), 27, "all 27 Obsidian spellings");
    }

    #[test]
    fn type_matching_is_case_insensitive() {
        for spelling in ["TIP", "Tip", "tIp"] {
            let html = transform(&format!("> [!{spelling}]\n> body\n"));
            assert!(html.contains("callout-tip"));
            assert!(html.contains("data-callout=\"tip\""));
        }
    }

    #[test]
    fn custom_title_replaces_the_default() {
        let html = transform("> [!tip] Custom title\n> body\n");
        assert!(html.contains("<p class=\"callout-title\">Custom title</p>"));
        assert!(html.contains("body"));
    }

    #[test]
    fn default_title_is_the_type_in_title_case() {
        assert!(transform("> [!faq]\n> b\n").contains(">Faq</p>"));
        assert!(transform("> [!note]\n> b\n").contains(">Note</p>"));
    }

    #[test]
    fn title_only_callout_is_legal() {
        let html = transform("> [!warning] Careful\n");
        assert!(html.contains("callout-warning"));
        assert!(html.contains(">Careful</p>"));
        // No empty paragraph left where the marker line was.
        assert!(!html.contains("<p></p>"));
    }

    #[test]
    fn no_fold_marker_emits_a_div_never_details() {
        let html = transform("> [!note]\n> body\n");
        assert!(html.contains("<div class=\"callout callout-note\""));
        assert!(!html.contains("<details"));
    }

    #[test]
    fn fold_markers_emit_details_open_or_closed() {
        let expanded = transform("> [!note]+ Expanded\n> body\n");
        assert!(expanded.contains("<details class=\"callout callout-note\""));
        assert!(expanded.contains(" open>"));
        assert!(expanded.contains("<summary class=\"callout-title\">Expanded</summary>"));
        assert!(expanded.contains("</details>"));

        let collapsed = transform("> [!faq]- Collapsed by default\n> body\n");
        assert!(collapsed.contains("<details class=\"callout callout-question\""));
        assert!(!collapsed.contains(" open>"));
        assert!(collapsed.contains(">Collapsed by default</summary>"));
    }

    #[test]
    fn unknown_type_keeps_its_literal_and_falls_back_to_note() {
        let html = transform("> [!whatever] Custom\n> body\n");
        assert!(html.contains("class=\"callout callout-note\""));
        assert!(html.contains("data-callout=\"whatever\""));
    }

    #[test]
    fn nested_callouts_are_both_emitted() {
        let html = transform("> [!note] Outer\n>\n> > [!tip] Inner\n> > body\n");
        assert!(html.contains("callout-note"));
        assert!(html.contains("callout-tip"));
        assert!(html.contains(">Outer</p>"));
        assert!(html.contains(">Inner</p>"));
        // Neither survives as a blockquote.
        assert!(!html.contains("<blockquote"));
    }

    #[test]
    fn a_plain_blockquote_is_untouched() {
        let html = transform("> Just a quotation.\n");
        assert!(html.contains("<blockquote"));
        assert!(!html.contains("callout"));
    }

    #[test]
    fn a_marker_inside_inline_code_is_not_a_callout() {
        // The marker must be plain text; `Code` is opaque to the match.
        let html = transform("> `[!note]` is the callout syntax.\n");
        assert!(html.contains("<blockquote"));
        assert!(!html.contains("class=\"callout"));
        assert!(html.contains("[!note]"));
    }

    #[test]
    fn a_marker_not_at_the_start_is_not_a_callout() {
        let html = transform("> See [!note] below.\n");
        assert!(html.contains("<blockquote"));
        assert!(!html.contains("class=\"callout"));
    }

    #[test]
    fn title_markup_degrades_to_plain_text() {
        // DESIGN D11 amendment: dropping the marker line drops its markup.
        let html = transform("> [!tip] **Bold** title\n> body\n");
        assert!(html.contains(">Bold title</p>"), "{html}");
        assert!(!html.contains("<strong>Bold</strong> title"));
    }

    #[test]
    fn a_title_is_html_escaped() {
        // A `>` in the title must not leak into the wrapper's opening tag —
        // `inject_sourcepos` inserts before the first `>` and relies on it.
        let html = transform("> [!note] a > b & \"c\"\n> body\n");
        assert!(html.contains(">a &gt; b &amp; &quot;c&quot;</p>"), "{html}");
        assert!(html.starts_with("<div class=\"callout callout-note\" data-callout=\"note\">"));
    }

    #[test]
    fn gfm_alert_spellings_still_render_as_callouts() {
        // The coverage the deleted `alerts`-extension tests used to give.
        // Note `IMPORTANT` is now an alias of `tip` — deliberate (research §3).
        for (kw, canonical) in [
            ("NOTE", "note"),
            ("TIP", "tip"),
            ("IMPORTANT", "tip"),
            ("WARNING", "warning"),
            ("CAUTION", "warning"),
        ] {
            let html = transform(&format!("> [!{kw}]\n> Heads up.\n"));
            assert!(
                html.contains(&format!("callout-{canonical}")),
                "[!{kw}] should be callout-{canonical}"
            );
            assert!(html.contains("Heads up."));
        }
    }
}
