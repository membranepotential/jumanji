//! The Obsidian reference grammar: a parsed `[[…]]` / `![[…]]` target.
//!
//! comrak hands us `NodeWikiLink { url: String }` and nothing else — the
//! fragment rides along inside the URL, `clean_url`-encoded, and embeds never
//! reach the parser at all (`docs/research/04-obsidian.md` §5). So the target
//! is re-parsed here into a real ADT before anything tries to resolve it
//! (DESIGN D11). This module is pure string work: no filesystem, no vault.
//!
//! The grammar, in the order the pieces appear (research §1.1):
//! `[[note#frag#frag|pipe]]`, where the pipe is an alias for a link and
//! `W`/`WxH` dimensions for an embed.

use std::path::Path;

use comrak::Anchorizer;

/// Whether the reference was written as a link (`[[…]]`) or an embed
/// (`![[…]]`). It changes what the pipe means and how the fragment is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Link,
    Embed,
}

/// A block identifier (`^37066d`), stored **without** the leading `^`.
/// Obsidian's charset is Latin letters, numbers and dashes (research §1.4);
/// anything else is not a block reference, so [`BlockId::new`] rejects it and
/// the fragment degrades to none rather than to a dangling anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockId(String);

impl BlockId {
    pub fn new(id: &str) -> Option<Self> {
        let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        valid.then(|| Self(id.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What follows the `#`: a nested heading path (`#A#B` is *two* headings, never
/// a literal `#` — research §1.1) or a block reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fragment {
    Heading(Vec<String>),
    Block(BlockId),
}

/// A parsed, percent-decoded reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiRef {
    /// The note/asset name or vault-relative path. `None` for a same-file
    /// reference (`[[#Heading]]`).
    pub note: Option<String>,
    pub fragment: Option<Fragment>,
    /// Alias (link) or dimensions (embed); `None` when absent or empty.
    pub pipe: Option<String>,
    pub kind: RefKind,
}

/// Parse an **already percent-decoded** target into a [`WikiRef`]. Never fails:
/// every malformed shape degrades to a reference that simply resolves to
/// nothing (`[[]]`, `[[Note#^]]`), which is what a reader wants.
pub fn parse(raw: &str, kind: RefKind) -> WikiRef {
    // Split on the *first* pipe: comrak aborts the whole construct on a second
    // one (research §5), so anything past it is not our problem.
    let (head, pipe) = match raw.split_once('|') {
        Some((head, pipe)) => (head, non_empty(pipe)),
        None => (raw, None),
    };

    let mut parts = head.split('#');
    let note = non_empty(parts.next().unwrap_or(""));
    let components: Vec<String> = parts
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect();

    let fragment = match components.first() {
        None => None,
        Some(first) => match first.strip_prefix('^') {
            // A block reference consumes the whole fragment; Obsidian has no
            // `#Heading#^id` form.
            Some(id) => BlockId::new(id).map(Fragment::Block),
            // `![[doc.pdf#page=3]]` / `#height=400` are viewer parameters, not
            // anchors: parsed and dropped, so `Fragment` stays two-state.
            None if kind == RefKind::Embed && components.len() == 1 && is_viewer_param(first) => {
                None
            }
            None => Some(Fragment::Heading(components)),
        },
    };

    WikiRef {
        note,
        fragment,
        pipe,
        kind,
    }
}

/// `Some(trimmed)` unless the trimmed string is empty.
fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// `page=<n>` / `height=<n>` — the PDF-viewer parameters (research §2).
fn is_viewer_param(component: &str) -> bool {
    match component.split_once('=') {
        Some((key, value)) => {
            matches!(key, "page" | "height")
                && !value.is_empty()
                && value.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Obsidian's display text for a reference: the alias when one is given (links
/// only — an embed's pipe carries dimensions), otherwise the note name with the
/// fragment components appended, joined by `" > "` (`Note > Heading`). Empty
/// only for a wholly empty target (`[[]]`).
pub fn display_label(r: &WikiRef) -> String {
    if r.kind == RefKind::Link
        && let Some(alias) = &r.pipe
    {
        return alias.clone();
    }
    let mut parts: Vec<String> = r.note.iter().cloned().collect();
    match &r.fragment {
        Some(Fragment::Heading(components)) => parts.extend(components.iter().cloned()),
        Some(Fragment::Block(id)) => parts.push(format!("^{}", id.as_str())),
        None => {}
    }
    parts.join(" > ")
}

/// An embed's `|W` / `|WxH` dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimensions {
    pub w: u32,
    pub h: Option<u32>,
}

/// Parse an embed pipe as dimensions. Anything else (an alias accidentally
/// written on an embed, say) is `None` and simply carries no size.
pub fn parse_dimensions(pipe: &str) -> Option<Dimensions> {
    let pipe = pipe.trim();
    match pipe.split_once(['x', 'X']) {
        Some((w, h)) => Some(Dimensions {
            w: w.trim().parse().ok()?,
            h: Some(h.trim().parse().ok()?),
        }),
        None => Some(Dimensions {
            w: pipe.parse().ok()?,
            h: None,
        }),
    }
}

/// The GitHub-style slug for a heading's text, matching what comrak emits as
/// the heading's `id` and what `core::toc` records as its anchor.
///
/// A **fresh** [`Anchorizer`] every call, deliberately: Obsidian resolves a
/// duplicate heading to the first occurrence, and the first occurrence is
/// exactly the one that gets the unsuffixed slug. Sharing `toc::extract`'s
/// instance would hand out `-1`, `-2`, … and every such link would miss.
pub fn heading_slug(text: &str) -> String {
    Anchorizer::new().anchorize(text)
}

/// Decode `%XX` escapes. Invalid escapes pass through verbatim (a lone `%` in
/// a filename is legal); if the decoded bytes are not UTF-8, the input is
/// returned unchanged rather than lossily mangled.
///
/// Hand-rolled because `percent-encoding` would be a dependency for two call
/// sites, and comrak's encoding is the only producer we ever decode.
pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let src = s.as_bytes();
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%'
            && i + 2 < src.len()
            && let (Some(hi), Some(lo)) = (hex_digit(src[i + 1]), hex_digit(src[i + 2]))
        {
            out.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        out.push(src[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// An absolute path as a `file://` URI, percent-encoding everything outside the
/// URI unreserved set (`/` excepted). Vault-resolved targets routinely sit
/// outside the document's own directory, so the webview's base URI cannot carry
/// them, and Obsidian filenames routinely contain spaces and `#`.
pub fn file_uri(path: &Path) -> String {
    format!("file://{}", percent_encode(&path.to_string_lossy()))
}

/// Percent-encode everything outside the URI unreserved set, keeping `/`.
/// Used for both the path and the fragment of an emitted `file://` link — a
/// block id's `^` and a slug's non-ASCII letters both need it.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn link(raw: &str) -> WikiRef {
        parse(raw, RefKind::Link)
    }

    fn embed(raw: &str) -> WikiRef {
        parse(raw, RefKind::Embed)
    }

    fn headings(r: &WikiRef) -> Option<Vec<String>> {
        match &r.fragment {
            Some(Fragment::Heading(components)) => Some(components.clone()),
            _ => None,
        }
    }

    // --- the target grammar (research §1.1) -------------------------------

    #[test]
    fn bare_note() {
        let r = link("Note");
        assert_eq!(r.note.as_deref(), Some("Note"));
        assert_eq!(r.fragment, None);
        assert_eq!(r.pipe, None);
    }

    #[test]
    fn folder_path_is_kept_whole() {
        assert_eq!(link("folder/Note").note.as_deref(), Some("folder/Note"));
    }

    #[test]
    fn pipe_is_the_alias() {
        let r = link("Note|Display text");
        assert_eq!(r.note.as_deref(), Some("Note"));
        assert_eq!(r.pipe.as_deref(), Some("Display text"));
    }

    #[test]
    fn heading_fragment() {
        let r = link("Note#Heading");
        assert_eq!(headings(&r), Some(vec!["Heading".to_string()]));
    }

    #[test]
    fn nested_heading_path() {
        let r = link("Note#Heading#Subheading");
        assert_eq!(
            headings(&r),
            Some(vec!["Heading".to_string(), "Subheading".to_string()])
        );
    }

    #[test]
    fn same_file_heading_has_no_note() {
        let r = link("#Heading");
        assert_eq!(r.note, None);
        assert_eq!(headings(&r), Some(vec!["Heading".to_string()]));
    }

    #[test]
    fn block_reference_drops_the_caret() {
        let r = link("Note#^block-id");
        assert_eq!(
            r.fragment,
            Some(Fragment::Block(BlockId::new("block-id").unwrap()))
        );
    }

    #[test]
    fn fragment_then_alias_in_that_order() {
        let r = link("Note#Heading|alias");
        assert_eq!(r.note.as_deref(), Some("Note"));
        assert_eq!(headings(&r), Some(vec!["Heading".to_string()]));
        assert_eq!(r.pipe.as_deref(), Some("alias"));
    }

    #[test]
    fn md_extension_is_kept_for_the_resolver() {
        assert_eq!(link("Note.md").note.as_deref(), Some("Note.md"));
    }

    #[test]
    fn degenerate_targets_never_panic() {
        assert_eq!(
            link(""),
            WikiRef {
                note: None,
                fragment: None,
                pipe: None,
                kind: RefKind::Link
            }
        );
        assert_eq!(link("Note#").fragment, None);
        // `^` with no id is not a block reference — and not a heading either.
        assert_eq!(link("Note#^").fragment, None);
        assert_eq!(link("Note#^bad id!").fragment, None);
    }

    // --- embeds (research §2) ---------------------------------------------

    #[test]
    fn embed_dimensions_ride_the_pipe() {
        assert_eq!(embed("img.png|100x145").pipe.as_deref(), Some("100x145"));
        assert_eq!(
            parse_dimensions("100x145"),
            Some(Dimensions {
                w: 100,
                h: Some(145)
            })
        );
        assert_eq!(
            parse_dimensions("100"),
            Some(Dimensions { w: 100, h: None })
        );
        assert_eq!(parse_dimensions("wide"), None);
    }

    #[test]
    fn embed_viewer_params_are_parsed_and_dropped() {
        assert_eq!(embed("doc.pdf#page=3").fragment, None);
        assert_eq!(embed("doc.pdf#height=400").fragment, None);
        // Only for embeds, and only as the sole component.
        assert!(headings(&link("doc.pdf#page=3")).is_some());
        assert!(headings(&embed("Note#page=3#More")).is_some());
    }

    // --- display labels ----------------------------------------------------

    #[test]
    fn label_joins_fragment_components() {
        assert_eq!(display_label(&link("Note#Heading")), "Note > Heading");
        assert_eq!(display_label(&link("Note")), "Note");
        assert_eq!(display_label(&link("#Heading")), "Heading");
        assert_eq!(display_label(&link("Note#^abc123")), "Note > ^abc123");
    }

    #[test]
    fn alias_wins_for_links_but_not_embeds() {
        assert_eq!(display_label(&link("Note|shown")), "shown");
        // An embed's pipe is dimensions, never a label.
        assert_eq!(display_label(&embed("img.png|100")), "img.png");
    }

    // --- percent coding ----------------------------------------------------

    #[test]
    fn decodes_comraks_clean_url_encoding() {
        assert_eq!(percent_decode("Note%20name#%5Eid"), "Note name#^id");
        // Invalid escapes survive verbatim.
        assert_eq!(percent_decode("100% sure"), "100% sure");
        assert_eq!(percent_decode("a%zz"), "a%zz");
        assert_eq!(percent_decode("trailing%"), "trailing%");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn file_uri_round_trips_through_the_decoder() {
        let path = PathBuf::from("/vault/My Notes/a#b.md");
        let uri = file_uri(&path);
        assert_eq!(uri, "file:///vault/My%20Notes/a%23b.md");
        assert_eq!(
            percent_decode(uri.strip_prefix("file://").unwrap()),
            "/vault/My Notes/a#b.md"
        );
    }

    // --- slugs -------------------------------------------------------------

    #[test]
    fn heading_slug_matches_the_toc_anchorizer() {
        assert_eq!(heading_slug("Getting Started"), "getting-started");
        assert_eq!(heading_slug("Ticks aren't in"), "ticks-arent-in");
    }

    #[test]
    fn duplicate_headings_all_slug_to_the_first() {
        // Obsidian resolves a duplicate heading to the first occurrence, which
        // is the one holding the unsuffixed slug — so a fresh anchorizer per
        // call is the correct behaviour, not a shortcut.
        assert_eq!(heading_slug("Notes"), "notes");
        assert_eq!(heading_slug("Notes"), "notes");
    }
}
