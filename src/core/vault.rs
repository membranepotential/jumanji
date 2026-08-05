//! The vault index and link resolution (DESIGN D11).
//!
//! **The vault root is the process working directory, always.** jumanji borrows
//! Obsidian's *syntax* and depends on none of its machinery: there is no
//! `.obsidian/` marker to discover, no application to have installed, and no
//! second resolution mode to reason about. A directory of notes is a vault
//! because you opened jumanji in it.
//!
//! Resolution semantics are Obsidian's (`docs/research/04-obsidian.md` §1.2):
//! **vault-wide by filename**, not by path join. The root outranks a sibling
//! folder, matching is case-insensitive, `.md` is optional on notes and
//! mandatory on everything else, and frontmatter `aliases` participate. A
//! reader that treats `[[A]]` as a relative path resolves it differently
//! depending on which note contains it, which is exactly what Obsidian's design
//! rejects.
//!
//! Because resolution is a table lookup over scanned entries and never a path
//! join, a wikilink cannot address anything outside the root — `[[../secrets]]`
//! and `[[/etc/passwd]]` are simply not keys.
//!
//! The directory walk is the only filesystem I/O in the core besides
//! `core::fence`'s subprocesses (D6.2 precedent): `Result`-shaped, no display,
//! and injectable — [`VaultIndex::build`] takes already-scanned entries, so
//! every resolution rule is unit-tested without a fixture tree.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::obsidian::{Fragment, WikiRef, heading_slug};

/// Directory-recursion cap: a pathological (or symlink-looped) tree must not
/// hang the UI thread. Deeper directories are simply not indexed.
const MAX_DEPTH: usize = 32;

/// Cap on indexed files, for the same reason. A vault this large is already
/// past the point where the "measure, don't cache" rule (D11) needs revisiting.
const MAX_FILES: usize = 50_000;

/// How much of a note's head is read looking for `aliases`.
const HEAD_BYTES: u64 = 4096;

/// What a reference resolved to. "Unresolved" is a variant, not a dangling
/// href — the renderer emits a visibly dead link rather than a broken one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A markdown note, with the fragment already translated to an HTML
    /// fragment id (a heading slug, or `^blockid`).
    Note {
        path: PathBuf,
        anchor: Option<String>,
    },
    Asset {
        path: PathBuf,
        kind: AssetKind,
    },
    Unresolved,
}

/// Non-markdown vault files, classified by the accepted-format list in
/// `docs/research/04-obsidian.md` §2. Drives the embed link-card's
/// `data-embed` and nothing else — jumanji plays none of these itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Image,
    Pdf,
    /// Audio or video: one card either way, both handed to the system opener.
    Av,
    Canvas,
    Other,
}

impl AssetKind {
    /// Classify by file extension (case-insensitive).
    pub fn classify(path: &Path) -> Self {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "avif" | "bmp" | "gif" | "jpeg" | "jpg" | "png" | "svg" | "webp" => Self::Image,
            "pdf" => Self::Pdf,
            "flac" | "m4a" | "mp3" | "ogg" | "wav" | "webm" | "3gp" | "mkv" | "mov" | "mp4"
            | "ogv" => Self::Av,
            "canvas" => Self::Canvas,
            _ => Self::Other,
        }
    }

    /// The token emitted as `data-embed` on a link-card.
    pub fn token(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Pdf => "pdf",
            Self::Av => "av",
            Self::Canvas => "canvas",
            Self::Other => "file",
        }
    }
}

/// One scanned vault file: its path relative to the vault root, plus the
/// frontmatter `aliases` of a note (always empty for a non-note).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub rel_path: PathBuf,
    pub aliases: Vec<String>,
}

/// Per-document resolution state: one index, plus the document it was built
/// for (a same-file reference, `[[#H]]`, resolves straight to it). Binding the
/// source here is what keeps `pipeline::render(md, opts, vault)` three-argument.
#[derive(Debug, Clone)]
pub struct Vault {
    /// The document being rendered, absolute.
    source: PathBuf,
    index: VaultIndex,
}

impl Vault {
    /// Index `root` and bind `source` to it. Production always passes the
    /// process working directory as `root`; taking it as an argument keeps the
    /// core free of ambient state and lets tests root a vault anywhere.
    ///
    /// Called on every document load and by `r` — never cached, so the index is
    /// fresh at exactly the moment resolution happens (D11).
    pub fn rooted(root: &Path, source: &Path) -> Self {
        let root = absolutize(root);
        let entries = scan(&root);
        Self {
            source: absolutize(source),
            index: VaultIndex::build(root, entries),
        }
    }

    /// Resolve a reference against this vault.
    pub fn resolve(&self, r: &WikiRef) -> Target {
        self.index.resolve(r, &self.source)
    }
}

/// The case-folded lookup tables for one vault.
#[derive(Debug, Clone)]
pub struct VaultIndex {
    /// Full vault-relative path, with **and** without the `.md` extension.
    by_path: HashMap<String, PathBuf>,
    /// Bare filename (a note's stem *and* its full name; an asset's full
    /// name). Several notes may share one, hence the ranked candidate list.
    by_name: HashMap<String, Vec<PathBuf>>,
    /// Frontmatter aliases. A real file of the same spelling outranks these,
    /// which falls out of the lookup order in [`VaultIndex::resolve`].
    aliases: HashMap<String, PathBuf>,
}

impl VaultIndex {
    /// Build the tables from already-scanned entries. Taking entries rather
    /// than walking is what makes every resolution rule testable without a
    /// fixture tree (D11).
    pub fn build(root: PathBuf, entries: Vec<Entry>) -> Self {
        let mut by_path = HashMap::new();
        let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut aliases = HashMap::new();

        for entry in &entries {
            let path = root.join(&entry.rel_path);
            let rel = key(&entry.rel_path.to_string_lossy());
            by_path.insert(rel.clone(), path.clone());
            if let Some(stem) = rel.strip_suffix(".md") {
                by_path.insert(stem.to_string(), path.clone());
            }
            let name = entry
                .rel_path
                .file_name()
                .map(|n| key(&n.to_string_lossy()))
                .unwrap_or_default();
            by_name.entry(name.clone()).or_default().push(path.clone());
            if let Some(stem) = name.strip_suffix(".md") {
                by_name
                    .entry(stem.to_string())
                    .or_default()
                    .push(path.clone());
            }
            for alias in &entry.aliases {
                aliases.entry(key(alias)).or_insert_with(|| path.clone());
            }
        }
        for candidates in by_name.values_mut() {
            candidates.sort();
        }
        Self {
            by_path,
            by_name,
            aliases,
        }
    }

    /// Resolve `r` against the index; `source` breaks a filename tie. Lookup
    /// order is exact vault-relative path, then bare filename, then alias.
    pub fn resolve(&self, r: &WikiRef, source: &Path) -> Target {
        let Some(note) = r.note.as_deref() else {
            return Target::Note {
                path: source.to_path_buf(),
                anchor: anchor_for(r),
            };
        };
        let wanted = key(note);
        let found = self
            .by_path
            .get(&wanted)
            .cloned()
            .or_else(|| self.by_name.get(&wanted).and_then(|c| best(c, source)))
            .or_else(|| self.aliases.get(&wanted).cloned());
        match found {
            Some(path) => classify(path, r),
            None => Target::Unresolved,
        }
    }
}

/// Pick among same-named notes: the vault root wins, then the source's own
/// directory, then the lexicographically first (candidates are pre-sorted, so
/// this is deterministic). Root-beats-sibling is deliberate Obsidian
/// behaviour — `[[A]]` must mean the same note everywhere (research §1.2).
fn best(candidates: &[PathBuf], source: &Path) -> Option<PathBuf> {
    let source_dir = source.parent();
    let rank = |path: &PathBuf| -> u8 {
        match path.parent() {
            Some(dir) if Some(dir) == source_dir => 1,
            _ => 2,
        }
    };
    // The root is whichever candidate sits shallowest; ranked by depth first so
    // a root file always outranks a sibling one.
    candidates
        .iter()
        .min_by_key(|path| (path.components().count(), rank(path)))
        .cloned()
}

/// A resolved path becomes a `Note` (markdown) or an `Asset`, and a note keeps
/// its translated fragment.
fn classify(path: PathBuf, r: &WikiRef) -> Target {
    let is_md = path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false);
    if is_md {
        Target::Note {
            anchor: anchor_for(r),
            path,
        }
    } else {
        Target::Asset {
            kind: AssetKind::classify(&path),
            path,
        }
    }
}

/// The HTML fragment id a reference's fragment names. Obsidian anchors at the
/// *deepest* component of a nested heading path. The `^` is kept on a block
/// id: heading slugs never contain one, so the two namespaces cannot collide.
fn anchor_for(r: &WikiRef) -> Option<String> {
    match r.fragment.as_ref()? {
        Fragment::Heading(components) => Some(heading_slug(components.last()?)),
        Fragment::Block(id) => Some(format!("^{}", id.as_str())),
    }
}

/// The lookup key: case-folded, `/`-separated.
fn key(s: &str) -> String {
    s.trim_start_matches("./").to_lowercase()
}

/// Make `path` absolute: canonicalized when it exists (so the vault walk and
/// the emitted `file://` URIs agree), else joined onto the CWD.
fn absolutize(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    })
}

/// Walk `root`, returning every file with its vault-relative path (and, for a
/// note, its frontmatter aliases). Dot-directories are skipped wholesale
/// (`.git`, `.trash`, and anything else hidden), directory symlinks are not
/// followed, and both the depth and the file count are capped so a pathological
/// tree cannot hang the UI thread — which matters more now that the root is
/// whatever directory the user happened to launch from. Unreadable directories
/// are skipped, not reported: a partial index is strictly better than none.
pub fn scan(root: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth >= MAX_DEPTH || entries.len() >= MAX_FILES {
            continue;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in read.flatten() {
            let name = item.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            // `DirEntry::file_type` does not follow symlinks, so a symlinked
            // directory is neither descended into nor indexed as a file.
            let Ok(file_type) = item.file_type() else {
                continue;
            };
            let path = item.path();
            if file_type.is_dir() {
                stack.push((path, depth + 1));
            } else if file_type.is_file() {
                if entries.len() >= MAX_FILES {
                    break;
                }
                let Ok(rel_path) = path.strip_prefix(root) else {
                    continue;
                };
                let aliases = if is_note(&path) {
                    parse_aliases(&read_head(&path))
                } else {
                    Vec::new()
                };
                entries.push(Entry {
                    rel_path: rel_path.to_path_buf(),
                    aliases,
                });
            }
        }
    }
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    entries
}

fn is_note(path: &Path) -> bool {
    path.extension()
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

/// The first [`HEAD_BYTES`] of `path`, or `""` if it is unreadable or does not
/// start with a `---` frontmatter fence (in which case there is nothing to
/// parse and the read is not worth finishing).
fn read_head(path: &Path) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut buf = Vec::new();
    if file.take(HEAD_BYTES).read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    if head.starts_with("---") {
        head
    } else {
        String::new()
    }
}

/// Read the `aliases` property out of a YAML frontmatter head.
///
/// A targeted parser, not a YAML crate (D11): one key, in the three shapes
/// Obsidian documents — `aliases: x`, `aliases: [a, b]`, and a `- ` block —
/// plus the singular `alias:` spelling. Anything malformed degrades to no
/// aliases, which costs a link its shortcut and nothing else.
pub fn parse_aliases(head: &str) -> Vec<String> {
    let mut lines = head.lines();
    if !matches!(lines.next(), Some(first) if first.trim_end() == "---") {
        return Vec::new();
    }
    let mut aliases = Vec::new();
    let mut in_block = false;
    for line in lines {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            break;
        }
        if in_block {
            match line.trim_start().strip_prefix("- ") {
                Some(item) => {
                    push_alias(&mut aliases, item);
                    continue;
                }
                // The block ended; a later `aliases:` would be a duplicate key,
                // so this is the whole answer.
                None => break,
            }
        }
        let Some(value) = alias_key_value(trimmed) else {
            continue;
        };
        match value {
            "" => in_block = true,
            list if list.starts_with('[') && list.ends_with(']') => {
                for item in list[1..list.len() - 1].split(',') {
                    push_alias(&mut aliases, item);
                }
                break;
            }
            single => {
                push_alias(&mut aliases, single);
                break;
            }
        }
    }
    aliases
}

/// The value of a top-level `aliases:` / `alias:` line, if this is one.
/// Indented lines are nested under some other key and are not ours.
fn alias_key_value(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("aliases:")
        .or_else(|| line.strip_prefix("alias:"))?;
    Some(rest.trim())
}

fn push_alias(out: &mut Vec<String>, raw: &str) {
    let alias = raw.trim().trim_matches(['"', '\'']).trim();
    if !alias.is_empty() {
        out.push(alias.to_string());
    }
}

/// Throwaway fixture trees for the tests that must touch the filesystem
/// (the vault walk here, resolved-link rendering in `core::wikilink` and
/// `core::textscan`). Kept next to the walk it exercises so the three modules
/// share one implementation.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::PathBuf;

    /// A directory under the system temp dir, removed on drop.
    pub(crate) struct TempTree(pub(crate) PathBuf);

    impl TempTree {
        pub(crate) fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("jumanji-vault-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub(crate) fn write(&self, rel: &str, contents: &str) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }

        pub(crate) fn mkdir(&self, rel: &str) {
            std::fs::create_dir_all(self.0.join(rel)).unwrap();
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TempTree;
    use super::*;
    use crate::core::obsidian::{RefKind, parse};

    fn wiki(raw: &str) -> WikiRef {
        parse(raw, RefKind::Link)
    }

    fn note(rel: &str) -> Entry {
        Entry {
            rel_path: PathBuf::from(rel),
            aliases: Vec::new(),
        }
    }

    fn aliased(rel: &str, aliases: &[&str]) -> Entry {
        Entry {
            rel_path: PathBuf::from(rel),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn index(entries: Vec<Entry>) -> VaultIndex {
        VaultIndex::build(PathBuf::from("/vault"), entries)
    }

    fn resolved_path(target: &Target) -> &Path {
        match target {
            Target::Note { path, .. } | Target::Asset { path, .. } => path,
            Target::Unresolved => panic!("expected a resolved target"),
        }
    }

    // --- index resolution (no fixture tree) --------------------------------

    #[test]
    fn vault_root_beats_a_sibling_folder() {
        // Obsidian's deliberate rule: `[[A]]` means the same note everywhere,
        // so the root file wins even when a sibling of the source matches.
        let index = index(vec![note("A.md"), note("Folder/A.md")]);
        let target = index.resolve(&wiki("A"), Path::new("/vault/Folder/B.md"));
        assert_eq!(resolved_path(&target), Path::new("/vault/A.md"));
    }

    #[test]
    fn source_directory_is_only_a_tiebreaker() {
        // No root candidate: the source's own folder wins over another folder.
        let index = index(vec![note("One/A.md"), note("Two/A.md")]);
        let target = index.resolve(&wiki("A"), Path::new("/vault/Two/B.md"));
        assert_eq!(resolved_path(&target), Path::new("/vault/Two/A.md"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let index = index(vec![note("Note.md")]);
        let target = index.resolve(&wiki("note"), Path::new("/vault/x.md"));
        assert_eq!(resolved_path(&target), Path::new("/vault/Note.md"));
    }

    #[test]
    fn aliases_resolve() {
        let index = index(vec![aliased("Three laws of motion.md", &["The 3 laws"])]);
        let target = index.resolve(&wiki("the 3 LAWS"), Path::new("/vault/x.md"));
        assert_eq!(
            resolved_path(&target),
            Path::new("/vault/Three laws of motion.md")
        );
    }

    #[test]
    fn a_real_file_outranks_an_alias_of_the_same_spelling() {
        let index = index(vec![note("Alpha.md"), aliased("Beta.md", &["Alpha"])]);
        let target = index.resolve(&wiki("Alpha"), Path::new("/vault/x.md"));
        assert_eq!(resolved_path(&target), Path::new("/vault/Alpha.md"));
    }

    #[test]
    fn full_relative_path_form_resolves() {
        let index = index(vec![note("Projects/Note.md"), note("Note.md")]);
        let target = index.resolve(&wiki("Projects/Note"), Path::new("/vault/x.md"));
        assert_eq!(resolved_path(&target), Path::new("/vault/Projects/Note.md"));
    }

    #[test]
    fn md_extension_is_optional_on_notes_and_mandatory_on_assets() {
        let index = index(vec![note("Deep/Note.md"), note("Deep/diagram.png")]);
        let source = Path::new("/vault/x.md");
        // Both spellings find the note, by bare name and by full path.
        for form in ["Note", "Note.md", "Deep/Note", "Deep/Note.md"] {
            assert_eq!(
                resolved_path(&index.resolve(&wiki(form), source)),
                Path::new("/vault/Deep/Note.md"),
                "{form}"
            );
        }
        // An asset needs its extension.
        assert_eq!(
            resolved_path(&index.resolve(&wiki("diagram.png"), source)),
            Path::new("/vault/Deep/diagram.png")
        );
        assert_eq!(index.resolve(&wiki("diagram"), source), Target::Unresolved);
    }

    #[test]
    fn unknown_target_is_unresolved() {
        let index = index(vec![note("A.md")]);
        assert_eq!(
            index.resolve(&wiki("Nowhere"), Path::new("/vault/x.md")),
            Target::Unresolved
        );
    }

    #[test]
    fn fragments_become_html_anchors() {
        let index = index(vec![note("Note.md")]);
        let source = Path::new("/vault/x.md");
        assert_eq!(
            index.resolve(&wiki("Note#Getting Started"), source),
            Target::Note {
                path: PathBuf::from("/vault/Note.md"),
                anchor: Some("getting-started".to_string()),
            }
        );
        // Obsidian anchors at the deepest component of a nested path.
        assert_eq!(
            index.resolve(&wiki("Note#A#Deep One"), source),
            Target::Note {
                path: PathBuf::from("/vault/Note.md"),
                anchor: Some("deep-one".to_string()),
            }
        );
        // The `^` is kept: heading slugs never contain one.
        assert_eq!(
            index.resolve(&wiki("Note#^37066d"), source),
            Target::Note {
                path: PathBuf::from("/vault/Note.md"),
                anchor: Some("^37066d".to_string()),
            }
        );
    }

    #[test]
    fn same_file_reference_targets_the_source() {
        let index = index(vec![note("Note.md")]);
        assert_eq!(
            index.resolve(&wiki("#Heading"), Path::new("/vault/Note.md")),
            Target::Note {
                path: PathBuf::from("/vault/Note.md"),
                anchor: Some("heading".to_string()),
            }
        );
    }

    #[test]
    fn asset_kinds_cover_the_accepted_formats() {
        for (name, kind) in [
            ("a.png", AssetKind::Image),
            ("a.SVG", AssetKind::Image),
            ("a.avif", AssetKind::Image),
            ("a.pdf", AssetKind::Pdf),
            ("a.mp3", AssetKind::Av),
            ("a.mp4", AssetKind::Av),
            ("a.webm", AssetKind::Av),
            ("a.canvas", AssetKind::Canvas),
            ("a.base", AssetKind::Other),
            ("a", AssetKind::Other),
        ] {
            assert_eq!(AssetKind::classify(Path::new(name)), kind, "{name}");
        }
    }

    #[test]
    fn nothing_outside_the_root_is_addressable() {
        // Resolution is a table lookup over scanned entries, never a path join,
        // so an escaping target is simply not a key — no guard needed.
        let index = index(vec![note("A.md")]);
        let source = Path::new("/vault/A.md");
        for escape in ["../secrets", "/etc/passwd", "../../A", "./../A"] {
            assert_eq!(
                index.resolve(&wiki(escape), source),
                Target::Unresolved,
                "{escape}"
            );
        }
    }

    // --- frontmatter aliases -----------------------------------------------

    #[test]
    fn parses_the_three_documented_alias_shapes() {
        assert_eq!(
            parse_aliases("---\naliases: Solo\n---\n"),
            vec!["Solo".to_string()]
        );
        assert_eq!(
            parse_aliases("---\naliases: [a, \"b c\"]\n---\n"),
            vec!["a".to_string(), "b c".to_string()]
        );
        assert_eq!(
            parse_aliases("---\ntitle: x\naliases:\n  - First\n  - 'Second'\n---\nbody\n"),
            vec!["First".to_string(), "Second".to_string()]
        );
        // The singular spelling is accepted too.
        assert_eq!(
            parse_aliases("---\nalias: One\n---\n"),
            vec!["One".to_string()]
        );
    }

    #[test]
    fn absent_or_malformed_frontmatter_yields_no_aliases() {
        assert!(parse_aliases("").is_empty());
        assert!(parse_aliases("# Just a heading\n").is_empty());
        assert!(parse_aliases("---\ntitle: x\n---\n").is_empty());
        // A closing fence stops the scan: `aliases` in the body is not ours.
        assert!(parse_aliases("---\ntitle: x\n---\naliases: nope\n").is_empty());
        // An indented key belongs to some other mapping.
        assert!(parse_aliases("---\nmeta:\n  aliases: nope\n---\n").is_empty());
        assert!(parse_aliases("---\naliases: []\n---\n").is_empty());
    }

    // --- rooting and scanning (temp fixture tree) --------------------------

    #[test]
    fn scan_skips_dot_directories_and_reads_aliases() {
        let tree = TempTree::new("scan");
        tree.mkdir(".git");
        tree.write(".git/config", "[core]\n");
        tree.write("A.md", "---\naliases: [Ay]\n---\nbody\n");
        tree.write("Folder/B.md", "no frontmatter\n");
        tree.write("Folder/pic.png", "\u{89}PNG");

        let entries = scan(&tree.0);
        let paths: Vec<_> = entries
            .iter()
            .map(|e| e.rel_path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, vec!["A.md", "Folder/B.md", "Folder/pic.png"]);
        assert_eq!(entries[0].aliases, vec!["Ay".to_string()]);
        assert!(entries[1].aliases.is_empty());
    }

    #[test]
    fn rooted_indexes_the_whole_root_from_anywhere_inside_it() {
        let tree = TempTree::new("rooted");
        tree.write("Welcome.md", "hi\n");
        tree.write("Notes/sibling.md", "hi\n");
        let deep = tree.write("Concepts/Deep.md", "hi\n");

        // The root is given, not discovered: a document deep inside it still
        // resolves every note in the tree, by bare name and case-insensitively.
        let vault = Vault::rooted(&tree.0, &deep);
        let root = tree.0.canonicalize().unwrap();
        assert_eq!(
            resolved_path(&vault.resolve(&wiki("Welcome"))),
            root.join("Welcome.md")
        );
        assert_eq!(
            resolved_path(&vault.resolve(&wiki("SIBLING"))),
            root.join("Notes/sibling.md")
        );
        assert_eq!(vault.resolve(&wiki("nowhere")), Target::Unresolved);
    }

    #[test]
    fn a_document_outside_the_root_resolves_only_against_the_root() {
        // The accepted consequence of rooting at the CWD (DESIGN D11): a note
        // opened from elsewhere still renders, still links within itself, but
        // its bare wikilinks reach only what the root indexes.
        let root = TempTree::new("root-only");
        root.write("Inside.md", "hi\n");
        let outside = TempTree::new("outside");
        let doc = outside.write("Doc.md", "hi\n");
        outside.write("Neighbour.md", "hi\n");

        let vault = Vault::rooted(&root.0, &doc);
        assert_eq!(
            resolved_path(&vault.resolve(&wiki("Inside"))),
            root.0.canonicalize().unwrap().join("Inside.md")
        );
        // A sibling of the document is *not* in the index.
        assert_eq!(vault.resolve(&wiki("Neighbour")), Target::Unresolved);
        // A same-file reference still works — it never consults the index.
        assert_eq!(
            vault.resolve(&wiki("#Heading")),
            Target::Note {
                path: doc.canonicalize().unwrap(),
                anchor: Some("heading".to_string()),
            }
        );
    }
}
