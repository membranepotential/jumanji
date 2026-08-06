//! YAML frontmatter → a properties table (DESIGN D11).
//!
//! Frontmatter is **hidden by default** — that is what makes an Obsidian note
//! read like prose rather than opening on a block of machine metadata. Showing
//! it is a reader-side toggle (`:frontmatter`, `show-frontmatter`), so this
//! module only answers one question: when the reader *does* ask for it, what
//! should it look like?
//!
//! Not raw YAML. Obsidian shows properties as a key/value table and so do we:
//! the point of asking for the frontmatter is to read the values, and a
//! monospace dump of the source is a worse way to do that than a table.
//!
//! The parser is deliberately shallow, on the same reasoning (and by the same
//! rules) as `vault::parse_aliases`: one level of `key: value`, the two list
//! spellings Obsidian writes, and *verbatim* text for everything it does not
//! model. It never fails and never lies — an unmodelled value is shown as it
//! was written rather than silently reshaped, so the honest fallback for a
//! whole unparseable block is to print the block.

use super::highlight::escape_html;

/// One property, in source order (which is the order Obsidian preserves).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub key: String,
    pub value: Value,
}

/// A property value, in the three shapes worth distinguishing when *showing*
/// properties. Anything else is [`Value::Raw`] — carried through verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `title: Some note` — a single value, surrounding quotes stripped. An
    /// empty string is a property that was declared with no value.
    Scalar(String),
    /// Both list spellings: inline `tags: [a, b]` and a `- ` block.
    List(Vec<String>),
    /// A nested map, a multi-line scalar — anything with structure this parser
    /// does not model. The source lines, verbatim.
    Raw(String),
}

/// Parse a frontmatter block into its properties. `block` is comrak's
/// `NodeValue::FrontMatter` literal, delimiter lines included.
///
/// An empty result means "nothing recognisable here" — the caller renders the
/// block verbatim rather than showing an empty table.
pub fn parse(block: &str) -> Vec<Property> {
    // (key, inline value, continuation lines) — grouping first, classifying
    // second, so a value's shape is decided with its whole group in hand.
    let mut groups: Vec<(String, String, Vec<&str>)> = Vec::new();
    for line in body_lines(block) {
        match top_level_key(line) {
            Some((key, inline)) => groups.push((key, inline.trim().to_string(), Vec::new())),
            // A continuation of the property above. A non-empty line before any
            // key at all is malformed YAML; dropping it is the graceful move.
            None => match groups.last_mut() {
                Some(group) if !line.trim().is_empty() => group.2.push(line),
                _ => {}
            },
        }
    }
    groups
        .into_iter()
        .map(|(key, inline, rest)| Property {
            key,
            value: classify(&inline, &rest),
        })
        .collect()
}

/// The block's content lines: the `---` opener and the `---`/`...` closer that
/// comrak includes in the literal are not properties.
fn body_lines(block: &str) -> impl Iterator<Item = &str> {
    let mut lines: Vec<&str> = block.lines().collect();
    if matches!(lines.first(), Some(first) if first.trim_end() == "---") {
        lines.remove(0);
    }
    while matches!(lines.last(), Some(last) if last.trim().is_empty()) {
        lines.pop();
    }
    if matches!(lines.last(), Some(last) if matches!(last.trim_end(), "---" | "...")) {
        lines.pop();
    }
    lines.into_iter()
}

/// Split a top-level `key: value` line into its key and the rest. Returns
/// `None` for an indented line (nested under some other key), a `#` comment, a
/// `- ` list item, and anything without a `key:` at all.
fn top_level_key(line: &str) -> Option<(String, &str)> {
    if line.starts_with([' ', '\t', '#', '-']) {
        return None;
    }
    let (key, rest) = line.split_once(':')?;
    // `key:value` is not a YAML mapping — the colon must be followed by space
    // or end of line. This is also what keeps a bare `https://x` from parsing
    // as a property named `https`.
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let key = unquote(key.trim());
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), rest))
}

/// Decide a value's shape from its inline part plus its continuation lines.
fn classify(inline: &str, rest: &[&str]) -> Value {
    match (inline, rest) {
        // `tags: [a, b]`, with nothing trailing it.
        (list, []) if list.starts_with('[') && list.ends_with(']') => {
            Value::List(split_items(&list[1..list.len() - 1]))
        }
        ("", []) => Value::Scalar(String::new()),
        (scalar, []) => Value::Scalar(unquote(scalar).to_string()),
        // A `- ` block under a bare `key:`.
        ("", rest) if rest.iter().all(|l| list_item(l).is_some()) => {
            Value::List(rest.iter().filter_map(|l| list_item(l)).collect())
        }
        // Structure we do not model: a nested map, a `|`/`>` scalar, a list
        // with a stray line in it. Verbatim, with the inline part if any.
        (inline, rest) => {
            let mut out = String::new();
            if !inline.is_empty() {
                out.push_str(inline);
                out.push('\n');
            }
            out.push_str(&dedent(rest));
            Value::Raw(out.trim_end().to_string())
        }
    }
}

/// A `- item` list entry, unquoted and trimmed. `None` if the line is not one.
fn list_item(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let item = match trimmed.strip_prefix("- ") {
        Some(item) => item.trim(),
        // A bare `-` is an empty entry, not a non-entry.
        None if trimmed == "-" => "",
        None => return None,
    };
    Some(unquote(item).to_string())
}

/// Split an inline list body on commas. Naive by intent — the same rule
/// `vault::parse_aliases` applies, where a comma inside a quoted item costs
/// that item a split and nothing more. Empty items are dropped.
fn split_items(body: &str) -> Vec<String> {
    body.split(',')
        .map(|item| unquote(item.trim()).trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Strip one matched pair of surrounding quotes, either spelling.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Join lines with their common leading indentation removed, so a verbatim
/// value is not pushed off to the right by the indentation that nested it.
fn dedent(lines: &[&str]) -> String {
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| if l.len() >= indent { &l[indent..] } else { *l })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a frontmatter block as the HTML the stylesheet dresses: a `<dl>` of
/// properties, or — when nothing parsed — the block itself, verbatim.
///
/// Returns `None` for a block with no content at all, so an empty `---\n---`
/// head does not leave a hairline box floating above the document.
pub fn to_html(block: &str) -> Option<String> {
    let properties = parse(block);
    if properties.is_empty() {
        let raw = body_lines(block).collect::<Vec<_>>().join("\n");
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        return Some(format!(
            "<div class=\"frontmatter\"><pre class=\"frontmatter-raw\">{}</pre></div>",
            escape_html(raw)
        ));
    }
    let mut out = String::from("<div class=\"frontmatter\"><dl>");
    for Property { key, value } in properties {
        out.push_str(&format!(
            "<dt>{}</dt><dd>{}</dd>",
            escape_html(&key),
            value_html(&value)
        ));
    }
    out.push_str("</dl></div>");
    Some(out)
}

fn value_html(value: &Value) -> String {
    match value {
        Value::Scalar(text) if text.is_empty() => {
            "<span class=\"frontmatter-empty\">empty</span>".to_string()
        }
        Value::Scalar(text) => escape_html(text),
        Value::List(items) => items
            .iter()
            .map(|item| {
                format!(
                    "<span class=\"frontmatter-item\">{}</span>",
                    escape_html(item)
                )
            })
            .collect::<Vec<_>>()
            .join(""),
        Value::Raw(text) => format!("<pre class=\"frontmatter-raw\">{}</pre>", escape_html(text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(key: &str, value: &str) -> Property {
        Property {
            key: key.to_string(),
            value: Value::Scalar(value.to_string()),
        }
    }

    #[test]
    fn parses_scalars_in_source_order() {
        let props = parse("---\ntitle: A note\nauthor: \"Ada L\"\n---\n");
        assert_eq!(
            props,
            vec![scalar("title", "A note"), scalar("author", "Ada L")]
        );
    }

    #[test]
    fn parses_both_list_spellings() {
        let inline = parse("---\ntags: [alpha, beta]\n---\n");
        let block = parse("---\ntags:\n  - alpha\n  - beta\n---\n");
        let expected = vec![Property {
            key: "tags".to_string(),
            value: Value::List(vec!["alpha".to_string(), "beta".to_string()]),
        }];
        assert_eq!(inline, expected);
        assert_eq!(block, expected);
    }

    #[test]
    fn a_declared_but_valueless_property_is_an_empty_scalar() {
        assert_eq!(
            parse("---\ncssclasses:\n---\n"),
            vec![scalar("cssclasses", "")]
        );
    }

    #[test]
    fn unmodelled_structure_is_kept_verbatim_and_dedented() {
        let props = parse("---\nnested:\n  a: 1\n  b: 2\n---\n");
        assert_eq!(
            props,
            vec![Property {
                key: "nested".to_string(),
                value: Value::Raw("a: 1\nb: 2".to_string()),
            }]
        );
    }

    #[test]
    fn multiline_scalar_is_verbatim_including_its_marker() {
        let props = parse("---\nnote: |\n  line one\n  line two\n---\n");
        assert_eq!(
            props,
            vec![Property {
                key: "note".to_string(),
                value: Value::Raw("|\nline one\nline two".to_string()),
            }]
        );
    }

    #[test]
    fn a_colon_without_a_following_space_is_not_a_key() {
        // Otherwise a bare URL line would become a property named `https`.
        assert_eq!(parse("---\nhttps://example.com\n---\n"), vec![]);
        assert_eq!(parse("---\nurl: https://example.com\n---\n").len(), 1);
    }

    #[test]
    fn comments_and_indented_lines_do_not_start_properties() {
        let props = parse("---\n# a comment\ntitle: x\n---\n");
        assert_eq!(props, vec![scalar("title", "x")]);
    }

    #[test]
    fn quoted_keys_are_unquoted() {
        assert_eq!(
            parse("---\n\"my key\": x\n---\n"),
            vec![scalar("my key", "x")]
        );
    }

    #[test]
    fn empty_frontmatter_renders_nothing() {
        assert_eq!(to_html("---\n---\n"), None);
        assert_eq!(to_html("---\n\n---\n"), None);
    }

    #[test]
    fn unparseable_frontmatter_falls_back_to_the_block_itself() {
        let html = to_html("---\n- just\n- a list\n---\n").expect("content");
        assert!(html.contains("frontmatter-raw"), "{html}");
        assert!(html.contains("- just"), "{html}");
    }

    #[test]
    fn html_in_a_value_is_escaped() {
        let html = to_html("---\ntitle: <script>x</script>\n---\n").expect("content");
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    #[test]
    fn renders_keys_and_values_as_a_property_list() {
        let html = to_html("---\ntitle: A note\ntags: [x, y]\n---\n").expect("content");
        assert!(html.contains("<dt>title</dt><dd>A note</dd>"), "{html}");
        assert!(
            html.contains("<span class=\"frontmatter-item\">x</span>"),
            "{html}"
        );
    }
}
