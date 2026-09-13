//! Mermaid fence interception.
//!
//! A comrak AST pass finds ```` ```mermaid ```` fenced code blocks, renders
//! each to an inline SVG with merman (full-fidelity — the output lands in
//! WebKitGTK, which supports `<foreignObject>`), and replaces the code block
//! node with a raw-HTML node. Rendering failures degrade to the fence shown as
//! a highlighted code block plus a small styled error note — never a panic,
//! never blank output.

use comrak::nodes::{AstNode, NodeHtmlBlock, NodeValue};
use merman::svg::{SvgRenderOptions, sanitize_svg_id};
use merman::{OperationControl, RenderError, RenderOutput, RenderRequest, Renderer, SvgRequest};

use super::highlight::{escape_html, highlight_block};

/// True if a fence info string selects the mermaid renderer (first token,
/// case-insensitive), e.g. `mermaid` or `mermaid title="x"`.
pub fn is_mermaid_fence(info: &str) -> bool {
    matches!(
        info.split_whitespace().next(),
        Some(token) if token.eq_ignore_ascii_case("mermaid")
    )
}

/// Outcome of rendering a single mermaid source.
enum Rendered {
    /// Inline SVG (already wrapped in its display container).
    Svg(String),
    /// merman could not render it; carries a human-readable reason.
    Failed(String),
}

/// The message shown when a fence holds no recognisable diagram.
const NO_DIAGRAM: &str = "no mermaid diagram detected in fence";

fn render(renderer: &Renderer, source: &str, diagram_id: &str) -> Rendered {
    let svg = SvgRequest {
        options: SvgRenderOptions {
            diagram_id: Some(sanitize_svg_id(diagram_id)),
            ..SvgRenderOptions::default()
        },
        ..SvgRequest::default()
    };
    let request = RenderRequest::svg(source, OperationControl::default(), svg);
    match renderer.render(request) {
        Ok(RenderOutput::Svg(Some(out))) => Rendered::Svg(wrap_svg(out.svg())),
        Ok(_) => Rendered::Failed(NO_DIAGRAM.to_string()),
        Err(RenderError::NoDiagram) => Rendered::Failed(NO_DIAGRAM.to_string()),
        Err(err) => Rendered::Failed(describe(&err, source)),
    }
}

/// Turn a merman failure into something a reader can act on.
///
/// merman reports a parse failure as a byte offset into the fence body
/// (`Unexpected character at 964`), which is unactionable in a reader — nobody
/// counts bytes. A parse error also carries a structured [`SourceSpan`], so
/// resolve that offset to `line:col` and quote the offending line. Any other
/// failure (a resource limit, a cancelled operation) has no span and falls back
/// to its own message.
///
/// [`SourceSpan`]: merman::SourceSpan
fn describe(err: &RenderError, source: &str) -> String {
    let RenderError::Parse(diagnostic) = err else {
        return err.to_string();
    };
    let message = diagnostic.terminal_safe_message();
    let Some(span) = diagnostic.terminal_diagnostic_details().span else {
        return message;
    };
    match line_col(source, span.start) {
        Some((line, col, text)) => format!("{message} (line {line}, column {col}: {text})"),
        None => message,
    }
}

/// Resolve a byte offset into the 1-based line and column it falls on, plus
/// that line's text (trimmed). `None` if the offset is past the end of the
/// source or does not land on a character boundary.
fn line_col(source: &str, offset: usize) -> Option<(usize, usize, &str)> {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return None;
    }
    let line_start = source[..offset].rfind('\n').map_or(0, |nl| nl + 1);
    let line_end = source[line_start..]
        .find('\n')
        .map_or(source.len(), |nl| line_start + nl);
    let line = source[..offset].matches('\n').count() + 1;
    // Count characters, not bytes: a column is what the author sees.
    let col = source[line_start..offset].chars().count() + 1;
    Some((line, col, source[line_start..line_end].trim_end()))
}

/// Wrap a merman SVG in its `.mermaid` display container, pinning the diagram's
/// intrinsic (natural) width onto the `--dw` custom property so the stylesheet
/// can render it at natural size (overflowing into its own scroll box) rather
/// than shrinking it to fit the column. The intrinsic px width is recoverable
/// from the SVG root's inline `max-width:<N>px` that merman emits (and parses
/// back itself, `merman::render::raster::parse_svg_max_width_px`). If the width
/// can't be parsed, degrade gracefully to a plain wrapper with no `--dw`.
///
/// The declaration is **moved**, not copied: once `--dw` carries the value, the
/// inline one is deleted. An inline declaration outranks every selector, so
/// leaving it would cap the diagram at its intrinsic width and silently undo any
/// stylesheet rule that scales it up — per-diagram zoom (`--dz`, DESIGN D5a.2)
/// in particular. The only other lever over an inline declaration is
/// `!important`, which would also beat the user's own themes, and those are
/// emitted last precisely so they win.
fn wrap_svg(svg: &str) -> String {
    match intrinsic_max_width(svg) {
        Some((px, start, end)) => format!(
            "<div class=\"mermaid\" style=\"--dw:{px}px\">{}{}</div>",
            &svg[..start],
            &svg[end..]
        ),
        None => format!("<div class=\"mermaid\">{svg}</div>"),
    }
}

/// The intrinsic width and the byte range of the whole `max-width:<N>px`
/// declaration that carries it — trailing `;` and following whitespace
/// included, so deleting the range cannot leave a doubled separator behind.
///
/// Only the **root tag** is searched (up to the first `>`). A diagram's labels
/// can be arbitrary HTML inside a `<foreignObject>`, which may carry inline
/// styles of its own; reading the first `max-width:` in the whole document
/// happened to work, but deleting it would not.
fn intrinsic_max_width(svg: &str) -> Option<(f32, usize, usize)> {
    let root_end = svg.find('>')?;
    let start = svg[..root_end].find("max-width:")?;
    let after_prop = start + "max-width:".len();
    let value = &svg[after_prop..root_end];
    let trimmed = value.trim_start();
    let px_end = trimmed.find("px")?;
    let px = trimmed[..px_end].trim().parse::<f32>().ok()?;

    let mut end = after_prop + (value.len() - trimmed.len()) + px_end + "px".len();
    let tail = &svg[end..root_end];
    let after_space = tail.trim_start();
    if let Some(rest) = after_space.strip_prefix(';') {
        end += (tail.len() - after_space.len()) + 1 + (rest.len() - rest.trim_start().len());
    }
    Some((px, start, end))
}

/// Graceful degradation: an error note above the original fence, rendered as a
/// (plain-text) highlighted code block so the source is never lost.
fn degrade(source: &str, reason: &str) -> String {
    format!(
        "<div class=\"diagram-error\">\
           <p class=\"diagram-error__note\">\u{26a0} Mermaid render failed: {}</p>\
           {}\
         </div>",
        escape_html(reason),
        highlight_block("mermaid", source),
    )
}

/// AST pass: replace each mermaid fence with inline SVG (or a degraded error
/// block). Diagram ids are made unique per document so multiple inlined SVGs
/// do not collide on internal marker/id references.
///
/// At most one [`Renderer`] is built per document, on the first mermaid fence,
/// and reused across all the rest: `render` takes `&self` and the renderer holds
/// only immutable engine/parse config (no per-render caching), so reuse is sound
/// — it just skips re-building those registries for every fence. Per-fence state
/// (the diagram id) rides in the [`SvgRequest`] instead, which is where merman
/// 0.8 puts operation-scoped options.
///
/// Lazily, because building the renderer costs ~70k instructions and most
/// documents have no diagram at all: constructing it eagerly put that on every
/// render, which the pipeline benches see as a fixed cost — +4.6 % on
/// `prose_10k`, and proportionally less on larger ones (DESIGN D13).
pub fn transform_mermaid<'a>(root: &'a AstNode<'a>) {
    let mut renderer: Option<Renderer> = None;
    let mut index = 0usize;
    for node in root.descendants() {
        let html = {
            let data = node.data.borrow();
            match &data.value {
                NodeValue::CodeBlock(block) if block.fenced && is_mermaid_fence(&block.info) => {
                    let id = format!("jumanji-diagram-{index}");
                    index += 1;
                    let renderer = renderer.get_or_insert_with(Renderer::new);
                    Some(match render(renderer, &block.literal, &id) {
                        Rendered::Svg(svg) => svg,
                        Rendered::Failed(reason) => degrade(&block.literal, &reason),
                    })
                }
                _ => None,
            }
        };
        if let Some(html) = html {
            node.data.borrow_mut().value = NodeValue::HtmlBlock(NodeHtmlBlock {
                block_type: 0,
                literal: html,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Just the parsed width, for the cases that do not care about the span.
    fn width(svg: &str) -> Option<f32> {
        intrinsic_max_width(svg).map(|(px, _, _)| px)
    }

    #[test]
    fn detects_mermaid_fences() {
        assert!(is_mermaid_fence("mermaid"));
        assert!(is_mermaid_fence("Mermaid"));
        assert!(is_mermaid_fence("mermaid title=\"x\""));
        assert!(!is_mermaid_fence("rust"));
        assert!(!is_mermaid_fence(""));
    }

    #[test]
    fn valid_flowchart_renders_inline_svg_with_intrinsic_width() {
        let renderer = Renderer::new();
        match render(&renderer, "flowchart TD\nA[Start] --> B[Done]", "test-ok") {
            Rendered::Svg(html) => {
                // Real merman output carries an inline `max-width:<N>px`, so the
                // wrapper must pin it onto `--dw` for the intrinsic-size model.
                assert!(
                    html.starts_with("<div class=\"mermaid\" style=\"--dw:"),
                    "wrapper should carry a --dw intrinsic width: {html:.80}"
                );
                assert!(html.contains("px\">"));
                assert!(html.contains("<svg"));
            }
            Rendered::Failed(reason) => panic!("expected SVG, got failure: {reason}"),
        }
    }

    #[test]
    fn parses_intrinsic_width_from_inline_style() {
        let svg = "<svg width=\"100%\" style=\"max-width: 512.5px; background: #fff;\">x</svg>";
        assert_eq!(width(svg), Some(512.5));
    }

    #[test]
    fn wrap_svg_moves_the_intrinsic_width_onto_dw() {
        // The inline declaration is deleted, not duplicated: left in place it
        // outranks every selector and would cap `--dz` scaling at intrinsic.
        let svg = "<svg style=\"max-width: 480px; background: #fff;\"></svg>";
        assert_eq!(
            wrap_svg(svg),
            "<div class=\"mermaid\" style=\"--dw:480px\">\
             <svg style=\"background: #fff;\"></svg></div>"
        );
        // Sole declaration: the style attribute is left empty, not malformed.
        assert_eq!(
            wrap_svg("<svg style=\"max-width: 480px;\"></svg>"),
            "<div class=\"mermaid\" style=\"--dw:480px\"><svg style=\"\"></svg></div>"
        );
        // No trailing semicolon.
        assert_eq!(
            wrap_svg("<svg style=\"max-width: 480px\"></svg>"),
            "<div class=\"mermaid\" style=\"--dw:480px\"><svg style=\"\"></svg></div>"
        );
    }

    /// Only the root tag is searched. A `<foreignObject>` label can carry its
    /// own inline `max-width`, and deleting *that* would corrupt the diagram.
    #[test]
    fn wrap_svg_leaves_max_width_inside_the_body_alone() {
        let svg = "<svg viewBox=\"0 0 10 10\"><foreignObject>\
                   <div style=\"max-width: 99px\">x</div></foreignObject></svg>";
        let wrapped = wrap_svg(svg);
        assert!(
            wrapped.contains("max-width: 99px"),
            "the label's own style must survive: {wrapped}"
        );
        // No root declaration to move, so no `--dw` either.
        assert_eq!(wrapped, format!("<div class=\"mermaid\">{svg}</div>"));
    }

    #[test]
    fn wrap_svg_degrades_to_plain_wrapper_without_width() {
        // No parseable `max-width:<N>px` → plain wrapper, no style attribute.
        let svg = "<svg viewBox=\"0 0 10 10\"></svg>";
        assert_eq!(
            wrap_svg(svg),
            "<div class=\"mermaid\"><svg viewBox=\"0 0 10 10\"></svg></div>"
        );
        assert_eq!(width(svg), None);
    }

    /// Regression: merman 0.7 could not lex `;` or `]` inside a quoted
    /// `subgraph` title (`LexError { message: "Unexpected character at N" }`)
    /// even though mermaid.js accepts them — its lexer pushes a `string` state
    /// on `"` and swallows everything up to the closing quote. merman targets
    /// mermaid parity, so this was an upstream bug; 0.8 fixes it. A swept set
    /// of other punctuation parsed on 0.7 already, so these two are the whole
    /// known gap. Red on 0.7.0.
    #[test]
    fn punctuation_in_quoted_subgraph_title_parses() {
        let renderer = Renderer::new();
        for title in ["one; two", "one] two", "a (b; c) [d]"] {
            let source = format!("graph TD\n  subgraph a[\"{title}\"]\n    A[\"x\"]\n  end");
            match render(&renderer, &source, "test-punct") {
                Rendered::Svg(html) => assert!(html.contains("<svg")),
                Rendered::Failed(reason) => {
                    panic!("subgraph title {title:?} should parse: {reason}")
                }
            }
        }
    }

    #[test]
    fn line_col_resolves_offset_to_position_and_line() {
        let source = "graph TD\n  A --> B\n  C";
        assert_eq!(line_col(source, 0), Some((1, 1, "graph TD")));
        // Offset 11 is the `A` on line 2 (9 bytes of line 1 + newline + 2 spaces).
        assert_eq!(line_col(source, 11), Some((2, 3, "  A --> B")));
        assert_eq!(line_col(source, source.len()), Some((3, 4, "  C")));
        assert_eq!(line_col(source, source.len() + 1), None);
    }

    #[test]
    fn line_col_counts_columns_in_characters_not_bytes() {
        // `A["··· x"]` — `·` is 2 bytes, so the `x` is character 8 but byte 10.
        let source = "A[\"··· x\"]";
        let offset = source.find('x').expect("x is present");
        assert_eq!(offset, 10, "the x should be byte 10");
        assert_eq!(line_col(source, offset).map(|(_, col, _)| col), Some(8));
        // A byte offset mid-character resolves to nothing rather than panicking.
        assert_eq!(line_col(source, 4), None);
    }

    /// A parse failure must name a line and column, not a raw byte offset —
    /// "Unexpected character at 964" is unactionable in a reader.
    #[test]
    fn parse_failure_reports_line_and_column() {
        let renderer = Renderer::new();
        let source = "graph TD\n  A --> ]]]";
        match render(&renderer, source, "test-bad") {
            Rendered::Svg(_) => panic!("expected a parse failure"),
            Rendered::Failed(reason) => {
                assert!(
                    reason.contains("line 2"),
                    "failure should name the offending line: {reason}"
                );
                assert!(
                    reason.contains("A --> ]]]"),
                    "failure should quote the offending line: {reason}"
                );
            }
        }
    }

    #[test]
    fn broken_diagram_degrades_without_panic() {
        let html = degrade("not a real diagram !!!", "boom");
        assert!(html.contains("diagram-error__note"));
        assert!(html.contains("Mermaid render failed"));
        // Source preserved as a code block.
        assert!(html.contains("<pre class=\"code\">"));
        assert!(html.contains("not a real diagram"));
    }
}
