//! Mermaid fence interception.
//!
//! A comrak AST pass finds ```` ```mermaid ```` fenced code blocks, renders
//! each to an inline SVG with merman (full-fidelity — the output lands in
//! WebKitGTK, which supports `<foreignObject>`), and replaces the code block
//! node with a raw-HTML node. Rendering failures degrade to the fence shown as
//! a highlighted code block plus a small styled error note — never a panic,
//! never blank output.

use comrak::nodes::{AstNode, NodeHtmlBlock, NodeValue};
use merman::render::{HeadlessRenderer, sanitize_svg_id};

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

fn render(source: &str, diagram_id: &str) -> Rendered {
    let renderer = HeadlessRenderer::new().with_diagram_id(diagram_id);
    match renderer.render_svg_sync(source) {
        Ok(Some(svg)) => Rendered::Svg(format!("<div class=\"mermaid\">{svg}</div>")),
        Ok(None) => Rendered::Failed("no mermaid diagram detected in fence".to_string()),
        Err(err) => Rendered::Failed(err.to_string()),
    }
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
pub fn transform_mermaid<'a>(root: &'a AstNode<'a>) {
    let mut index = 0usize;
    for node in root.descendants() {
        let html = {
            let data = node.data.borrow();
            match &data.value {
                NodeValue::CodeBlock(block) if block.fenced && is_mermaid_fence(&block.info) => {
                    let id = sanitize_svg_id(&format!("jumanji-diagram-{index}"));
                    index += 1;
                    Some(match render(&block.literal, &id) {
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

    #[test]
    fn detects_mermaid_fences() {
        assert!(is_mermaid_fence("mermaid"));
        assert!(is_mermaid_fence("Mermaid"));
        assert!(is_mermaid_fence("mermaid title=\"x\""));
        assert!(!is_mermaid_fence("rust"));
        assert!(!is_mermaid_fence(""));
    }

    #[test]
    fn valid_flowchart_renders_inline_svg() {
        match render("flowchart TD\nA[Start] --> B[Done]", "test-ok") {
            Rendered::Svg(html) => {
                assert!(html.starts_with("<div class=\"mermaid\">"));
                assert!(html.contains("<svg"));
            }
            Rendered::Failed(reason) => panic!("expected SVG, got failure: {reason}"),
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
