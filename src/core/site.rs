//! Publishing a vault as a static site (DESIGN D15): what gets published, and
//! the navigation around each page.
//!
//! Pure. [`plan`] reads notes only through the closure it is given, as
//! `graph::build` does, so the whole selection is tested over a map; the
//! `jumanji-publish` binary is the imperative shell that reads, renders and
//! writes.
//!
//! **One rule decides what is published: whatever the entry note reaches by
//! links.** The walk follows the links the document graph follows
//! ([`graph::references`]) — wikilinks, note embeds and relative markdown
//! links — transitively, from `index.md` (else `README.md`). An asset is
//! published when a published note links or embeds it. A note nothing links to
//! stays private, so no opt-out list is needed. Every link that cannot be
//! published is reported as a [`DeadLink`], and the page renders it dead.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use super::graph::{self, Link};
use super::sitelink::{self, Url};
use super::vault::{self, Entry, Target, VaultIndex};

/// The entry note's possible names, in precedence order. It is published as
/// the site's `index.html`.
pub const ENTRY_NOTES: [&str; 2] = ["index.md", "README.md"];

/// Why [`plan`] cannot publish a vault.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("no entry note: the folder has neither index.md nor README.md")]
    NoEntryNote,
}

/// What [`plan`] decided: the published notes and assets, the dead links, and
/// the index the pages are rendered against.
#[derive(Debug, Clone)]
pub struct SitePlan {
    /// The published notes in walk order; the entry note is first.
    pub notes: Vec<Note>,
    /// The published assets, vault-relative, sorted.
    pub assets: Vec<PathBuf>,
    /// Every link a published note makes that the site cannot serve, in walk
    /// order.
    pub dead: Vec<DeadLink>,
    /// The vault as the pages see it: the published notes and assets only, so
    /// a link to anything else fails to resolve and renders dead through the
    /// pipeline's ordinary unresolved path.
    pub index: VaultIndex,
}

/// One published note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Vault-relative path of the source.
    pub rel: PathBuf,
    /// Frontmatter `title`, else the first `# H1`, else the file stem.
    pub title: String,
    /// The published notes that link here, as indices into
    /// [`SitePlan::notes`], in walk order.
    pub backlinks: Vec<usize>,
}

/// A link the site cannot serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLink {
    /// Vault-relative path of the note the link is in.
    pub source: PathBuf,
    /// 1-based source line.
    pub line: usize,
    /// The target as written: a URL, or a `[[…]]` target.
    pub written: String,
    pub reason: Dead,
}

/// Why a link is dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dead {
    /// Outside the vault root, or a `file://` URL: never part of the site.
    Outside,
    /// No note or attachment in the vault answers to it.
    NotFound,
}

/// Walk the links from the entry note of the vault at `root` (absolute) and
/// decide what is published. `entries` is the vault scan; `read` returns a
/// note's markdown by absolute path, `None` if it cannot be read (the note is
/// still published, with no links).
pub fn plan(
    root: &Path,
    entries: Vec<Entry>,
    mut read: impl FnMut(&Path) -> Option<String>,
) -> Result<SitePlan, PlanError> {
    let full = VaultIndex::build(root.to_path_buf(), entries.clone());
    let entry = ENTRY_NOTES
        .iter()
        .map(PathBuf::from)
        .find(|name| entries.iter().any(|e| e.rel_path == *name))
        .ok_or(PlanError::NoEntryNote)?;

    let mut notes = vec![Note::new(entry.clone())];
    let mut placed: HashMap<PathBuf, usize> = HashMap::from([(entry, 0)]);
    let mut assets = BTreeSet::new();
    let mut dead = Vec::new();
    // `notes` grows while it is walked: breadth-first, in discovery order.
    let mut at = 0;
    while at < notes.len() {
        let source = root.join(&notes[at].rel);
        let document = graph::references(&read(&source).unwrap_or_default(), &source, &full);
        if let Some(title) = document.title {
            notes[at].title = title;
        }
        for reference in document.references {
            let (written, found) = match reference.link {
                Link::Markdown { url } | Link::Image { url } => {
                    let found = match sitelink::classify(&url, &source, root) {
                        Url::Web => continue,
                        Url::Outside => Err(Dead::Outside),
                        Url::File { rel, .. } => {
                            full.file(&rel).map(Path::to_path_buf).ok_or(Dead::NotFound)
                        }
                    };
                    (url, found)
                }
                Link::Vault { written, target } => match target {
                    Target::Note { path, .. } | Target::Asset { path, .. } => (written, Ok(path)),
                    Target::Unresolved => (written, Err(Dead::NotFound)),
                },
            };
            let path = match found {
                Ok(path) => path,
                Err(reason) => {
                    dead.push(DeadLink {
                        source: notes[at].rel.clone(),
                        line: reference.line,
                        written,
                        reason,
                    });
                    continue;
                }
            };
            // Every path the index hands out lies under the root.
            let Ok(rel) = path.strip_prefix(root).map(Path::to_path_buf) else {
                continue;
            };
            if !vault::is_note(&rel) {
                assets.insert(rel);
                continue;
            }
            let target = *placed.entry(rel.clone()).or_insert_with(|| {
                notes.push(Note::new(rel));
                notes.len() - 1
            });
            let backlinks = &mut notes[target].backlinks;
            if target != at && backlinks.last() != Some(&at) {
                backlinks.push(at);
            }
        }
        at += 1;
    }

    let published: Vec<Entry> = entries
        .into_iter()
        .filter(|e| placed.contains_key(&e.rel_path) || assets.contains(&e.rel_path))
        .collect();
    Ok(SitePlan {
        notes,
        assets: assets.into_iter().collect(),
        dead,
        index: VaultIndex::build(root.to_path_buf(), published),
    })
}

impl Note {
    fn new(rel: PathBuf) -> Self {
        let title = rel
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            rel,
            title,
            backlinks: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::obsidian::{RefKind, parse};

    const ROOT: &str = "/v";

    fn entries(rels: &[&str]) -> Vec<Entry> {
        rels.iter()
            .map(|rel| Entry {
                rel_path: PathBuf::from(rel),
                aliases: Vec::new(),
            })
            .collect()
    }

    /// Plan the vault whose notes are `files` (path, markdown); every other
    /// name in `assets` exists as an attachment.
    fn plan_of(files: &[(&str, &str)], assets: &[&str]) -> SitePlan {
        let mut rels: Vec<&str> = files.iter().map(|(rel, _)| *rel).collect();
        rels.extend(assets);
        let read = |path: &Path| {
            files
                .iter()
                .find(|(rel, _)| Path::new(ROOT).join(rel) == path)
                .map(|(_, md)| md.to_string())
        };
        plan(Path::new(ROOT), entries(&rels), read).unwrap()
    }

    fn rels(plan: &SitePlan) -> Vec<&str> {
        plan.notes.iter().map(|n| n.rel.to_str().unwrap()).collect()
    }

    #[test]
    fn the_entry_note_and_what_it_reaches_are_published_and_nothing_else() {
        let plan = plan_of(
            &[
                ("index.md", "# Home\n[a](topic/a.md) ![fig](fig.svg)\n"),
                ("topic/a.md", "# A\n[[b]] ![[pic.png]]\n"),
                ("topic/b.md", "[home](../index.md)\n"),
                ("CLAUDE.md", "[a](topic/a.md) ![x](secret.png)\n"),
            ],
            &["fig.svg", "pic.png", "secret.png", "unused.pdf"],
        );
        assert_eq!(rels(&plan), ["index.md", "topic/a.md", "topic/b.md"]);
        assert_eq!(
            plan.assets,
            [PathBuf::from("fig.svg"), PathBuf::from("pic.png")]
        );
        assert_eq!(plan.notes[0].title, "Home");
        assert_eq!(plan.notes[2].title, "b", "no heading: the file stem");
        assert!(plan.dead.is_empty(), "{:?}", plan.dead);

        // The render-time index knows the published files and nothing else.
        let resolve = |name: &str| {
            plan.index
                .resolve(&parse(name, RefKind::Link), Path::new("/v/index.md"))
        };
        assert!(matches!(resolve("a"), Target::Note { .. }));
        assert_eq!(resolve("CLAUDE"), Target::Unresolved);
        assert_eq!(resolve("secret.png"), Target::Unresolved);
    }

    #[test]
    fn readme_is_the_entry_note_when_there_is_no_index() {
        let readme = plan_of(&[("README.md", "x\n")], &[]);
        assert_eq!(rels(&readme), ["README.md"]);
        let none = plan(Path::new(ROOT), entries(&["a.md"]), |_| None);
        assert_eq!(none.unwrap_err(), PlanError::NoEntryNote);
    }

    #[test]
    fn links_that_cannot_be_served_are_dead_and_reported_with_their_line() {
        let plan = plan_of(
            &[(
                "index.md",
                "# Home\n\n[paper](../papers/p%20q.pdf)\n[gone](gone.md) [[Nowhere]]\n\
                 [abs](file:///v/a.md) [web](https://x.org) [top](#home)\n",
            )],
            &[],
        );
        let dead: Vec<(usize, &str, Dead)> = plan
            .dead
            .iter()
            .map(|d| (d.line, d.written.as_str(), d.reason))
            .collect();
        assert_eq!(
            dead,
            [
                (3, "../papers/p%20q.pdf", Dead::Outside),
                (4, "gone.md", Dead::NotFound),
                (4, "Nowhere", Dead::NotFound),
                (5, "file:///v/a.md", Dead::Outside),
            ]
        );
        assert!(plan.dead.iter().all(|d| d.source == Path::new("index.md")));
    }

    #[test]
    fn backlinks_are_the_graphs_links_read_backwards() {
        let files = [
            ("index.md", "[a](a.md) [[b]] [a again](a.md)\n"),
            ("a.md", "[b](b.md) [self](a.md)\n"),
            ("b.md", "![[a]] [home](index.md)\n"),
        ];
        let plan = plan_of(&files, &[]);
        let backlinks: Vec<Vec<&str>> = plan
            .notes
            .iter()
            .map(|n| n.backlinks.iter().map(|&i| rels(&plan)[i]).collect())
            .collect();
        assert_eq!(
            backlinks,
            [
                vec!["b.md"],
                vec!["index.md", "b.md"],
                vec!["index.md", "a.md"]
            ]
        );

        // The same edges, forwards, are what the document graph scans.
        let full = VaultIndex::build(PathBuf::from(ROOT), entries(&["index.md", "a.md", "b.md"]));
        for (from, (rel, md)) in files.iter().enumerate() {
            let source = Path::new(ROOT).join(rel);
            let mut graph: Vec<PathBuf> = graph::scan(md, &source, &full).links;
            graph.retain(|p| *p != source);
            let mut site: Vec<PathBuf> = plan
                .notes
                .iter()
                .filter(|n| n.backlinks.contains(&from))
                .map(|n| Path::new(ROOT).join(&n.rel))
                .collect();
            graph.sort();
            site.sort();
            assert_eq!(site, graph, "{rel}");
        }
    }
}
