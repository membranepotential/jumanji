//! The vault index and link resolution (DESIGN D11).
//!
//! **The vault root is derived from the document you opened** — see
//! [`root_for`]: the nearest ancestor marked `.obsidian/`, else the nearest
//! marked `.git/`, else the document's own directory. One rule, one resolution
//! mode, and no dependence on Obsidian being *installed*: the marker is read as
//! a fact about the directory tree, not as a handshake with another program.
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
//! every resolution rule is unit-tested without a fixture tree. That same split
//! is what lets the shell run [`scan`] on a worker thread and hand the entries
//! back to the main loop (D11).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use super::obsidian::{Fragment, WikiRef, heading_slug};

/// Directory-recursion cap: a pathological tree must not turn the scan into an
/// unbounded one. Deeper directories are simply not indexed.
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
    /// An accepted format with no card of its own — today, only `.base`.
    Other,
}

impl AssetKind {
    /// Classify by file extension (case-insensitive), or `None` for a format
    /// Obsidian does not accept.
    ///
    /// Totality is the point: this list *is* the set of non-note files a vault
    /// contains, so "not an accepted format" is a `None` rather than a kind, and
    /// [`is_indexable`] can be one line over it instead of a second copy of the
    /// same extensions drifting out of sync.
    pub fn classify(path: &Path) -> Option<Self> {
        match extension(path).as_str() {
            "avif" | "bmp" | "gif" | "jpeg" | "jpg" | "png" | "svg" | "webp" => Some(Self::Image),
            "pdf" => Some(Self::Pdf),
            "flac" | "m4a" | "mp3" | "ogg" | "wav" | "webm" | "3gp" | "mkv" | "mov" | "mp4"
            | "ogv" => Some(Self::Av),
            "canvas" => Some(Self::Canvas),
            "base" => Some(Self::Other),
            _ => None,
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

/// Marker directories that identify a vault root, in precedence order: the
/// explicit Obsidian marker first, then the one nearly every marker-less notes
/// tree still has. Both are *directories*; a file of the same name is not a
/// marker.
const ROOT_MARKERS: [&str; 2] = [".obsidian", ".git"];

/// The vault root for `document`: the nearest ancestor holding `.obsidian/`,
/// else the nearest holding `.git/`, else the document's own directory
/// (DESIGN D11).
///
/// Each marker is searched over the *whole* ancestor chain before the next is
/// tried, so an Obsidian vault nested inside a git repo roots at the vault, not
/// the repo — the explicit marker always wins over the incidental one.
///
/// The shell calls this once, for the document it was launched with, and pins
/// the result: recomputing it per document would let following a wikilink into
/// a subfolder silently narrow the vault under the reader's feet.
pub fn root_for(document: &Path) -> PathBuf {
    let document = absolutize(document);
    let start = match document.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        // A bare filename with no parent, or the filesystem root itself.
        _ => Path::new("."),
    };
    for marker in ROOT_MARKERS {
        if let Some(root) = start.ancestors().find(|dir| dir.join(marker).is_dir()) {
            return root.to_path_buf();
        }
    }
    start.to_path_buf()
}

impl Vault {
    /// Bind `source` to an index built elsewhere — in the shell, by the
    /// background scan (D11). The index outlives any one document: the root is
    /// pinned at launch, so switching documents changes only which note "this
    /// one" is.
    pub fn new(index: VaultIndex, source: &Path) -> Self {
        Self {
            source: absolutize(source),
            index,
        }
    }

    /// Index `root` synchronously and bind `source` to it.
    ///
    /// Tests only, and deliberately so: nothing in the running reader may block
    /// the main loop on a directory walk, so the shell scans off-thread and
    /// assembles its vault with [`Vault::new`] (D11). Tests want the walk and
    /// the resolution in one statement, and are welcome to wait.
    #[cfg(test)]
    pub fn rooted(root: &Path, source: &Path) -> Self {
        let root = absolutize(root);
        let entries = scan(&root);
        Self::new(VaultIndex::build(root, entries), source)
    }

    /// Point this vault at a different document, keeping the index.
    pub fn rebind(&mut self, source: &Path) {
        self.source = absolutize(source);
    }

    /// Swap in a freshly-scanned index (a background rescan landed).
    pub fn set_index(&mut self, index: VaultIndex) {
        self.index = index;
    }

    /// The current index, for the equality check that decides whether a landed
    /// rescan is worth re-rendering for.
    pub fn index(&self) -> &VaultIndex {
        &self.index
    }

    /// Resolve a reference against this vault.
    pub fn resolve(&self, r: &WikiRef) -> Target {
        self.index.resolve(r, &self.source)
    }
}

/// Whether `md` could name anything in the vault at all — the gate the shell
/// uses to decide whether a freshly-landed index can change what is on screen.
///
/// Deliberately a substring test, not a parse: `[[` opens both a wikilink and
/// an embed, and every construct that consults the index starts with one. It
/// over-approximates (a `[[` inside a code fence says yes), which costs at most
/// one redundant re-render — never a stale one.
pub fn may_reference_vault(md: &str) -> bool {
    md.contains("[[")
}

/// The case-folded lookup tables for one vault.
///
/// `PartialEq` so the shell can tell a background rescan that changed nothing
/// (the overwhelmingly common case) from one that did, and skip the re-render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultIndex {
    /// How many files were indexed. Not recoverable from the tables below — a
    /// note contributes two keys to each and an asset one — and reported over
    /// D-Bus, where it is the first thing to look at when a `[[…]]` will not
    /// resolve: it says whether the vault jumanji found is the one you meant.
    files: usize,
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
            files: entries.len(),
            by_path,
            by_name,
            aliases,
        }
    }

    /// How many vault files this index covers.
    pub fn file_count(&self) -> usize {
        self.files
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
    if is_note(&path) {
        Target::Note {
            anchor: anchor_for(r),
            path,
        }
    } else {
        Target::Asset {
            // Every resolved path came out of the index, so it is an accepted
            // format; the fallback is for total-function's sake, not a case.
            kind: AssetKind::classify(&path).unwrap_or(AssetKind::Other),
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

/// Walk `root`, returning every *indexable* file with its vault-relative path
/// (and, for a note, its frontmatter aliases).
///
/// Two filters keep the walk proportional to the vault rather than to the tree
/// it happens to sit in — which matters because the `.git/` fallback can root at
/// a source repo:
///
/// - **Ignore files are obeyed** (`ignore`, the crate ripgrep walks with):
///   `.gitignore`, `.ignore`, `.git/info/exclude`, the user's global gitignore,
///   and the same files in parent directories. `require_git` is off, so a
///   `.gitignore` in a marker-less or `.obsidian`-rooted vault is honoured too —
///   the file says "this is not my content" whether or not git is watching.
///   Hidden entries are skipped as before, which is what keeps `.obsidian/`,
///   `.git/` and `.trash/` out.
/// - **Only accepted formats are indexed** ([`is_indexable`]). A build tree's
///   object files and a repo's source cannot be named by any `[[…]]`, so
///   walking into them buys nothing and costs an alias read each.
///
/// Directory symlinks are not followed, and both depth and file count stay
/// capped so a pathological tree cannot hang the scan. Unreadable directories
/// are skipped, not reported: a partial index is strictly better than none.
pub fn scan(root: &Path) -> Vec<Entry> {
    let walk = WalkBuilder::new(root)
        .max_depth(Some(MAX_DEPTH))
        .require_git(false)
        .build();

    let mut entries = Vec::new();
    for item in walk.flatten() {
        if entries.len() >= MAX_FILES {
            break;
        }
        // `None` is the stdin entry, which a rooted walk never yields.
        if !item.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = item.path();
        if !is_indexable(path) {
            continue;
        }
        let Ok(rel_path) = path.strip_prefix(root) else {
            continue;
        };
        let aliases = if is_note(path) {
            parse_aliases(&read_head(path))
        } else {
            Vec::new()
        };
        entries.push(Entry {
            rel_path: rel_path.to_path_buf(),
            aliases,
        });
    }
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    entries
}

/// Whether the vault indexes this file: a note, or one of Obsidian's accepted
/// attachment formats (research §2). Everything else is invisible to `[[…]]`
/// anyway, so indexing it would only make the tables bigger and the walk slower.
pub fn is_indexable(path: &Path) -> bool {
    is_note(path) || AssetKind::classify(path).is_some()
}

/// A note's extension. `.markdown` is not on Obsidian's list, but jumanji opens
/// such files, and a document it can open should be one its links can reach.
fn is_note(path: &Path) -> bool {
    matches!(extension(path).as_str(), "md" | "markdown")
}

/// A path's extension, lowercased; `""` when there is none.
fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
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
            ("a.png", Some(AssetKind::Image)),
            ("a.SVG", Some(AssetKind::Image)),
            ("a.avif", Some(AssetKind::Image)),
            ("a.pdf", Some(AssetKind::Pdf)),
            ("a.mp3", Some(AssetKind::Av)),
            ("a.mp4", Some(AssetKind::Av)),
            ("a.webm", Some(AssetKind::Av)),
            ("a.canvas", Some(AssetKind::Canvas)),
            ("a.base", Some(AssetKind::Other)),
            // Not an accepted format — and so not indexable either.
            ("a.rs", None),
            ("a", None),
        ] {
            assert_eq!(AssetKind::classify(Path::new(name)), kind, "{name}");
        }
    }

    #[test]
    fn only_notes_and_accepted_attachments_are_indexable() {
        for name in ["A.md", "A.MD", "A.markdown", "pic.png", "doc.pdf", "x.base"] {
            assert!(is_indexable(Path::new(name)), "{name}");
        }
        // The kind of file a `.git/`-rooted vault is full of, and that no
        // `[[…]]` could ever name.
        for name in ["main.rs", "lib.o", "Cargo.toml", "README", "a.tar.gz"] {
            assert!(!is_indexable(Path::new(name)), "{name}");
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

    /// The scanned paths, as `/`-joined strings, for assertions.
    fn scanned(tree: &TempTree) -> Vec<String> {
        scan(&tree.0)
            .iter()
            .map(|e| e.rel_path.to_string_lossy().into_owned())
            .collect()
    }

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
    fn scan_indexes_only_accepted_formats() {
        let tree = TempTree::new("scan-formats");
        tree.write("Note.md", "hi\n");
        tree.write("pic.png", "\u{89}PNG");
        // A source tree's worth of files no wikilink could name.
        tree.write("src/main.rs", "fn main() {}\n");
        tree.write("Cargo.toml", "[package]\n");
        tree.write("build/out.o", "\0");
        assert_eq!(scanned(&tree), vec!["Note.md", "pic.png"]);
    }

    #[test]
    fn scan_obeys_gitignore() {
        let tree = TempTree::new("scan-gitignore");
        tree.write(".gitignore", "Drafts/\nSecret.md\n!Drafts/Public.md\n");
        tree.write("Kept.md", "hi\n");
        tree.write("Secret.md", "hi\n");
        tree.write("Drafts/Hidden.md", "hi\n");
        // There is no `.git/` here: an ignore file is a statement about the
        // tree, not a git artefact, so it is honoured either way.
        assert_eq!(scanned(&tree), vec!["Kept.md"]);
    }

    #[test]
    fn scan_obeys_a_plain_ignore_file_too() {
        // The escape hatch for a vault that is not a repo and does not want to
        // pretend to be one.
        let tree = TempTree::new("scan-ignore");
        tree.write(".ignore", "Archive/\n");
        tree.write("Kept.md", "hi\n");
        tree.write("Archive/Old.md", "hi\n");
        assert_eq!(scanned(&tree), vec!["Kept.md"]);
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

    // --- root resolution ----------------------------------------------------

    #[test]
    fn an_obsidian_marker_roots_the_vault_at_its_directory() {
        let tree = TempTree::new("root-obsidian");
        tree.mkdir(".obsidian");
        let deep = tree.write("Concepts/Nested/Deep.md", "hi\n");
        assert_eq!(root_for(&deep), tree.0.canonicalize().unwrap());
    }

    #[test]
    fn a_git_marker_roots_the_vault_when_there_is_no_obsidian_one() {
        let tree = TempTree::new("root-git");
        tree.mkdir(".git");
        let deep = tree.write("notes/Deep.md", "hi\n");
        assert_eq!(root_for(&deep), tree.0.canonicalize().unwrap());
    }

    #[test]
    fn an_obsidian_marker_outranks_a_nearer_git_marker() {
        // Precedence is per *marker*, not per ancestor: `.obsidian` is searched
        // over the whole chain before `.git` is tried at all. A repo checked out
        // inside a vault therefore reads as part of that vault, which is what
        // makes `[[…]]` in it reach the rest of the notes.
        let tree = TempTree::new("root-nested");
        tree.mkdir(".obsidian");
        tree.mkdir("project/.git");
        let doc = tree.write("project/Notes/Deep.md", "hi\n");
        assert_eq!(root_for(&doc), tree.0.canonicalize().unwrap());
    }

    #[test]
    fn a_marker_less_tree_roots_at_the_documents_own_directory() {
        let tree = TempTree::new("root-bare");
        let doc = tree.write("Concepts/Deep.md", "hi\n");
        assert_eq!(
            root_for(&doc),
            tree.0.canonicalize().unwrap().join("Concepts")
        );
    }

    #[test]
    fn a_marker_that_is_a_file_is_not_a_marker() {
        let tree = TempTree::new("root-file-marker");
        tree.write(".obsidian", "not a directory\n");
        // Nested, so a marker wrongly honoured would root at `tree` and be
        // visibly different from the document-directory fallback.
        let doc = tree.write("Notes/Deep.md", "hi\n");
        assert_eq!(root_for(&doc), tree.0.canonicalize().unwrap().join("Notes"));
    }

    #[test]
    fn a_document_outside_the_root_resolves_only_against_the_root() {
        // The accepted consequence of pinning one root (DESIGN D11): a note
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
