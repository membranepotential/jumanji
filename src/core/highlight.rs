//! Syntax highlighting via syntect, emitting *classed* HTML (not inline
//! styles) so a single highlighted document can be recoloured for light and
//! dark themes by swapping CSS. Two CSS blocks are generated once and embedded
//! in the page head by [`super::pipeline`].

use std::sync::OnceLock;

use comrak::nodes::{AstNode, NodeHtmlBlock, NodeValue};
use syntect::highlighting::Theme;
use syntect::html::{ClassStyle, ClassedHTMLGenerator, css_for_theme_with_class_style};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use two_face::theme::EmbeddedThemeName;

/// Spaced classes keep the generated CSS selectors readable (`.comment`,
/// `.keyword`, …) and match [`css_for_theme_with_class_style`] output.
const CLASS_STYLE: ClassStyle = ClassStyle::Spaced;

/// Light theme: GitHub's classic light palette (white background).
const LIGHT_THEME: EmbeddedThemeName = EmbeddedThemeName::InspiredGithub;
/// Dark theme: a calm, high-legibility dark palette that reads well on #1a1a1a.
const DARK_THEME: EmbeddedThemeName = EmbeddedThemeName::Base16OceanDark;

/// The extended syntax set (bat's set, via two-face). Loaded once.
///
/// The *newlines* variant is required by
/// [`ClassedHTMLGenerator::parse_html_for_line_which_includes_newline`].
fn syntax_set() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

/// Highlight one code block into `<pre class="code"><code>…</code></pre>` with
/// syntect classes on inner spans. The `class="code"` wrapper is what the
/// generated theme CSS targets for the block background/foreground.
///
/// Unknown languages (or a highlighter error) degrade to plain, HTML-escaped
/// text in the same wrapper — never a panic, never dropped content.
pub fn highlight_block(info: &str, code: &str) -> String {
    let syntaxes = syntax_set();
    let token = info.split_whitespace().next().unwrap_or_default();
    let syntax = syntaxes
        .find_syntax_by_token(token)
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());

    let mut generator = ClassedHTMLGenerator::new_with_class_style(syntax, syntaxes, CLASS_STYLE);
    for line in LinesWithEndings::from(code) {
        if generator
            .parse_html_for_line_which_includes_newline(line)
            .is_err()
        {
            return plain_block(code);
        }
    }
    format!(
        "<pre class=\"code\"><code>{}</code></pre>",
        generator.finalize()
    )
}

/// Fallback rendering: escaped, unstyled code in the themed wrapper.
fn plain_block(code: &str) -> String {
    format!(
        "<pre class=\"code\"><code>{}</code></pre>",
        escape_html(code)
    )
}

/// CSS for the light syntax theme (unscoped: applies by default).
pub fn light_css() -> &'static str {
    static CSS: OnceLock<String> = OnceLock::new();
    CSS.get_or_init(|| theme_css(LIGHT_THEME))
}

/// CSS for the dark syntax theme, scoped under `html.dark` (via CSS nesting)
/// so it only applies when the shell has toggled dark mode.
pub fn dark_css() -> &'static str {
    static CSS: OnceLock<String> = OnceLock::new();
    CSS.get_or_init(|| format!("html.dark {{\n{}\n}}\n", theme_css(DARK_THEME)))
}

fn theme_css(name: EmbeddedThemeName) -> String {
    let themes = two_face::theme::extra();
    let theme: &Theme = themes.get(name);
    // Infallible in practice for the embedded themes; degrade to empty CSS
    // rather than propagate an error into the render path.
    css_for_theme_with_class_style(theme, CLASS_STYLE).unwrap_or_default()
}

/// AST pass: replace every remaining fenced/indented code block with its
/// highlighted HTML. Run *after* the mermaid pass so mermaid fences (already
/// turned into HTML blocks) are left untouched.
pub fn highlight_code_blocks<'a>(root: &'a AstNode<'a>) {
    for node in root.descendants() {
        let replacement = match &node.data.borrow().value {
            NodeValue::CodeBlock(block) => Some(highlight_block(&block.info, &block.literal)),
            _ => None,
        };
        if let Some(html) = replacement {
            node.data.borrow_mut().value = NodeValue::HtmlBlock(NodeHtmlBlock {
                block_type: 0,
                literal: html,
            });
        }
    }
}

/// Minimal HTML-text escaping for the plain-text degrade path.
pub(crate) fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_language_gets_syntect_classes() {
        let html = highlight_block("rust", "fn main() {}\n");
        assert!(html.starts_with("<pre class=\"code\"><code>"));
        assert!(html.contains("class=\""), "expected classed spans: {html}");
    }

    #[test]
    fn unknown_language_degrades_to_plain_wrapper() {
        let html = highlight_block("this-is-not-a-language", "just text\n");
        assert!(html.starts_with("<pre class=\"code\"><code>"));
        assert!(html.contains("just text"));
    }

    #[test]
    fn escapes_html_in_code() {
        assert_eq!(escape_html("a<b>&\"c"), "a&lt;b&gt;&amp;&quot;c");
    }

    #[test]
    fn theme_css_blocks_are_nonempty_and_scoped() {
        assert!(light_css().contains(".code"));
        assert!(dark_css().starts_with("html.dark {"));
        assert!(dark_css().contains(".code"));
    }
}
