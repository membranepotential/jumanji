//! Mermaid fence interception.
//!
//! A comrak AST pass finds ```` ```mermaid ```` fenced code blocks, renders
//! each to an inline SVG with merman (full-fidelity — the output lands in
//! WebKitGTK, which supports `<foreignObject>`), and replaces the code block
//! node with a raw-HTML node. Rendering failures degrade to the fence shown as
//! a highlighted code block plus a small styled error note — never a panic,
//! never blank output.

use std::sync::OnceLock;

use comrak::nodes::{AstNode, NodeHtmlBlock, NodeValue};
use merman::svg::{SvgRenderOptions, sanitize_svg_id};
use merman::{
    Engine, MermaidConfig, OperationControl, RenderError, RenderOutput, RenderRequest, Renderer,
    SvgRequest,
};
use serde_json::{Map, Value};

use super::highlight::{escape_html, highlight_block};

/// How diagrams look (option `diagram-look`).
///
/// Mermaid 12 gave ten diagram families (flowchart, sequence, class, state,
/// ER, …) their own defaults, theme `redux-color` and look `neo`, while the
/// global defaults stayed theme `default` and look `classic`. merman 0.9
/// follows it. The two variants pick between those.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiagramLook {
    /// Mermaid 11's look, the one jumanji had before merman 0.9: theme
    /// `default`, look `classic`, for every family.
    #[default]
    Classic,
    /// Mermaid 12's defaults, unchanged.
    Neo,
}

impl DiagramLook {
    /// The option spelling, which [`DiagramLook::parse`] round-trips.
    pub fn name(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Neo => "neo",
        }
    }

    /// Parse the option value, case-insensitively.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "classic" => Ok(Self::Classic),
            "neo" => Ok(Self::Neo),
            other => Err(format!(
                "unknown diagram look {other:?}: expected \"classic\" or \"neo\""
            )),
        }
    }

    /// The renderer that draws in this look, built on first use and then
    /// shared by every render in the process.
    ///
    /// Shared because building one is not cheap: the classic site config
    /// makes merman re-derive its whole effective config and theme, about
    /// 3 M instructions — more than a small diagram costs to render. The
    /// renderer holds only immutable engine and parse config (`render` takes
    /// `&self`, and per-fence state rides in the [`SvgRequest`]), so sharing
    /// it is sound. Lazy, so a process that never meets a diagram never pays.
    fn renderer(self) -> &'static Renderer {
        static CLASSIC: OnceLock<Renderer> = OnceLock::new();
        static NEO: OnceLock<Renderer> = OnceLock::new();
        match self {
            Self::Classic => CLASSIC.get_or_init(|| {
                Renderer::new().with_engine(Engine::new().with_site_config(classic_site_config()))
            }),
            Self::Neo => NEO.get_or_init(Renderer::new),
        }
    }
}

/// The site config behind [`DiagramLook::Classic`]: theme `default` and look
/// `classic` at the top level and in every section whose Mermaid default sets
/// its own, plus Mermaid 11's flowchart and state node sizing. The sections come from merman's defaults, so a family that gains
/// its own look in a later Mermaid is covered without a list to maintain.
///
/// Every section is overridden, not just the top level, because merman
/// resolves a section's appearance before the global one: a top-level
/// override alone would leave the sections on `neo`. Matching values also keep
/// merman off its slow path — when the resolved appearance differs from the
/// config's, it rewrites the shared config, deep-cloning it on every parse.
fn classic_site_config() -> MermaidConfig {
    const CLASSIC: [(&str, &str); 2] = [("theme", "default"), ("look", "classic")];
    let appearance = |defaults: &Map<String, Value>| -> Map<String, Value> {
        CLASSIC
            .iter()
            .filter(|(key, _)| defaults.contains_key(*key))
            .map(|(key, value)| (key.to_string(), Value::from(*value)))
            .collect()
    };
    let defaults = merman::generated::default_site_config();
    let mut config: Map<String, Value> = CLASSIC
        .iter()
        .map(|(key, value)| (key.to_string(), Value::from(*value)))
        .collect();
    if let Some(sections) = defaults.as_value().as_object() {
        for (name, section) in sections {
            if let Some(section) = section.as_object() {
                let overrides = appearance(section);
                if !overrides.is_empty() {
                    config.insert(name.clone(), Value::Object(overrides));
                }
            }
        }
    }
    // Mermaid 12 also grew flowchart and state nodes, in either look: a
    // minimum node width of 120 and wrapping at 120 instead of 200. Mermaid
    // 11 had no minimum, so a short label kept a snug box.
    for section in ["flowchart", "state"] {
        if let Value::Object(section) = config
            .entry(section)
            .or_insert_with(|| Value::Object(Map::new()))
        {
            section.insert("minNodeWidth".into(), Value::from(0));
            section.insert("wrappingWidth".into(), Value::from(200));
        }
    }
    MermaidConfig::from_value(Value::Object(config))
}

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
/// `look` picks the diagrams' look and with it the process-wide renderer; see
/// [`DiagramLook::renderer`].
pub fn transform_mermaid<'a>(root: &'a AstNode<'a>, look: DiagramLook) {
    let mut index = 0usize;
    for node in root.descendants() {
        let html = {
            let data = node.data.borrow();
            match &data.value {
                NodeValue::CodeBlock(block) if block.fenced && is_mermaid_fence(&block.info) => {
                    let id = format!("jumanji-diagram-{index}");
                    index += 1;
                    Some(match render(look.renderer(), &block.literal, &id) {
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

    /// The line before a fence that is broken on purpose (the demo's
    /// graceful-degradation sample). Such a fence must *fail* to render.
    const EXPECTED_FAILURE: &str = "<!-- mermaid: expected to fail -->";

    /// Every markdown file in the repository, outside build and tool output.
    fn markdown_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if !matches!(
                    name.to_str(),
                    Some("target" | ".git" | ".claude" | "node_modules" | "packaging")
                ) {
                    markdown_files(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }

    /// The docs are read in jumanji, so their diagrams must parse in merman,
    /// the engine the reader renders with. (A node named `graph` broke
    /// docs/graph/README.md: it is a keyword in merman and mermaid.js alike.)
    #[test]
    fn every_mermaid_diagram_in_the_repository_renders() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        markdown_files(root, &mut files);
        files.sort();
        let renderer = DiagramLook::default().renderer();
        let (mut checked, mut problems) = (0, Vec::new());
        for file in &files {
            let md = std::fs::read_to_string(file).expect("read markdown");
            let lines: Vec<&str> = md.lines().collect();
            let arena = comrak::Arena::new();
            let doc = comrak::parse_document(&arena, &md, &crate::core::pipeline::comrak_options());
            for node in doc.descendants() {
                let data = node.data.borrow();
                let NodeValue::CodeBlock(block) = &data.value else {
                    continue;
                };
                if !block.fenced || !is_mermaid_fence(&block.info) {
                    continue;
                }
                checked += 1;
                let line = data.sourcepos.start.line;
                let expected_failure = line >= 2 && lines[line - 2].trim() == EXPECTED_FAILURE;
                let at = format!(
                    "{}:{line}",
                    file.strip_prefix(root).unwrap_or(file).display()
                );
                match (render(renderer, &block.literal, "check"), expected_failure) {
                    (Rendered::Svg(_), false) | (Rendered::Failed(_), true) => {}
                    (Rendered::Failed(reason), false) => problems.push(format!("{at}: {reason}")),
                    (Rendered::Svg(_), true) => {
                        problems.push(format!("{at}: marked {EXPECTED_FAILURE} but renders"))
                    }
                }
            }
        }
        assert!(
            checked > 0,
            "no mermaid fences found under {}",
            root.display()
        );
        assert!(
            problems.is_empty(),
            "mermaid diagrams that fail:\n{}",
            problems.join("\n")
        );
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
        let renderer = DiagramLook::default().renderer();
        match render(renderer, "flowchart TD\nA[Start] --> B[Done]", "test-ok") {
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

    /// The two looks, told apart by what each one's SVG carries: classic
    /// nodes are tagged `data-look="classic"` and filled with Mermaid 11's
    /// `#ECECFF`; neo nodes are tagged `data-look="neo"` and filled from the
    /// `redux-color` palette. (Both stylesheets carry `[data-look="neo"]`
    /// selectors, hence the leading space: it matches the attribute only.)
    fn rendered(look: DiagramLook, source: &str) -> String {
        match render(look.renderer(), source, "test-look") {
            Rendered::Svg(svg) => svg,
            Rendered::Failed(reason) => panic!("expected SVG, got failure: {reason}"),
        }
    }

    #[test]
    fn classic_look_draws_every_family_as_mermaid_11_did() {
        let flowchart = rendered(
            DiagramLook::Classic,
            "flowchart TD
A[Start] --> B[Done]",
        );
        assert!(flowchart.contains(" data-look=\"classic\""), "{flowchart}");
        assert!(!flowchart.contains(" data-look=\"neo\""), "{flowchart}");
        assert!(flowchart.contains("fill:#ECECFF"), "{flowchart}");
        // Sequence diagrams have their own Mermaid 12 default too, so this
        // proves the per-section override, not just the global one.
        let sequence = rendered(
            DiagramLook::Classic,
            "sequenceDiagram
A->>B: hi",
        );
        assert!(!sequence.contains(" data-look=\"neo\""), "{sequence}");
        assert!(sequence.contains("fill:#ECECFF"), "{sequence}");
    }

    /// The width of an SVG's `viewBox`.
    fn view_box_width(svg: &str) -> f64 {
        let view_box = svg.split("viewBox=\"").nth(1).expect("a viewBox");
        let width = view_box.split(['"', ' ']).nth(2).expect("a viewBox width");
        width.parse().expect("a numeric viewBox width")
    }

    #[test]
    fn classic_look_keeps_mermaid_11s_snug_nodes() {
        // Mermaid 12 gives flowchart nodes a minimum width of 120; Mermaid 11
        // sized a short label's box to the label.
        let source = "flowchart TD\nA[Start]";
        let classic = view_box_width(&rendered(DiagramLook::Classic, source));
        let neo = view_box_width(&rendered(DiagramLook::Neo, source));
        assert!(classic < 120.0, "classic is {classic} wide");
        assert!(neo > classic, "neo {neo} vs classic {classic}");
    }

    #[test]
    fn neo_look_keeps_mermaid_12s_defaults() {
        for source in [
            "flowchart TD
A[Start] --> B[Done]",
            "sequenceDiagram
A->>B: hi",
        ] {
            let svg = rendered(DiagramLook::Neo, source);
            assert!(svg.contains(" data-look=\"neo\""), "{svg}");
            assert!(!svg.contains("#ECECFF"), "{svg}");
        }
    }

    #[test]
    fn a_diagrams_own_init_directive_outranks_the_look() {
        let svg = rendered(
            DiagramLook::Classic,
            "%%{init: {\"look\": \"neo\"}}%%\nflowchart TD\nA[Start] --> B[Done]",
        );
        assert!(svg.contains(" data-look=\"neo\""), "{svg}");
    }

    #[test]
    fn diagram_look_parses_its_names() {
        for look in [DiagramLook::Classic, DiagramLook::Neo] {
            assert_eq!(DiagramLook::parse(look.name()), Ok(look));
        }
        assert_eq!(DiagramLook::parse(" NEO "), Ok(DiagramLook::Neo));
        assert!(DiagramLook::parse("dark").is_err());
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
        let renderer = DiagramLook::default().renderer();
        for title in ["one; two", "one] two", "a (b; c) [d]"] {
            let source = format!("graph TD\n  subgraph a[\"{title}\"]\n    A[\"x\"]\n  end");
            match render(renderer, &source, "test-punct") {
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
        let renderer = DiagramLook::default().renderer();
        let source = "graph TD\n  A --> ]]]";
        match render(renderer, source, "test-bad") {
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
