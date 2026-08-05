//! Markdown -> self-contained HTML document. Pure; no I/O beyond `md` input.
//!
//! Pipeline: parse (comrak, GFM + Obsidian dialect + footnotes + header ids) ->
//! strip comments -> callouts -> intercept mermaid fences -> highlight
//! remaining code fences -> embeds/wikilinks/block ids -> task markers -> wrap
//! tables for horizontal scroll -> extract the TOC -> format to HTML ->
//! assemble a complete, offline, CSP-locked page with embedded CSS and inline
//! SVG. The ordering rationale lives on [`render`] itself.

use std::cell::RefCell;
use std::collections::BTreeMap;

use comrak::nodes::{Ast, AstNode, LineColumn, NodeHtmlBlock, NodeValue};
use comrak::{Arena, Options as ComrakOptions, format_html, parse_document};

use super::highlight::escape_html;
use super::vault::Vault;
use super::{
    Heading, RenderedDocument, callout, diagram, fence, highlight, math, textscan, toc, wikilink,
};

/// The stylesheet is embedded at compile time; nothing is fetched at runtime.
const BASE_CSS: &str = include_str!("assets/style.css");

/// Content Security Policy for the rendered page. Network is fully locked out;
/// only inline styles/SVG, local (`file:`/`data:`) images, and the base64
/// `data:` math fonts are permitted — see `docs/DESIGN.md` D3/D8. `font-src
/// data:` is harmless when a document has no math (nothing references a font).
const CSP: &str =
    "default-src 'none'; img-src file: data:; style-src 'unsafe-inline'; font-src data:";

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
    /// User CSS theme sources, each emitted verbatim in its own `<style>`
    /// block *after* the built-in and generated CSS so user rules win the
    /// cascade. The shell populates this from `~/.config/jumanji/themes/*.css`
    /// (sorted by filename); the core just concatenates in the given order.
    pub extra_css: Vec<String>,
    /// External fence renderers (DESIGN D6.2): fence language token → shell
    /// command. A fence whose language has an entry here is replaced by the
    /// command's stdout (run via `sh -c`, fence body on stdin). Keys are
    /// lowercase; empty = the built-in pipeline only. See `super::fence`.
    pub renderers: BTreeMap<String, String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            page_width_px: 960,
            font_body: String::new(),
            font_mono: String::new(),
            font_size_px: 18,
            extra_css: Vec::new(),
            renderers: BTreeMap::new(),
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
///
/// `vault` is per-document state (it binds the source path), not config-derived
/// render options — the Obsidian passes resolve `[[…]]` / `![[…]]` through it
/// (DESIGN D11).
pub fn render(md: &str, opts: &Options, vault: &Vault) -> RenderedDocument {
    let arena = Arena::new();
    let comrak_opts = comrak_options();
    let root = parse_document(&arena, md, &comrak_opts);

    // Order matters, and every edge below is load-bearing (DESIGN D3/D6.2/D11):
    //
    // - Comments go first, so a commented-out fence never runs a renderer.
    // - Callouts before the fence passes: a callout's body may hold a fence,
    //   and unwrapping the blockquote must not move an already-rendered block.
    // - External fence renderers before built-in mermaid: a fence whose
    //   language has a configured command is consumed here (turned into an HTML
    //   block), so a configured `mermaid` renderer overrides the built-in pass,
    //   and the highlighter skips it too. Built-in mermaid then handles any
    //   remaining mermaid fences, and the highlighter the rest.
    // - Math touches only inline `Math` nodes (disjoint from code blocks and
    //   tables), so its position among the block passes is free.
    // - Embeds before wikilinks: `!` consumes the `[`, so `![[x]]` survives as
    //   literal text while `[[x]]` is a node — the two must not race on the
    //   same text.
    // - Block ids after wikilinks, so an `^id` inside a link label is not
    //   mistaken for a trailing block id.
    // - Tables are wrapped last; none of these add or reorder headings, so the
    //   TOC's anchors still match the emitted ids.
    textscan::strip_comments(&arena, root);
    callout::transform_callouts(&arena, root);
    fence::transform_fences(root, &opts.renderers, &fence::run_command);
    diagram::transform_mermaid(root);
    highlight::highlight_code_blocks(root);
    let has_math = math::transform_math(root);
    textscan::transform_embeds(&arena, root, vault);
    wikilink::transform_wikilinks(&arena, root, vault);
    textscan::extract_block_ids(&arena, root);
    mark_task_symbols(&arena, root);
    wrap_tables(&arena, root);

    // Editor sync (DESIGN D7): the code-block-replacing passes above turn their
    // nodes into raw `HtmlBlock`s, which comrak emits verbatim *without* the
    // `data-sourcepos` attribute its `render.sourcepos` option adds to native
    // blocks — yet the node still carries the original source position. Inject it
    // so mermaid/fence/highlighted blocks stay addressable for forward/reverse
    // search, uniform with comrak's own `data-sourcepos`.
    annotate_html_block_lines(root);

    let toc = toc::extract(root);

    let mut body = String::new();
    format_html(root, &comrak_opts, &mut body)
        .expect("formatting a comrak AST into a String cannot fail");

    let html = assemble(&body, &toc, opts, has_math);
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
    // LaTeX math: `$inline$` / `$$display$$` (dollar math) and `` $`code`$ ``
    // (code math) become inline `NodeValue::Math` nodes; `core::math` renders
    // them to MathML. GitHub's dollar rules apply (see `math.rs` tests).
    o.extension.math_dollars = true;
    o.extension.math_code = true;
    // GitHub-style alerts are **off**: `core::callout` subsumes them (DESIGN
    // D11). comrak recognises 5 of Obsidian's 27 callout spellings and folds
    // the `+`/`-` marker into the title, so one pass covering the whole dialect
    // replaces it rather than running alongside it.
    o.extension.alerts = false;
    // Obsidian dialect (DESIGN D11) — four constructs that are flags, not code:
    // `==highlight==` → `<mark>`, inline footnotes `^[…]` folded into the
    // existing footnote numbering, `[[wikilinks]]` (url-first, which is
    // Obsidian's order — `core::wikilink` does the resolution), and YAML
    // frontmatter recognised so properties stop rendering as a setext heading.
    o.extension.highlight = true;
    o.extension.inline_footnotes = true;
    o.extension.wikilinks_title_after_pipe = true;
    o.extension.front_matter_delimiter = Some("---".to_string());
    // `- [-]` / `- [?]` parse as task items instead of literal text;
    // `core::pipeline::mark_task_symbols` then renders the character rather
    // than a (wrong) checked box.
    o.parse.relaxed_tasklist_matching = true;
    // Editor sync (DESIGN D7): emit `data-sourcepos="startLine:col-endLine:col"`
    // on every rendered element, so the shell can map a source line to the
    // nearest element (forward) and a clicked element back to its source line
    // (reverse). `core::pipeline::annotate_html_block_lines` extends this to the
    // raw-HTML blocks the code-fence passes inject (which comrak does not tag).
    o.render.sourcepos = true;
    // Empty prefix => ids are the bare GitHub-style slug. `toc::extract`
    // mirrors this to keep TOC anchors identical to the emitted ids.
    o.extension.header_id_prefix = Some(String::new());
    o.render.r#unsafe = true;
    o
}

/// AST pass: render a non-`x` task marker as the character Obsidian shows.
///
/// `parse.relaxed_tasklist_matching` makes comrak *parse* `- [-]` / `- [?]`,
/// but `render_task_item` emits `checked` for any non-empty symbol — so they
/// would render as done, which is exactly wrong. Clearing the symbol is the
/// honest state (not done); the character is put back as a small badge in front
/// of the item's first paragraph (DESIGN D11 amendment).
fn mark_task_symbols<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>) {
    for node in root.descendants().collect::<Vec<_>>() {
        let symbol = {
            let mut data = node.data.borrow_mut();
            match &mut data.value {
                NodeValue::TaskItem(item) if matches!(item.symbol, Some(c) if !matches!(c, 'x' | 'X')) => {
                    item.symbol.take()
                }
                _ => continue,
            }
        };
        let Some(symbol) = symbol else { continue };
        let Some(first) = node.first_child() else {
            continue;
        };
        let badge = html_inline(
            arena,
            &format!(
                "<span class=\"task-marker\">{}</span>",
                escape_html(&symbol.to_string())
            ),
        );
        match first.first_child() {
            Some(inline) => inline.insert_before(badge),
            None => first.append(badge),
        }
    }
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

/// Allocate a *synthetic* raw-HTML block node holding `html`: line 0, which
/// [`annotate_html_block_lines`] skips, so a wrapper never inherits a bogus
/// source line (the table-wrap scroll containers, and every closing tag).
pub(super) fn html_block<'a>(arena: &'a Arena<'a>, html: &str) -> &'a AstNode<'a> {
    html_block_at(arena, html, 0)
}

/// Allocate a raw-HTML block node holding `html` and standing in for source
/// `line`. A wrapper that *replaces* a real block (a callout's opening tag)
/// carries that block's line so `annotate_html_block_lines` gives it a
/// `data-sourcepos` and D7 reverse click still lands on the right source.
pub(super) fn html_block_at<'a>(arena: &'a Arena<'a>, html: &str, line: usize) -> &'a AstNode<'a> {
    let ast = Ast::new(
        NodeValue::HtmlBlock(NodeHtmlBlock {
            block_type: 0,
            literal: html.to_string(),
        }),
        LineColumn { line, column: 0 },
    );
    arena.alloc(AstNode::new(RefCell::new(ast)))
}

/// Allocate a raw-HTML *inline* node holding `html`. Inline nodes carry no
/// `data-sourcepos` of their own here (line 0): reverse click walks up to the
/// enclosing block, whose position is untouched — D11's inline-passes rule.
pub(super) fn html_inline<'a>(arena: &'a Arena<'a>, html: &str) -> &'a AstNode<'a> {
    let ast = Ast::new(
        NodeValue::HtmlInline(html.to_string()),
        LineColumn { line: 0, column: 0 },
    );
    arena.alloc(AstNode::new(RefCell::new(ast)))
}

/// Allocate a plain `Text` inline node holding `text`.
pub(super) fn text_inline<'a>(arena: &'a Arena<'a>, text: &str) -> &'a AstNode<'a> {
    let ast = Ast::new(
        NodeValue::Text(text.to_string().into()),
        LineColumn { line: 0, column: 0 },
    );
    arena.alloc(AstNode::new(RefCell::new(ast)))
}

/// AST pass (DESIGN D7): give every raw-`HtmlBlock` node that stands in for a
/// real source block a `data-sourcepos` attribute, matching what comrak's
/// `render.sourcepos` emits for native blocks. The code-fence passes
/// (mermaid/fence/highlight) replace a `CodeBlock` with an `HtmlBlock` but only
/// swap `.value`, leaving `.sourcepos` intact — so the node still knows its
/// origin line. Nodes with line 0 (the synthetic table-wrap wrappers) are
/// skipped.
fn annotate_html_block_lines<'a>(root: &'a AstNode<'a>) {
    for node in root.descendants() {
        let line = {
            let data = node.data.borrow();
            match &data.value {
                NodeValue::HtmlBlock(_) => data.sourcepos.start.line,
                _ => continue,
            }
        };
        if line == 0 {
            continue;
        }
        let mut data = node.data.borrow_mut();
        if let NodeValue::HtmlBlock(block) = &mut data.value {
            block.literal = inject_sourcepos(&block.literal, line);
        }
    }
}

/// Insert a `data-sourcepos="line:1-line:1"` attribute into the first opening
/// tag of `html`, so a raw-HTML wrapper carries its source line the same way
/// comrak's native `data-sourcepos` does. The wrapper always begins with a
/// single opening tag whose first `>` closes it (our wrappers never place a `>`
/// inside an attribute value), so inserting just before that `>` is unambiguous.
/// If there is no `>`, the fragment is returned unchanged.
fn inject_sourcepos(html: &str, line: usize) -> String {
    match html.find('>') {
        Some(i) => {
            let attr = format!(" data-sourcepos=\"{line}:1-{line}:1\"");
            let mut out = String::with_capacity(html.len() + attr.len());
            out.push_str(&html[..i]);
            out.push_str(&attr);
            out.push_str(&html[i..]);
            out
        }
        None => html.to_string(),
    }
}

/// Wrap the rendered body in a complete, self-contained HTML document. The math
/// stylesheet (pulldown-latex's CSS + base64 `data:` fonts) is included only
/// when `has_math`, so math-free documents carry none of its ~0.7 MB weight.
fn assemble(body: &str, toc: &[Heading], opts: &Options, has_math: bool) -> String {
    let title = toc
        .first()
        .map(|h| escape_html(&h.text))
        .unwrap_or_else(|| "jumanji".to_string());

    // The math stylesheet lives in its own `<style>` block: it opens with an
    // `@namespace m` rule (scoped to that sheet) and only loads fonts when the
    // document actually references them.
    let math_css = if has_math {
        format!("<style>{}</style>\n", math::math_css())
    } else {
        String::new()
    };

    // User themes come last so their rules win the cascade over the built-in
    // and generated styles. Each source keeps its own `<style>` block, so one
    // malformed theme cannot break the parsing of another.
    let user_css: String = opts
        .extra_css
        .iter()
        .map(|css| format!("<style>{css}</style>\n"))
        .collect();

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
         {math_css}\
         {user_css}\
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
        math_css = math_css,
        user_css = user_css,
        body = body,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A vault rooted at a directory that does not exist: the scan finds
    /// nothing, so every `[[…]]` is unresolved — which is exactly what a
    /// pipeline-*shape* test wants, and it keeps these tests from reaching into
    /// whatever directory `cargo test` happens to run in. Resolution itself is
    /// `core::vault`'s business and is tested there.
    fn test_vault() -> Vault {
        let root = Path::new("/jumanji-pipeline-tests-have-no-vault");
        Vault::rooted(root, &root.join("test.md"))
    }

    fn render_str(md: &str) -> String {
        render(md, &Options::default(), &test_vault()).html
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
            &test_vault(),
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
            &test_vault(),
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
            &test_vault(),
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
        // With `render.sourcepos` on, the table opens `<table data-sourcepos=…>`.
        assert!(html.contains("<table"));
        assert!(html.contains("<div class=\"table-wrap\">"));
        // Wrapper opens before the table and closes after it.
        let wrap = html.find("table-wrap").unwrap();
        let table = html.find("<table").unwrap();
        assert!(wrap < table);
    }

    #[test]
    fn code_fence_gets_syntect_classes() {
        let html = render_str("```rust\nfn main() {}\n```\n");
        // The highlighted block carries an injected `data-sourcepos` (D7), so the
        // opening tag is `<pre class="code" data-sourcepos=…>`.
        assert!(html.contains("<pre class=\"code\""));
        assert!(html.contains("class=\"source rust\"") || html.contains("class=\""));
    }

    #[test]
    fn mermaid_fence_becomes_inline_svg() {
        let html = render_str("```mermaid\nflowchart TD\nA[Start] --> B[Done]\n```\n");
        // The wrapper carries the intrinsic width on `--dw` (intrinsic-size model).
        assert!(html.contains("<div class=\"mermaid\" style=\"--dw:"));
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

    /// Build render options carrying the given fence renderer table.
    fn render_with_renderers(md: &str, pairs: &[(&str, &str)]) -> String {
        let renderers = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        render(
            md,
            &Options {
                renderers,
                ..Options::default()
            },
            &test_vault(),
        )
        .html
    }

    #[test]
    fn configured_fence_is_replaced_by_command_output() {
        // `cat` echoes the fence body; the output lands in a `.rendered-fence`
        // scroll box, and the fence is not left as a highlighted code block.
        let html = render_with_renderers("```d2\n<svg>hi</svg>\n```\n", &[("d2", "cat")]);
        // The wrapper carries an injected `data-sourcepos` (D7).
        assert!(html.contains("<div class=\"rendered-fence\""));
        assert!(html.contains("<svg>hi</svg>"));
    }

    #[test]
    fn unconfigured_fence_is_highlighted_as_usual() {
        // Only `d2` is configured; a `rust` fence keeps the syntect path.
        let html = render_with_renderers("```rust\nfn main() {}\n```\n", &[("d2", "cat")]);
        assert!(html.contains("<pre class=\"code\""));
        // (The embedded stylesheet mentions `.rendered-fence`, so match the
        // actual wrapper element, not the bare class name.)
        assert!(!html.contains("<div class=\"rendered-fence\""));
    }

    #[test]
    fn failing_renderer_degrades_to_code_plus_note() {
        let html = render_with_renderers("```d2\nx -> y\n```\n", &[("d2", "false")]);
        assert!(html.contains("diagram-error__note"));
        assert!(html.contains("d2 renderer failed"));
        assert!(html.contains("<pre class=\"code\">"));
        assert!(!html.contains("<div class=\"rendered-fence\">"));
    }

    #[test]
    fn garbage_output_degrades_gracefully() {
        // Non-UTF-8 stdout is treated as a render failure, not embedded.
        let html = render_with_renderers("```d2\nx\n```\n", &[("d2", "printf '\\377'")]);
        assert!(html.contains("diagram-error__note"));
        assert!(html.contains("<pre class=\"code\">"));
        assert!(!html.contains("<div class=\"rendered-fence\">"));
    }

    #[test]
    fn configured_mermaid_renderer_overrides_builtin() {
        // A configured `mermaid` renderer wins over the built-in merman path:
        // the output is the command's stdout, not an inline `<svg>` diagram.
        let html = render_with_renderers(
            "```mermaid\nflowchart TD\nA --> B\n```\n",
            &[("mermaid", "printf 'OVERRIDDEN'")],
        );
        assert!(html.contains("<div class=\"rendered-fence\""));
        assert!(html.contains(">OVERRIDDEN</div>"));
        assert!(!html.contains("<svg"));
    }

    #[test]
    fn unconfigured_mermaid_keeps_builtin_path() {
        // With no `mermaid` renderer, the built-in merman pass still runs.
        let html =
            render_with_renderers("```mermaid\nflowchart TD\nA --> B\n```\n", &[("d2", "cat")]);
        assert!(html.contains("<div class=\"mermaid\""));
        assert!(html.contains("<svg"));
    }

    #[test]
    fn inline_math_renders_mathml_and_pulls_in_math_css() {
        let html = render_str("The identity $e^{i\\pi} + 1 = 0$ is famous.\n");
        assert!(html.contains("<math"));
        assert!(html.contains("display=\"inline\""));
        // The math stylesheet (with base64 fonts) is included when math is present.
        assert!(html.contains("data:font/woff2;base64,"));
        // font-src is permitted so the data: fonts load under the CSP.
        assert!(html.contains("font-src data:"));
    }

    #[test]
    fn display_math_renders_block_mathml() {
        let html = render_str("$$\\sum_{n=1}^{\\infty} \\frac{1}{n^2} = \\frac{\\pi^2}{6}$$\n");
        assert!(html.contains("<math"));
        assert!(html.contains("display=\"block\""));
    }

    #[test]
    fn math_css_absent_when_document_has_no_math() {
        // A math-free document must not carry the ~0.7 MB math stylesheet/fonts,
        // and keeps exactly the four built-in <style> blocks.
        let html = render_str("# No math here\n\nJust prose.\n");
        assert!(!html.contains("data:font/woff2"));
        assert_eq!(html.matches("<style>").count(), 4);
    }

    #[test]
    fn broken_display_math_degrades_with_note_no_panic() {
        // Unbalanced environment panics inside pulldown-latex; the pipeline must
        // still produce a page, degrading to the source + an error note.
        let html = render_str("$$\\begin{pmatrix} a & b$$\n");
        assert!(html.contains("math-error"));
        assert!(html.contains("invalid LaTeX math"));
        // No real MathML element (the stylesheet comment mentions `<math` too).
        assert!(!html.contains("<math display="));
    }

    #[test]
    fn dollars_in_prose_are_not_math() {
        // comrak follows GitHub's dollar-math rules: a `$` opening a span must not
        // be followed by whitespace, and (crucially here) a closing `$` must not
        // be immediately followed by a digit. So "costs $5 and $10" is plain
        // prose, not a `$5 and $`-delimited math span. Encoded as documentation.
        let html = render_str("The book costs $5 and the pen costs $10.\n");
        // Match the real MathML opening tag (`<math display=…`), not the bare
        // `<math` substring — the embedded stylesheet mentions it in a comment.
        assert!(
            !html.contains("<math display="),
            "prose dollars must not become math"
        );
        assert!(html.contains("$5"));
        assert!(html.contains("$10"));
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
        // `render.sourcepos` adds `data-sourcepos` to inline elements too, so the
        // tags open `<del data-sourcepos=…>` / `<a data-sourcepos=… href=…>`.
        assert!(html.contains("<del"));
        assert!(html.contains("href=\"https://example.com\""));
    }

    #[test]
    fn heading_anchor_matches_toc() {
        let doc = render("## Getting Started\n", &Options::default(), &test_vault());
        assert_eq!(doc.toc[0].anchor, "#getting-started");
        assert!(doc.html.contains("id=\"getting-started\""));
    }

    // --- Obsidian dialect: the four comrak flags (DESIGN D11) --------------

    #[test]
    fn highlight_extension_renders_mark() {
        let html = render_str("Some ==highlighted== text.\n");
        assert!(html.contains("<mark"));
        assert!(html.contains("highlighted"));
        assert!(!html.contains("==highlighted=="));
    }

    #[test]
    fn inline_footnote_folds_into_the_footnote_list() {
        let html = render_str("A claim.^[The supporting note.]\n");
        assert!(html.contains("class=\"footnotes\""));
        assert!(html.contains("The supporting note."));
        assert!(!html.contains("^["), "the inline marker must not survive");
    }

    #[test]
    fn relaxed_task_markers_render_the_character_not_a_checked_box() {
        for marker in ["-", "?"] {
            let html = render_str(&format!("- [{marker}] partial\n"));
            assert!(
                html.contains("type=\"checkbox\""),
                "[{marker}] should parse as a task item"
            );
            assert!(!html.contains(&format!("[{marker}] partial")));
            // Not done — comrak would have emitted `checked` for any symbol.
            assert!(
                !html.contains("checked=\"\""),
                "[{marker}] must not be checked"
            );
            assert!(html.contains(&format!("<span class=\"task-marker\">{marker}</span>")));
        }
    }

    #[test]
    fn standard_task_markers_are_left_alone() {
        // (Match the element, not the bare class name — the embedded
        // stylesheet mentions `.task-marker` too.)
        let done = render_str("- [x] done\n");
        assert!(done.contains("checked=\"\""));
        assert!(!done.contains("<span class=\"task-marker\">"));

        let todo = render_str("- [ ] todo\n");
        assert!(!todo.contains("checked=\"\""));
        assert!(!todo.contains("<span class=\"task-marker\">"));
    }

    // --- Obsidian dialect: the passes interacting (DESIGN D11 pass order) ---

    #[test]
    fn a_callout_hosts_a_wikilink_an_embed_and_a_fence() {
        let html = render_str(
            "> [!tip]+ Everything\n\
             > See [[Nowhere]] and ![[missing.png]].\n\
             >\n\
             > ```rust\n\
             > fn f() {}\n\
             > ```\n",
        );
        assert!(
            html.contains("<details class=\"callout callout-tip\""),
            "{html}"
        );
        assert!(html.contains("is-unresolved"));
        assert!(html.contains("embed-card"));
        assert!(html.contains("<pre class=\"code\""));
        assert!(html.contains("</details>"));
    }

    #[test]
    fn a_commented_out_mermaid_fence_never_runs_the_renderer() {
        // Proves comments strip *before* any renderer sees the fence.
        let html = render_str("%%\n\n```mermaid\nflowchart TD\nA --> B\n```\n\n%%\n\nAfter.\n");
        assert!(!html.contains("<svg"), "{html}");
        assert!(!html.contains("flowchart"));
        assert!(html.contains("After."));
    }

    #[test]
    fn every_construct_at_once_keeps_source_lines_non_decreasing() {
        let md = "---\ntitle: x\n---\n\n\
                  # Title\n\n\
                  > [!warning]- Folded\n> body with [[Nowhere]]\n\n\
                  Prose with ==mark==, a %%comment%% and an embed ![[missing.png]]. ^abc123\n\n\
                  - [x] done\n- [?] partial\n\n\
                  | a | b |\n|---|---|\n| 1 | 2 |\n\n\
                  ```rust\nfn f() {}\n```\n\n\
                  Final.\n";
        let lines = source_lines(&render_str(md));
        assert!(
            lines.len() > 5,
            "expected many annotated elements: {lines:?}"
        );
        for pair in lines.windows(2) {
            assert!(
                pair[0] <= pair[1],
                "source lines must be non-decreasing in document order: {lines:?}"
            );
        }
    }

    #[test]
    fn a_block_anchor_carries_no_source_line() {
        // Synthetic (line 0), so `annotate_html_block_lines` skips it and the
        // monotonic invariant above is untouched.
        let html = render_str("A happier place. ^37066d\n");
        assert!(
            html.contains("<span class=\"block-anchor\" id=\"^37066d\">"),
            "{html}"
        );
        assert!(!html.contains("block-anchor\" id=\"^37066d\" data-sourcepos"));
    }

    #[test]
    fn front_matter_is_hidden_not_rendered() {
        let html = render_str("---\ntitle: x\naliases: [y]\n---\n\n# Real heading\n");
        assert!(!html.contains("title: x"));
        assert!(!html.contains("<h2"), "no setext heading from the fences");
        assert!(!html.contains("<hr"), "no thematic break from the fences");
        assert!(html.contains("<h1"));
    }

    #[test]
    fn user_css_emitted_verbatim_after_builtin_css() {
        let marker = ".my-user-theme{color:hotpink}";
        let html = render(
            "# x\n",
            &Options {
                extra_css: vec![marker.to_string()],
                ..Options::default()
            },
            &test_vault(),
        )
        .html;
        assert!(html.contains(marker));
        // It must come after the generated dark-theme block so it wins the
        // cascade, and before the body.
        let user = html.find(marker).unwrap();
        let dark_marker = html.find("html.dark").unwrap();
        let body = html.find("<body").unwrap();
        assert!(dark_marker < user, "user CSS must follow generated CSS");
        assert!(user < body, "user CSS must sit in <head>");
    }

    #[test]
    fn multiple_user_css_blocks_keep_order_and_isolation() {
        let html = render(
            "# x\n",
            &Options {
                extra_css: vec!["/*first*/".to_string(), "/*second*/".to_string()],
                ..Options::default()
            },
            &test_vault(),
        )
        .html;
        let first = html.find("/*first*/").unwrap();
        let second = html.find("/*second*/").unwrap();
        assert!(first < second);
        // Each source is wrapped in its own <style> block.
        assert!(html.contains("<style>/*first*/</style>"));
        assert!(html.contains("<style>/*second*/</style>"));
    }

    #[test]
    fn no_user_css_emits_no_extra_style_blocks() {
        // Default options carry no themes; the four built-in blocks are all.
        let html = render_str("# x\n");
        assert_eq!(html.matches("<style>").count(), 4);
    }

    // --- editor sync: source-line mapping (DESIGN D7) ---------------------

    /// Collect the start lines of every `data-sourcepos="L:…"` in document order.
    fn source_lines(html: &str) -> Vec<usize> {
        let pat = "data-sourcepos=\"";
        let mut lines = Vec::new();
        let mut rest = html;
        while let Some(i) = rest.find(pat) {
            rest = &rest[i + pat.len()..];
            let value = &rest[..rest.find('"').unwrap_or(rest.len())];
            let line = value.split(':').next().unwrap_or("");
            if let Ok(n) = line.parse::<usize>() {
                lines.push(n);
            }
        }
        lines
    }

    #[test]
    fn inject_sourcepos_inserts_into_the_first_tag() {
        assert_eq!(
            inject_sourcepos("<pre class=\"code\"><code>x</code></pre>", 12),
            "<pre class=\"code\" data-sourcepos=\"12:1-12:1\"><code>x</code></pre>"
        );
        // A wrapper carrying a style attribute keeps it; the injection lands
        // before the first `>`, after existing attributes.
        assert_eq!(
            inject_sourcepos("<div class=\"mermaid\" style=\"--dw:40px\">s</div>", 3),
            "<div class=\"mermaid\" style=\"--dw:40px\" data-sourcepos=\"3:1-3:1\">s</div>"
        );
    }

    #[test]
    fn blocks_and_wrappers_carry_source_lines() {
        // A paragraph (native comrak sourcepos), a heading, a highlighted code
        // fence and a mermaid fence (injected sourcepos) each carry a line.
        let md = "para one\n\n## Heading\n\n```rust\nfn a() {}\n```\n\n\
                  ```mermaid\nflowchart TD\nA --> B\n```\n";
        let html = render_str(md);
        // Heading is on line 3, the rust fence opens on line 5, the mermaid fence
        // on line 9 — all must appear as data-sourcepos start lines.
        let lines = source_lines(&html);
        assert!(lines.contains(&1), "paragraph line 1 missing: {lines:?}");
        assert!(lines.contains(&3), "heading line 3 missing: {lines:?}");
        assert!(lines.contains(&5), "rust fence line 5 missing: {lines:?}");
        assert!(
            lines.contains(&9),
            "mermaid fence line 9 missing: {lines:?}"
        );
        // The highlighted code block itself carries the line (reverse-click walks
        // up to it), not just some inline descendant.
        assert!(html.contains("<pre class=\"code\" data-sourcepos=\"5:1-5:1\""));
        assert!(
            html.contains("<div class=\"mermaid\"") && html.contains("data-sourcepos=\"9:1-9:1\"")
        );
    }

    #[test]
    fn source_lines_are_monotonically_non_decreasing() {
        // Forward search relies on document-order start lines never going
        // backwards, across prose, nested lists, tables, code and math.
        let md = "# Title\n\nIntro *em* text.\n\n\
                  - a\n- b\n  - nested\n\n\
                  | x | y |\n|---|---|\n| 1 | 2 |\n\n\
                  ```rust\nfn f() {}\n```\n\n\
                  Inline $x^2$ and display:\n\n$$\\sum_{n} n$$\n\n\
                  Final paragraph.\n";
        let lines = source_lines(&render_str(md));
        assert!(
            lines.len() > 5,
            "expected many annotated elements: {lines:?}"
        );
        for pair in lines.windows(2) {
            assert!(
                pair[0] <= pair[1],
                "source lines must be non-decreasing in document order: {lines:?}"
            );
        }
    }

    #[test]
    fn callout_wrapper_carries_the_blockquotes_source_line() {
        // The opening wrapper stands in for a real block, so it must be
        // addressable for D7 reverse click; the closing tag is synthetic.
        let html = render_str("intro\n\n> [!tip] Hint\n> body\n");
        assert!(
            html.contains("<div class=\"callout callout-tip\" data-callout=\"tip\" data-sourcepos=\"3:1-3:1\">"),
            "{html}"
        );
        assert!(!html.contains("</div data-sourcepos"));
    }

    #[test]
    fn table_wrap_scroll_containers_carry_no_source_line() {
        // The synthetic `.table-wrap` divs are line 0 → never annotated; the
        // table itself (a native block) still is.
        let html = render_str("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(!html.contains("<div class=\"table-wrap\" data-sourcepos"));
        assert!(html.contains("<table data-sourcepos="));
    }
}
