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
        Ok(Some(svg)) => Rendered::Svg(wrap_svg(&svg)),
        Ok(None) => Rendered::Failed("no mermaid diagram detected in fence".to_string()),
        Err(err) => Rendered::Failed(err.to_string()),
    }
}

/// Wrap a merman SVG in its `.mermaid` display container, pinning the diagram's
/// intrinsic (natural) width onto the `--dw` custom property so the stylesheet
/// can render it at natural size (overflowing into its own scroll box) rather
/// than shrinking it to fit the column. The intrinsic px width is recoverable
/// from the SVG root's inline `max-width:<N>px` that merman emits (and parses
/// back itself, `merman::render::raster::parse_svg_max_width_px`). If the width
/// can't be parsed, degrade gracefully to a plain wrapper with no `--dw`.
fn wrap_svg(svg: &str) -> String {
    match parse_svg_intrinsic_width_px(svg) {
        Some(px) => format!("<div class=\"mermaid\" style=\"--dw:{px}px\">{svg}</div>"),
        None => format!("<div class=\"mermaid\">{svg}</div>"),
    }
}

/// Parse the intrinsic width (px) out of the SVG root's inline
/// `max-width:<N>px` declaration. Mirrors merman's own parser: find the
/// property, take the numeric run before `px`, parse it as a float.
fn parse_svg_intrinsic_width_px(svg: &str) -> Option<f32> {
    let start = svg.find("max-width:")? + "max-width:".len();
    let rest = svg[start..].trim_start();
    let px_end = rest.find("px")?;
    rest[..px_end].trim().parse::<f32>().ok()
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
    fn valid_flowchart_renders_inline_svg_with_intrinsic_width() {
        match render("flowchart TD\nA[Start] --> B[Done]", "test-ok") {
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
        assert_eq!(parse_svg_intrinsic_width_px(svg), Some(512.5));
    }

    #[test]
    fn wrap_svg_pins_dw_when_width_present() {
        let svg = "<svg style=\"max-width: 480px;\"></svg>";
        assert_eq!(
            wrap_svg(svg),
            "<div class=\"mermaid\" style=\"--dw:480px\"><svg style=\"max-width: 480px;\"></svg></div>"
        );
    }

    #[test]
    fn wrap_svg_degrades_to_plain_wrapper_without_width() {
        // No parseable `max-width:<N>px` → plain wrapper, no style attribute.
        let svg = "<svg viewBox=\"0 0 10 10\"></svg>";
        assert_eq!(
            wrap_svg(svg),
            "<div class=\"mermaid\"><svg viewBox=\"0 0 10 10\"></svg></div>"
        );
        assert_eq!(parse_svg_intrinsic_width_px(svg), None);
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
