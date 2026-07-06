//! LaTeX math (`$inline$`, `$$display$$`, and code-math) -> MathML Core.
//!
//! comrak's math extension (`math_dollars` + `math_code`) yields inline
//! `NodeValue::Math` nodes carrying the raw LaTeX. This pass renders each to
//! MathML with pulldown-latex (a pure-Rust LaTeX -> MathML Core renderer, ~95%
//! KaTeX coverage) and replaces the node with an inline raw-HTML node — the same
//! parse -> mutate -> format shape `diagram.rs` uses for mermaid.
//!
//! WebKitGTK renders MathML Core natively, so no JavaScript is involved; the
//! Latin Modern math fonts and pulldown-latex's stylesheet (both vendored under
//! `assets/math/`) are emitted once into the page head by `pipeline::assemble`,
//! with the fonts inlined as base64 `data:` URIs (jumanji serves no external
//! resources — there is no `app://` scheme and the CSP is `default-src 'none'`).
//!
//! Graceful degradation is a binding convention: a LaTeX render failure — a
//! parser error (pulldown-latex emits an inline `<merror>`), or an unbalanced
//! group/environment (which *panics* inside pulldown-latex's writer) — never
//! crashes and never blanks the page. It degrades to the raw source shown as a
//! code span (inline) or a small error box (display), mirroring `diagram.rs`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

use comrak::nodes::{AstNode, NodeMath, NodeValue};
use pulldown_latex::config::DisplayMode;
use pulldown_latex::{Parser, RenderConfig, Storage, push_mathml};

use super::highlight::escape_html;

/// Which MathML display style a math node renders in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Display {
    /// `$...$` (and code-math): inline, `\int`/`\sum` minimised to the line.
    Inline,
    /// `$$...$$`: block, centred on its own line, large operators.
    Block,
}

/// AST pass: replace every `NodeValue::Math` node with an inline raw-HTML node
/// holding its MathML (or a degraded error span). Returns `true` if the document
/// contained any math, so the pipeline can include the math stylesheet + fonts
/// only when they are actually needed.
pub fn transform_math<'a>(root: &'a AstNode<'a>) -> bool {
    let mut any = false;
    for node in root.descendants() {
        let html = {
            let data = node.data.borrow();
            match &data.value {
                NodeValue::Math(math) => Some(render_or_degrade(math)),
                _ => None,
            }
        };
        if let Some(html) = html {
            any = true;
            // Math is an inline node, so the replacement must be inline-level
            // too (`HtmlInline`, the inline analogue of the `HtmlBlock` that
            // `diagram.rs` uses). `render.unsafe` (set in `pipeline`) lets it
            // through verbatim.
            node.data.borrow_mut().value = NodeValue::HtmlInline(html);
        }
    }
    any
}

/// Render one math node to MathML, or degrade to its source on any failure.
fn render_or_degrade(math: &NodeMath) -> String {
    let display = if math.display_math {
        Display::Block
    } else {
        Display::Inline
    };
    match render_mathml(&math.literal, display) {
        // Wrap display math in a block-level scroll container so a wide equation
        // (big matrix, long alignment) scrolls inside its own box rather than
        // pushing the page horizontally — the same no-page-h-scroll invariant
        // tables (`.table-wrap`) and diagrams (`.mermaid`) honour. A `<span>`
        // set to `display:block` in CSS is a valid inline child of the enclosing
        // `<p>` (a `<div>` would force the browser to split the paragraph) while
        // still establishing the scroll box.
        Some(mathml) if display == Display::Block => {
            format!("<span class=\"math-scroll\">{mathml}</span>")
        }
        Some(mathml) => mathml,
        None => degrade(&math.literal, display),
    }
}

/// Render LaTeX to a `<math>` fragment, or `None` on any render failure.
///
/// Two failure modes are handled, both as `None`:
/// - **panic** — an unbalanced `\begin`/group makes pulldown-latex's writer
///   `panic!` on its end-of-input stack check; `catch_unwind` contains it.
/// - **inline `<merror>`** — a recoverable parser error (an unknown command,
///   say) is emitted by pulldown-latex as an inline `<merror>` element. We treat
///   its presence as a failure so degradation is uniform with `diagram.rs`
///   (source shown as code + note) rather than a half-rendered equation.
fn render_mathml(latex: &str, display: Display) -> Option<String> {
    let mode = match display {
        Display::Inline => DisplayMode::Inline,
        Display::Block => DisplayMode::Block,
    };
    let rendered = catch_unwind(AssertUnwindSafe(|| {
        let storage = Storage::new();
        let parser = Parser::new(latex, &storage);
        let config = RenderConfig {
            display_mode: mode,
            ..RenderConfig::default()
        };
        let mut out = String::new();
        // push_mathml only fails on a writer I/O error; a `String` sink cannot
        // produce one, so this is effectively infallible here.
        push_mathml(&mut out, parser, config).ok().map(|()| out)
    }))
    .ok()
    .flatten()?;

    if rendered.contains("<merror") {
        None
    } else {
        Some(rendered)
    }
}

/// Graceful degradation: show the raw LaTeX (with its delimiters) as a styled
/// code span. Inline math degrades inline; display math degrades to a
/// block-level box with a small error note. Both stay inline-level HTML so they
/// remain valid inside the enclosing paragraph (a `<pre>`/`<div>` block would
/// force the browser to split the `<p>`).
fn degrade(latex: &str, display: Display) -> String {
    match display {
        Display::Inline => format!(
            "<code class=\"math-error\" title=\"invalid LaTeX math\">${}$</code>",
            escape_html(latex)
        ),
        Display::Block => format!(
            "<span class=\"math-error math-error--block\">\
               <span class=\"math-error__note\">\u{26a0} invalid LaTeX math</span>\
               <code>$${}$$</code>\
             </span>",
            escape_html(latex)
        ),
    }
}

/// The math stylesheet: pulldown-latex's `styles.css` with each vendored font
/// `url('font/…woff2')` rewritten to a base64 `data:` URI, so the page stays
/// self-contained (no external fetch, no `app://` scheme). Built once, cached —
/// the base64 of ~0.5 MB of fonts is computed a single time per process.
pub fn math_css() -> &'static str {
    static CSS: OnceLock<String> = OnceLock::new();
    CSS.get_or_init(build_math_css)
}

const STYLES: &str = include_str!("assets/math/styles.css");
const FONT_MATH: &[u8] = include_bytes!("assets/math/font/latinmodern-math.woff2");
const FONT_REGULAR: &[u8] = include_bytes!("assets/math/font/lmroman12-regular.woff2");
const FONT_BOLD: &[u8] = include_bytes!("assets/math/font/lmroman12-bold.woff2");
const FONT_ITALIC: &[u8] = include_bytes!("assets/math/font/lmroman12-italic.woff2");

fn build_math_css() -> String {
    STYLES
        .replace(
            "url('font/latinmodern-math.woff2')",
            &font_data_url(FONT_MATH),
        )
        .replace(
            "url('font/lmroman12-regular.woff2')",
            &font_data_url(FONT_REGULAR),
        )
        .replace(
            "url('font/lmroman12-bold.woff2')",
            &font_data_url(FONT_BOLD),
        )
        .replace(
            "url('font/lmroman12-italic.woff2')",
            &font_data_url(FONT_ITALIC),
        )
}

/// A CSS `url('data:…')` token wrapping a WOFF2 font as base64.
fn font_data_url(bytes: &[u8]) -> String {
    format!("url('data:font/woff2;base64,{}')", base64_encode(bytes))
}

/// Standard base64 (RFC 4648, with `=` padding). Hand-rolled to avoid a
/// dependency for one small, timeless, well-specified transform.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_inline(latex: &str) -> Option<String> {
        render_mathml(latex, Display::Inline)
    }

    #[test]
    fn inline_math_renders_math_element_inline() {
        let html = render_inline("x^2 + y^2").expect("valid latex renders");
        assert!(html.starts_with("<math"));
        assert!(html.contains("display=\"inline\""));
        assert!(html.contains("</math>"));
        // The superscript became a real MathML element, not literal text.
        assert!(html.contains("<msup"));
    }

    #[test]
    fn display_math_is_block_style() {
        let html = render_mathml("\\int_0^1 x\\,dx", Display::Block).expect("valid latex");
        assert!(html.contains("display=\"block\""));
        assert!(html.contains("</math>"));
    }

    #[test]
    fn matrix_environment_renders_a_table() {
        let html = render_mathml(
            "\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}",
            Display::Block,
        )
        .expect("matrix renders");
        assert!(html.contains("<mtable"));
        assert!(html.contains("<mtr"));
    }

    #[test]
    fn unknown_command_degrades_to_none_not_partial_render() {
        // pulldown-latex emits an inline <merror> for an unknown command; we
        // report that as a render failure so degradation is uniform.
        assert_eq!(render_inline("\\thiscommanddoesnotexist"), None);
    }

    #[test]
    fn unbalanced_environment_does_not_panic_and_degrades() {
        // A `\begin` with no `\end` makes pulldown-latex's writer panic on its
        // end-of-input stack check; catch_unwind must contain it -> None.
        assert_eq!(
            render_mathml("\\begin{pmatrix} a & b", Display::Block),
            None
        );
    }

    #[test]
    fn degrade_inline_is_a_code_span_with_delimiters() {
        let html = degrade("x_", Display::Inline);
        assert!(html.starts_with("<code class=\"math-error\""));
        assert!(html.contains("$x_$"));
        assert!(!html.contains("<span")); // stays inline, no block box
    }

    #[test]
    fn degrade_display_is_a_block_box_with_note() {
        let html = degrade("\\frac{1}{", Display::Block);
        assert!(html.contains("math-error--block"));
        assert!(html.contains("math-error__note"));
        assert!(html.contains("invalid LaTeX math"));
        assert!(html.contains("$$\\frac{1}{$$"));
    }

    #[test]
    fn degrade_escapes_html_in_source() {
        // The raw source is HTML-escaped so a `<` in the LaTeX can't inject markup.
        let html = degrade("a < b & c", Display::Inline);
        assert!(html.contains("a &lt; b &amp; c"));
        assert!(!html.contains("a < b"));
    }

    #[test]
    fn math_css_inlines_fonts_as_data_uris_and_has_no_external_refs() {
        let css = math_css();
        assert!(css.contains("data:font/woff2;base64,"));
        // Every vendored font url must be rewritten — nothing points at `font/`.
        assert!(!css.contains("url('font/"));
        // The dark-mode color patch: the negation slash tracks currentColor.
        assert!(css.contains("currentColor"));
    }

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
