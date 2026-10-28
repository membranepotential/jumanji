//! Publishing a vault as a static site (DESIGN D15): what gets published, and
//! the navigation around each page.
//!
//! Pure. [`plan`] reads notes only through the closure it is given, as
//! `graph::build` does, so the whole selection is tested over a map; [`page`]
//! and [`site_css`] dress a rendered note in the site's navigation; the
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
use super::highlight::escape_html;
use super::obsidian;
use super::pipeline::{LinkStyle, MAIN_CLOSE, MAIN_OPEN, css_font_family, page_path};
use super::sitelink::{self, Url};
use super::vault::{self, Entry, Target, VaultIndex};
use super::{Heading, RenderedDocument};

/// The navigation's stylesheet; [`site_css`] adds what must be generated.
const SITE_CSS: &str = include_str!("assets/site.css");

/// The TOC sidebar's width in px: `--site-toc-width` in `site.css`, which a
/// test keeps equal.
const TOC_WIDTH_PX: u32 = 240;

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
    /// The vault root, absolute: the top of the site.
    pub root: PathBuf,
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
    /// A URL in raw HTML or in a diagram that names no published file. Only
    /// markdown links, images and `[[…]]` publish a file; raw HTML is checked
    /// after rendering, so it can use what they publish but adds nothing.
    Unpublished,
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
        root: root.to_path_buf(),
        notes,
        assets: assets.into_iter().collect(),
        dead,
        index: VaultIndex::build(root.to_path_buf(), published),
    })
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// A font file the site bundles, declared with `@font-face` so a viewer sees
/// the configured fonts without having them installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFace {
    /// The family name the configuration uses (`font-body`, `font-mono`,
    /// `font-ui`).
    pub family: String,
    pub weight: FontWeight,
    pub italic: bool,
    /// Where the file lies in the site, relative to its root.
    pub path: PathBuf,
}

/// The weights one font file covers, in CSS units (100–900).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontWeight {
    /// A static face: one weight. The browser picks the nearest face for any
    /// other weight, as fontconfig does for the reader.
    Single(u16),
    /// A variable font: every weight in the range, drawn at the weight asked
    /// for. Declared as a range, so a 650 heading is drawn at 650 instead of
    /// being matched to a single face (or synthesized bold).
    Range { min: u16, max: u16 },
}

/// The faces the stylesheet asks of each configured family, as (CSS weight,
/// italic): regular, italic, bold and bold italic. `strong` is 680, which
/// the browser matches to the 700 face, as fontconfig does for the reader.
pub const FACES: [(u16, bool); 4] = [(400, false), (400, true), (700, false), (700, true)];

/// The CSS a site page adds after the pipeline's own: an `@font-face` per
/// bundled font, the navigation's stylesheet, and the media query that shows
/// the TOC sidebar. The sidebar needs the margin beside the reading column,
/// so it appears only once the window is the column plus two sidebar widths;
/// from there on, wide blocks keep the sidebar's width from both window
/// edges, which is their gutter mechanism working unchanged (style.css D5a).
///
/// `note` is the page's vault-relative path: font URLs are relative to it.
pub fn site_css(page_width_px: u32, fonts: &[FontFace], note: &Path) -> String {
    let to_root = "../".repeat(note.components().count().saturating_sub(1));
    let faces: String = fonts
        .iter()
        .map(|face| {
            let format = match face.path.extension().and_then(|e| e.to_str()) {
                Some("woff2") => " format(\"woff2\")",
                Some("otf") => " format(\"opentype\")",
                Some("ttf") => " format(\"truetype\")",
                _ => "",
            };
            format!(
                "@font-face {{ font-family: {family}; src: url(\"{to_root}{url}\"){format}; \
                 font-weight: {weight}; font-style: {style}; font-display: swap; }}\n",
                family = css_font_family(&face.family),
                url = obsidian::percent_encode(&face.path.to_string_lossy()),
                weight = match face.weight {
                    FontWeight::Single(weight) => weight.to_string(),
                    FontWeight::Range { min, max } => format!("{min} {max}"),
                },
                style = if face.italic { "italic" } else { "normal" },
            )
        })
        .collect();
    let breakpoint = page_width_px + 2 * TOC_WIDTH_PX;
    format!(
        "{faces}{SITE_CSS}\n@media (min-width: {breakpoint}px) {{\n  .site-toc {{ display: block; }}\n  \
         :root {{ --wide-gutter: calc(var(--site-toc-width) + var(--main-pad-x)); }}\n}}\n"
    )
}

/// One published page, and the URLs that had to come out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub html: String,
    /// What [`sitelink::scrub`] removed: URLs in raw HTML or diagrams that
    /// name nothing the site serves. The markdown-level dead links are in
    /// [`SitePlan::dead`] already.
    pub dead: Vec<DeadLink>,
}

/// The published page for note `at`: its rendering `doc`, its content
/// scrubbed of every URL the site does not serve and of HTML comments
/// ([`sitelink::scrub`]), with the site's navigation spliced in around the
/// content — the TOC sidebar before it, the breadcrumb at its top, the
/// backlinks at its end.
pub fn page(plan: &SitePlan, at: usize, doc: &RenderedDocument) -> Page {
    let note = &plan.notes[at];
    let scrub = |html: &str| {
        sitelink::scrub(html, &plan.root.join(&note.rel), &plan.root, |rel| {
            plan.find(rel)
        })
    };
    let anchors = doc.html.find(MAIN_OPEN).zip(doc.html.rfind(MAIN_CLOSE));
    let (html, removed) = match anchors {
        Some((open, end)) if open < end => {
            let (content, removed) = scrub(&doc.html[open + MAIN_OPEN.len()..end]);
            let html = format!(
                "{head}{toc}{MAIN_OPEN}{crumbs}{content}{backlinks}{tail}",
                head = &doc.html[..open],
                toc = toc(&doc.toc),
                crumbs = crumbs(plan, at),
                backlinks = backlinks(plan, at),
                tail = &doc.html[end..],
            );
            (html, removed)
        }
        // Not a pipeline rendering: nowhere to put the navigation, but the
        // URL invariant still holds.
        _ => scrub(&doc.html),
    };
    let dead = removed
        .into_iter()
        .map(|r| DeadLink {
            source: note.rel.clone(),
            line: r.line,
            written: r.url,
            reason: if r.outside {
                Dead::Outside
            } else {
                Dead::Unpublished
            },
        })
        .collect();
    Page { html, dead }
}

impl SitePlan {
    /// The published file at vault-relative `rel`, as an absolute path: a
    /// page the site writes (`a.html`, the front door `index.html`), a
    /// published note's source, or a published asset.
    fn find(&self, rel: &Path) -> Option<PathBuf> {
        let page =
            rel == Path::new("index.html") || self.notes.iter().any(|n| page_path(&n.rel) == rel);
        if page || self.assets.iter().any(|a| a == rel) {
            return Some(self.root.join(rel));
        }
        self.index.file(rel).map(Path::to_path_buf)
    }
}

/// The href from note `from` to note `to` (both indices into the plan).
fn href(plan: &SitePlan, from: usize, to: usize) -> String {
    LinkStyle::Site {
        root: plan.root.clone(),
    }
    .href(
        &plan.root.join(&plan.notes[to].rel),
        &plan.root.join(&plan.notes[from].rel),
    )
}

/// The TOC sidebar: the page's headings, indented by level below the
/// shallowest. Nothing for a page with fewer than two headings, where a
/// contents list would only repeat the title.
fn toc(headings: &[Heading]) -> String {
    if headings.len() < 2 {
        return String::new();
    }
    let top = headings.iter().map(|h| h.level).min().unwrap_or(1);
    let items: String = headings
        .iter()
        .map(|h| {
            format!(
                "<li class=\"site-toc-{}\"><a href=\"{}\">{}</a></li>\n",
                (h.level - top + 1).min(4),
                escape_html(&h.anchor),
                escape_html(&h.text)
            )
        })
        .collect();
    format!("<nav class=\"site-toc\" aria-label=\"Contents\">\n<ol>\n{items}</ol>\n</nav>\n")
}

/// The breadcrumb: the entry note, then each folder above the note, linked to
/// the folder's own published `index.md` (by its title) where there is one.
/// A folder's index note does not list its own folder, and the entry page
/// has no breadcrumb at all — it would only name itself.
fn crumbs(plan: &SitePlan, at: usize) -> String {
    if at == 0 {
        return String::new();
    }
    let placed = |rel: &Path| plan.notes.iter().position(|n| n.rel == rel);
    let link = |to: usize| {
        format!(
            "<a href=\"{}\">{}</a>",
            href(plan, at, to),
            escape_html(&plan.notes[to].title)
        )
    };
    let rel = &plan.notes[at].rel;
    let own_index = rel.file_name().is_some_and(|n| n == "index.md");
    let mut folders: Vec<&Path> = rel
        .ancestors()
        .skip(if own_index { 2 } else { 1 })
        .filter(|dir| !dir.as_os_str().is_empty())
        .collect();
    folders.reverse();
    let mut parts = vec![link(0)];
    parts.extend(
        folders
            .into_iter()
            .map(|dir| match placed(&dir.join("index.md")) {
                Some(index) if index != 0 => link(index),
                _ => escape_html(&dir.file_name().unwrap_or_default().to_string_lossy()),
            }),
    );
    format!(
        "<nav class=\"site-crumbs\" aria-label=\"Breadcrumb\">{}</nav>\n",
        parts.join("<span class=\"site-sep\">/</span>")
    )
}

/// The notes linking here. Not on the entry page, which every note tends to
/// link back to.
fn backlinks(plan: &SitePlan, at: usize) -> String {
    let from = &plan.notes[at].backlinks;
    if at == 0 || from.is_empty() {
        return String::new();
    }
    let items: String = from
        .iter()
        .map(|&i| {
            format!(
                "<li><a href=\"{}\">{}</a></li>",
                href(plan, at, i),
                escape_html(&plan.notes[i].title)
            )
        })
        .collect();
    format!(
        "<nav class=\"site-backlinks\" aria-label=\"Backlinks\">\n\
         <p class=\"site-label\">Linked from</p>\n<ul>{items}</ul>\n</nav>\n"
    )
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

    // --- pages ---------------------------------------------------------------

    /// A small wiki: a home page, a topic folder with its own index, a note
    /// in it, and a note in a folder without one.
    fn wiki() -> SitePlan {
        plan_of(
            &[
                (
                    "index.md",
                    "# Home\n[t](topic/index.md) [d](deep/er/x.md)\n",
                ),
                ("topic/index.md", "# Topic & co\n[n](n.md)\n"),
                ("topic/n.md", "# N\n[home](../index.md)\n"),
                ("deep/er/x.md", "# X\n[n](../../topic/n.md)\n"),
            ],
            &[],
        )
    }

    fn at(plan: &SitePlan, rel: &str) -> usize {
        rels(plan).iter().position(|r| *r == rel).unwrap()
    }

    fn heading(level: u8, text: &str) -> Heading {
        Heading {
            level,
            text: text.to_string(),
            anchor: format!("#{}", text.to_lowercase()),
        }
    }

    #[test]
    fn the_breadcrumb_names_the_home_page_and_each_folder_above_the_note() {
        let plan = wiki();
        assert_eq!(crumbs(&plan, 0), "", "the home page names only itself");
        assert_eq!(
            crumbs(&plan, at(&plan, "topic/n.md")),
            "<nav class=\"site-crumbs\" aria-label=\"Breadcrumb\"><a href=\"../index.html\">Home</a>\
             <span class=\"site-sep\">/</span><a href=\"index.html\">Topic &amp; co</a></nav>\n"
        );
        // A folder's index does not list its own folder.
        assert!(!crumbs(&plan, at(&plan, "topic/index.md")).contains("Topic"));
        // A folder without a published index is named, not linked.
        let deep = crumbs(&plan, at(&plan, "deep/er/x.md"));
        assert!(
            deep.contains("<a href=\"../../index.html\">Home</a>"),
            "{deep}"
        );
        assert!(
            deep.ends_with(">deep<span class=\"site-sep\">/</span>er</nav>\n"),
            "{deep}"
        );
    }

    #[test]
    fn backlinks_list_the_linking_notes_except_on_the_home_page() {
        let plan = wiki();
        let n = backlinks(&plan, at(&plan, "topic/n.md"));
        assert!(
            n.contains("<li><a href=\"index.html\">Topic &amp; co</a></li>"),
            "{n}"
        );
        assert!(
            n.contains("<li><a href=\"../deep/er/x.html\">X</a></li>"),
            "{n}"
        );
        assert_eq!(backlinks(&plan, 0), "");
    }

    #[test]
    fn the_toc_sidebar_indents_by_level_and_needs_two_headings() {
        assert_eq!(toc(&[heading(1, "Only")]), "");
        let html = toc(&[heading(1, "Title"), heading(2, "A"), heading(3, "B<")]);
        assert!(html.contains("<li class=\"site-toc-1\"><a href=\"#title\">Title</a></li>"));
        assert!(html.contains("<li class=\"site-toc-3\"><a href=\"#b&lt;\">B&lt;</a></li>"));
    }

    #[test]
    fn the_navigation_goes_around_the_content() {
        let plan = wiki();
        let doc = RenderedDocument {
            html: format!("<body>\n{MAIN_OPEN}<h1>N</h1>\n{MAIN_CLOSE}</body>\n"),
            toc: vec![heading(1, "N"), heading(2, "Part")],
        };
        let html = page(&plan, at(&plan, "topic/n.md"), &doc).html;
        let order = [
            "site-toc",
            MAIN_OPEN,
            "site-crumbs",
            "<h1>N</h1>",
            "site-backlinks",
            MAIN_CLOSE,
        ];
        let positions: Vec<usize> = order.iter().map(|s| html.find(s).unwrap()).collect();
        assert!(positions.is_sorted(), "{html}");
    }

    #[test]
    fn site_css_declares_the_fonts_relative_to_the_page() {
        let fonts = [
            FontFace {
                family: "Source Serif 4".to_string(),
                weight: FontWeight::Single(700),
                italic: true,
                path: PathBuf::from("fonts/Serif Bold.woff2"),
            },
            FontFace {
                family: "Adwaita Sans".to_string(),
                weight: FontWeight::Range { min: 100, max: 900 },
                italic: false,
                path: PathBuf::from("fonts/AdwaitaSans-Regular.ttf"),
            },
        ];
        let css = site_css(840, &fonts, Path::new("topic/n.md"));
        assert!(
            css.contains(
                "@font-face { font-family: \"Source Serif 4\"; \
             src: url(\"../fonts/Serif%20Bold.woff2\") format(\"woff2\"); \
             font-weight: 700; font-style: italic; font-display: swap; }"
            ),
            "{css}"
        );
        assert!(
            css.contains(
                "src: url(\"../fonts/AdwaitaSans-Regular.ttf\") format(\"truetype\"); \
             font-weight: 100 900; font-style: normal;"
            ),
            "{css}"
        );
        assert!(site_css(840, &fonts, Path::new("index.md")).contains("url(\"fonts/"));
        assert!(css.contains("@media (min-width: 1320px)"), "{css}");
        assert!(SITE_CSS.contains(&format!("--site-toc-width: {TOC_WIDTH_PX}px;")));
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
