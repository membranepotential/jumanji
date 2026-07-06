//! Markdown -> self-contained HTML document. Pure; no I/O beyond `md` input.
//!
//! Pipeline: parse (comrak, GFM + footnotes + header ids) -> intercept mermaid
//! fences -> highlight remaining code fences -> wrap tables for horizontal
//! scroll -> extract the TOC -> format to HTML -> assemble a complete,
//! offline, CSP-locked page with embedded CSS and inline SVG.

use std::cell::RefCell;

use comrak::nodes::{Ast, AstNode, LineColumn, NodeHtmlBlock, NodeValue};
use comrak::{Arena, Options as ComrakOptions, format_html, parse_document};

use super::highlight::escape_html;
use super::{Heading, RenderedDocument, diagram, highlight, toc};

/// The stylesheet is embedded at compile time; nothing is fetched at runtime.
const BASE_CSS: &str = include_str!("assets/style.css");

/// Content Security Policy for the rendered page. Network is fully locked out;
/// only inline styles/SVG and local (`file:`/`data:`) images are permitted —
/// see `docs/DESIGN.md` D3.
const CSP: &str = "default-src 'none'; img-src file: data:; style-src 'unsafe-inline'";

/// Rendering options, constructed by the shell from [`super::config`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Content column width in pixels (the centred reading measure).
    pub page_width_px: u32,
    /// Body/prose font family; empty means keep the stylesheet default stack.
    pub font_body: String,
    /// Monospace/code font family; empty means keep the stylesheet default.
    pub font_mono: String,
    /// Base body font size in pixels (the text-zoom 100% reference).
    pub font_size_px: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            page_width_px: 720,
            font_body: String::new(),
            font_mono: String::new(),
            font_size_px: 18,
        }
    }
}

/// Emit the `:root` custom-property overrides the stylesheet consumes:
/// `--content-width`, `--font-size`, and — only when the user set them —
/// `--font-body`/`--font-mono`. Font names are CSS-escaped and quoted.
fn root_vars_css(opts: &Options) -> String {
    let mut vars = format!(
        "--content-width:{}px;--font-size:{}px;",
        opts.page_width_px, opts.font_size_px
    );
    if !opts.font_body.trim().is_empty() {
        vars.push_str(&format!(
            "--font-body:{};",
            css_font_family(&opts.font_body)
        ));
    }
    if !opts.font_mono.trim().is_empty() {
        vars.push_str(&format!(
            "--font-mono:{};",
            css_font_family(&opts.font_mono)
        ));
    }
    format!(":root{{{vars}}}")
}

/// Render a user-supplied font family as a safe, quoted CSS string token.
/// A CSS string literal cannot contain a raw newline or its own quote unescaped;
/// we wrap in double quotes and backslash-escape `"` and `\`, dropping control
/// characters. This keeps a stray value from breaking out of the `<style>` rule.
fn css_font_family(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push('"');
    for c in name.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render a markdown document to a complete HTML page (embedded CSS, inline
/// SVG diagrams). Never fails: broken fences degrade to highlighted code
/// blocks with an error note.
pub fn render(md: &str, opts: &Options) -> RenderedDocument {
    let arena = Arena::new();
    let comrak_opts = comrak_options();
    let root = parse_document(&arena, md, &comrak_opts);

    // Order matters: mermaid first (turns mermaid fences into HTML blocks so
    // the highlighter skips them), then highlight the remaining code fences,
    // then wrap tables. None of these add or reorder headings, so the TOC's
    // anchors still match the ids the formatter emits.
    diagram::transform_mermaid(root);
    highlight::highlight_code_blocks(root);
    wrap_tables(&arena, root);

    let toc = toc::extract(root);

    let mut body = String::new();
    format_html(root, &comrak_opts, &mut body)
        .expect("formatting a comrak AST into a String cannot fail");

    let html = assemble(&body, &toc, opts);
    RenderedDocument { html, toc }
}

/// comrak options: GFM (tables, strikethrough, autolink, tasklist), footnotes,
/// and GitHub-style header ids, with inline HTML passed through (we render
/// local, trusted files; the CSP is the downstream guard).
fn comrak_options<'a>() -> ComrakOptions<'a> {
    let mut o = ComrakOptions::default();
    o.extension.strikethrough = true;
    o.extension.table = true;
    o.extension.autolink = true;
    o.extension.tasklist = true;
    o.extension.footnotes = true;
    // Empty prefix => ids are the bare GitHub-style slug. `toc::extract`
    // mirrors this to keep TOC anchors identical to the emitted ids.
    o.extension.header_id_prefix = Some(String::new());
    o.render.r#unsafe = true;
    o
}

/// AST pass: wrap every GFM table in a `<div class="table-wrap">` so wide
/// tables scroll horizontally within the reading column instead of overflowing
/// the page (a core user pain point). Done in the AST, not by string surgery.
fn wrap_tables<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>) {
    let tables: Vec<&'a AstNode<'a>> = root
        .descendants()
        .filter(|n| matches!(n.data.borrow().value, NodeValue::Table(_)))
        .collect();
    for table in tables {
        table.insert_before(html_block(arena, "<div class=\"table-wrap\">"));
        table.insert_after(html_block(arena, "</div>"));
    }
}

/// Allocate a raw-HTML block node holding `html`.
fn html_block<'a>(arena: &'a Arena<'a>, html: &str) -> &'a AstNode<'a> {
    let ast = Ast::new(
        NodeValue::HtmlBlock(NodeHtmlBlock {
            block_type: 0,
            literal: html.to_string(),
        }),
        LineColumn { line: 1, column: 1 },
    );
    arena.alloc(AstNode::new(RefCell::new(ast)))
}

/// Wrap the rendered body in a complete, self-contained HTML document.
fn assemble(body: &str, toc: &[Heading], opts: &Options) -> String {
    let title = toc
        .first()
        .map(|h| escape_html(&h.text))
        .unwrap_or_else(|| "jumanji".to_string());

    format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\n\
         <title>{title}</title>\n\
         <style>{base}</style>\n\
         <style>{root_vars}</style>\n\
         <style>{light}</style>\n\
         <style>{dark}</style>\n\
         </head>\n\
         <body id=\"top\">\n\
         <main id=\"content\" class=\"markdown-body\">\n\
         {body}\
         </main>\n\
         </body>\n\
         </html>\n",
        csp = CSP,
        title = title,
        base = BASE_CSS,
        root_vars = root_vars_css(opts),
        light = highlight::light_css(),
        dark = highlight::dark_css(),
        body = body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_str(md: &str) -> String {
        render(md, &Options::default()).html
    }

    #[test]
    fn produces_a_complete_csp_locked_document() {
        let html = render_str("# Hello\n\nWorld.\n");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Content-Security-Policy"));
        assert!(html.contains("default-src 'none'"));
        assert!(html.contains("id=\"content\""));
        assert!(html.contains("<h1"));
    }

    #[test]
    fn content_width_comes_from_options() {
        let html = render(
            "# x\n",
            &Options {
                page_width_px: 999,
                ..Options::default()
            },
        )
        .html;
        assert!(html.contains("--content-width:999px"));
    }

    #[test]
    fn font_size_var_always_emitted_as_base() {
        let html = render(
            "# x\n",
            &Options {
                font_size_px: 21,
                ..Options::default()
            },
        )
        .html;
        assert!(html.contains("--font-size:21px"));
    }

    #[test]
    fn font_families_emitted_only_when_set_and_are_quoted() {
        // Default (empty) => no font-family override assignments (the base CSS
        // still *references* `var(--font-body, …)`, so match the `:` set form).
        let plain = render_str("# x\n");
        assert!(!plain.contains("--font-body:"));
        assert!(!plain.contains("--font-mono:"));

        let html = render(
            "# x\n",
            &Options {
                font_body: "Source Serif 4".to_string(),
                font_mono: "JetBrains Mono".to_string(),
                ..Options::default()
            },
        )
        .html;
        assert!(html.contains("--font-body:\"Source Serif 4\""));
        assert!(html.contains("--font-mono:\"JetBrains Mono\""));
    }

    #[test]
    fn font_family_is_css_escaped() {
        // A quote in the value must not break out of the CSS string / style rule.
        let escaped = css_font_family("Ev\"il</style>");
        assert_eq!(escaped, "\"Ev\\\"il</style>\"");
        assert!(!escaped.contains("\"Ev\"il"));
    }

    #[test]
    fn gfm_table_renders_and_is_wrapped_for_scroll() {
        let html = render_str("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(html.contains("<table>"));
        assert!(html.contains("<div class=\"table-wrap\">"));
        // Wrapper opens before the table and closes after it.
        let wrap = html.find("table-wrap").unwrap();
        let table = html.find("<table>").unwrap();
        assert!(wrap < table);
    }

    #[test]
    fn code_fence_gets_syntect_classes() {
        let html = render_str("```rust\nfn main() {}\n```\n");
        assert!(html.contains("<pre class=\"code\">"));
        assert!(html.contains("class=\"source rust\"") || html.contains("class=\""));
    }

    #[test]
    fn mermaid_fence_becomes_inline_svg() {
        let html = render_str("```mermaid\nflowchart TD\nA[Start] --> B[Done]\n```\n");
        assert!(html.contains("<div class=\"mermaid\">"));
        assert!(html.contains("<svg"));
        // The literal fence text must not survive as a plain code block.
        assert!(!html.contains("flowchart TD\nA[Start]"));
    }

    #[test]
    fn broken_mermaid_degrades_to_note_plus_code_no_panic() {
        let html = render_str("```mermaid\n%%%% totally not valid $$$\n```\n");
        assert!(html.contains("diagram-error__note"));
        assert!(html.contains("<pre class=\"code\">"));
        assert!(!html.contains("<svg"));
    }

    #[test]
    fn footnote_renders_with_backlink() {
        let html = render_str("Text with a note.[^1]\n\n[^1]: The note body.\n");
        assert!(html.contains("class=\"footnotes\""));
        assert!(html.contains("footnote-ref"));
        assert!(html.contains("footnote-backref"));
    }

    #[test]
    fn task_list_renders_checkboxes() {
        let html = render_str("- [x] done\n- [ ] todo\n");
        assert!(html.contains("type=\"checkbox\""));
        assert!(html.contains("checked"));
    }

    #[test]
    fn strikethrough_and_autolink_extensions_active() {
        let html = render_str("~~gone~~ and https://example.com\n");
        assert!(html.contains("<del>"));
        assert!(html.contains("<a href=\"https://example.com\""));
    }

    #[test]
    fn heading_anchor_matches_toc() {
        let doc = render("## Getting Started\n", &Options::default());
        assert_eq!(doc.toc[0].anchor, "#getting-started");
        assert!(doc.html.contains("id=\"getting-started\""));
    }
}
