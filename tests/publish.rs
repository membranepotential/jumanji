//! `jumanji-publish` end to end (DESIGN D15): publish a vault into a temp
//! directory, then check the site as a browser would see it. Every `href`,
//! `src` and CSS `url()` in every page must name a file that exists, or a
//! fragment present in that page, and no `file:` URL may appear anywhere.
//!
//! The publisher runs with an empty config directory, so the developer's own
//! configuration (fonts, themes, fence renderers) cannot change the result.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A directory under the system temp dir, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("jumanji-publish-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn publish(vault: &Path, out: &Path, config: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jumanji-publish"))
        .arg(vault)
        .arg("-o")
        .arg(out)
        .env("XDG_CONFIG_HOME", config)
        .output()
        .expect("run jumanji-publish")
}

/// Every file under `dir`, relative to it.
fn files(dir: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(&at).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.insert(path.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    out
}

/// The values of every `attr="…"` in `html`.
fn attributes<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
    let open = format!(" {attr}=\"");
    html.match_indices(&open)
        .filter_map(|(i, _)| {
            let rest = &html[i + open.len()..];
            rest.find('"').map(|end| &rest[..end])
        })
        .collect()
}

/// Every `url("…")` in the page's CSS.
fn css_urls(html: &str) -> Vec<&str> {
    html.match_indices("url(\"")
        .filter_map(|(i, _)| {
            let rest = &html[i + 5..];
            rest.find('"').map(|end| &rest[..end])
        })
        .collect()
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

/// Check every link of every page in the site at `dir`; return the problems.
fn broken_links(dir: &Path) -> Vec<String> {
    let mut broken = Vec::new();
    for page in files(dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
    {
        let html = fs::read_to_string(dir.join(&page)).unwrap();
        if html.contains("file:") && !html.contains("img-src file:") {
            // (The reader's CSP spells `file:`; a site page must not.)
            broken.push(format!("{}: contains a file: URL", page.display()));
        }
        if html.contains("img-src file:") {
            broken.push(format!("{}: carries the reader's CSP", page.display()));
        }
        let ids: BTreeSet<String> = attributes(&html, "id")
            .into_iter()
            .map(str::to_string)
            .collect();
        let links = attributes(&html, "href")
            .into_iter()
            .chain(attributes(&html, "src"))
            .chain(attributes(&html, "xlink:href"))
            .chain(css_urls(&html));
        for link in links {
            let link = link.replace("&amp;", "&");
            if link.starts_with("data:") || link.contains("://") || link.starts_with("mailto:") {
                continue;
            }
            let (path, fragment) = link.split_once('#').unwrap_or((&link, ""));
            if path.is_empty() {
                if !ids.contains(&decode(fragment)) {
                    broken.push(format!("{}: no #{fragment} in the page", page.display()));
                }
                continue;
            }
            let target = page.parent().unwrap().join(decode(path));
            if !dir.join(&target).is_file() {
                broken.push(format!("{}: {link} does not exist", page.display()));
            }
        }
    }
    broken
}

#[test]
fn the_demo_tree_publishes_with_every_link_intact() {
    let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("demo/graph");
    let tmp = Temp::new("demo");
    let out = tmp.0.join("site");
    let result = publish(&demo, &out, &tmp.0.join("config"));
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let site = files(&out);
    assert!(site.contains(Path::new("README.html")));
    assert!(
        site.contains(Path::new("index.html")),
        "README.md is also the front door"
    );
    assert!(site.contains(Path::new("architecture/pipeline.html")));
    assert_eq!(broken_links(&out), Vec::<String>::new());
    // A link to a folder is not a page: reported, and dead in the page.
    assert!(
        stdout.contains("README.md:20: guides/ (not in the vault)"),
        "{stdout}"
    );
}

#[test]
fn only_what_the_entry_note_reaches_is_published() {
    let tmp = Temp::new("vault");
    let vault = tmp.0.join("wiki");
    tmp.write(
        "wiki/index.md",
        "# Wiki\n\n[Topic](topic/index.md) and [[Lone Note]].\n\n\
         The [paper](../papers/A%20B.pdf) lives outside.\n",
    );
    tmp.write(
        "wiki/topic/index.md",
        "# Topic\n\n![fig](fig%201.svg) [home](../index.md#wiki) $e^{i\\pi}=-1$\n\n\
         ```mermaid\nflowchart TD\nA --> B\n```\n\n## Part\n\n[see part](#part)\n",
    );
    tmp.write(
        "wiki/topic/fig 1.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    );
    tmp.write(
        "wiki/Lone Note.md",
        "Back to [the topic](topic/index.md#part).\n",
    );
    tmp.write("wiki/CLAUDE.md", "[home](index.md) ![x](secret.png)\n");
    tmp.write("wiki/secret.png", "png");
    tmp.write("papers/A B.pdf", "%PDF");
    let out = tmp.0.join("site");
    let config = tmp.0.join("config");

    let result = publish(&vault, &out, &config);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        files(&out),
        [
            ".assetsignore",
            ".jumanji-site",
            "Lone Note.html",
            "index.html",
            "topic/fig 1.svg",
            "topic/index.html",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    );
    assert_eq!(broken_links(&out), Vec::<String>::new());
    assert!(
        stdout.contains("index.md:5: ../papers/A%20B.pdf (outside the vault)"),
        "{stdout}"
    );
    assert!(stdout.contains("published 3 notes, 1 assets"), "{stdout}");

    let topic = fs::read_to_string(out.join("topic/index.html")).unwrap();
    assert!(topic.contains("<math"), "math renders");
    assert!(topic.contains("<div class=\"mermaid\""), "mermaid renders");
    assert!(topic.contains("prefers-color-scheme: dark"));
    let index = fs::read_to_string(out.join("index.html")).unwrap();
    assert!(
        !index.contains("papers"),
        "a dead link's URL never reaches the page"
    );

    // Publishing again replaces the earlier output; a foreign folder is safe.
    assert!(publish(&vault, &out, &config).status.success());
    tmp.write("mine/notes.txt", "keep me");
    let refused = publish(&vault, &tmp.0.join("mine"), &config);
    assert!(!refused.status.success());
    assert!(tmp.0.join("mine/notes.txt").exists());
}
