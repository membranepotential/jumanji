//! `jumanji publish --out` end to end (DESIGN D15): publish a vault into a
//! temp directory, then check the site as a browser would see it. Every
//! `href`, `src` and CSS `url()` in every page must name a file that exists,
//! or a fragment present in that page, and no `file:` URL may appear anywhere.
//!
//! The publisher runs with an empty config directory, so the developer's own
//! configuration (fonts, themes, fence renderers) cannot change the result,
//! and with no display, as over ssh: publishing must never need GTK.
//!
//! Nothing here touches the network. The deploy test puts a fake `pnpm` on
//! PATH that plays wrangler's part and logs every call.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use jumanji::core::obsidian::percent_decode;

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
    Command::new(env!("CARGO_BIN_EXE_jumanji"))
        .arg("publish")
        .arg(vault)
        .arg("--out")
        .arg(out)
        .env("XDG_CONFIG_HOME", config)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .expect("run jumanji publish")
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
                if !ids.contains(&percent_decode(fragment)) {
                    broken.push(format!("{}: no #{fragment} in the page", page.display()));
                }
                continue;
            }
            let target = page.parent().unwrap().join(percent_decode(path));
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
        "# Wiki\n\n[Topic](topic/index.md) and [[Lone Note]], [[Hidden Plans|later]].\n\n\
         The [paper](../papers/A%20B.pdf) lives outside.\n\n\
         <a href=\"draft.md\">raw</a> <img src=\"secret.png\" alt=\"s\"> \
         <img src=\"file:///etc/passwd\"> <img src=\"topic/fig%201.svg\" alt=\"ok\">\n\n\
         <!-- hush: an aside for the author -->\n",
    );
    tmp.write(
        "wiki/topic/index.md",
        "# Topic\n\n![fig](fig%201.svg) [home](../index.md#wiki) $e^{i\\pi}=-1$\n\n\
         ```mermaid\nflowchart TD\nA --> B\nclick A \"../draft.md\"\n```\n\n## Part\n\n[see part](#part)\n",
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
    tmp.write("wiki/draft.md", "# Draft\n");
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
    for raw in [
        "index.md:7: draft.md",
        "index.md:7: secret.png",
        "index.md:7: file:///etc/passwd",
    ] {
        assert!(stdout.contains(raw), "{raw} not reported: {stdout}");
    }

    // Nothing unpublished is named anywhere, and no comment survives.
    for page in files(&out)
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
    {
        let html = fs::read_to_string(out.join(page)).unwrap();
        for leak in [
            "draft", "secret", "CLAUDE", "passwd", "papers", "<!--", "hush",
        ] {
            assert!(!html.contains(leak), "{} names {leak}", page.display());
        }
    }

    let topic = fs::read_to_string(out.join("topic/index.html")).unwrap();
    assert!(topic.contains("<math"), "math renders");
    assert!(topic.contains("<div class=\"mermaid\""), "mermaid renders");
    assert!(topic.contains("prefers-color-scheme: dark"));
    let index = fs::read_to_string(out.join("index.html")).unwrap();
    assert!(
        !index.contains("papers"),
        "a dead link's URL never reaches the page"
    );

    // The output may not land inside the vault, however it is spelled.
    std::os::unix::fs::symlink(&vault, tmp.0.join("link")).unwrap();
    for sneaky in [tmp.0.join("link/out"), tmp.0.join("nowhere/../wiki/out")] {
        let refused = publish(&vault, &sneaky, &config);
        assert!(!refused.status.success(), "{}", sneaky.display());
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("lies inside the vault"),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    assert!(!vault.join("out").exists());

    // Publishing again replaces the earlier output; a foreign folder is safe.
    assert!(publish(&vault, &out, &config).status.success());
    tmp.write("mine/notes.txt", "keep me");
    let refused = publish(&vault, &tmp.0.join("mine"), &config);
    assert!(!refused.status.success());
    assert!(tmp.0.join("mine/notes.txt").exists());
}

/// A stand-in for `pnpm --silent dlx wrangler@VERSION …`: it logs the
/// wrangler arguments, answers `whoami` with `$FAKE_WHOAMI`, records the
/// deploy's config and URL as wrangler does, lists the secrets in
/// `$FAKE_SECRETS`, and on `secret put` reads the password from stdin.
const FAKE_PNPM: &str = r#"#!/bin/sh
[ "$1 $2 $3" = "--silent dlx wrangler@4.151.3" ] || exit 99
shift 3
echo "$*" >> "$FAKE_DIR/log"
case "$1 $2" in
  "whoami --json") exit "${FAKE_WHOAMI:-0}" ;;
  "login "*) exit 0 ;;
  "deploy --config")
    cp "$3" "$FAKE_DIR/wrangler.json"
    echo '{"type":"deploy","version":1,"targets":["https://demo.me.workers.dev"]}' \
      >> "$WRANGLER_OUTPUT_FILE_PATH" ;;
  "secret list") cat "$FAKE_DIR/secrets" ;;
  "secret put")
    read -r password && [ "$password" = "hunter2" ] || exit 98
    echo '[{"name":"SITE_PASSWORD","type":"secret_text"}]' > "$FAKE_DIR/secrets" ;;
  *) exit 97 ;;
esac
"#;

#[test]
fn a_deploy_asks_for_the_password_once_and_after_the_upload() {
    let tmp = Temp::new("deploy");
    let fake = tmp.0.join("fake");
    tmp.write("fake/pnpm", FAKE_PNPM);
    fs::set_permissions(fake.join("pnpm"), fs::Permissions::from_mode(0o755)).unwrap();
    tmp.write("fake/secrets", "[]");
    let path = std::env::join_paths(
        std::iter::once(fake.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let deploy = |whoami: &str| {
        let _ = fs::remove_file(fake.join("log"));
        let mut child = Command::new(env!("CARGO_BIN_EXE_jumanji"))
            .args(["publish", "--name", "demo"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("demo/graph"))
            .env("PATH", &path)
            .env("FAKE_DIR", &fake)
            .env("FAKE_WHOAMI", whoami)
            .env("XDG_CONFIG_HOME", tmp.0.join("config"))
            .env("XDG_CACHE_HOME", tmp.0.join("cache"))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"hunter2\n").unwrap();
        let output = child.wait_with_output().unwrap();
        let log = fs::read_to_string(fake.join("log")).unwrap_or_default();
        (output, log)
    };
    let site = tmp.0.join("cache/jumanji/sites/demo");

    // First deploy: logged in, no password yet. The site goes up first, then
    // wrangler asks for the password.
    let (first, log) = deploy("0");
    let stdout = String::from_utf8_lossy(&first.stdout);
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(first.status.success(), "{stdout}{stderr}");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 4, "{log}");
    assert_eq!(lines[0], "whoami --json");
    assert!(lines[1].starts_with("deploy --config /"), "{log}");
    assert!(lines[1].ends_with("/wrangler.json"), "{log}");
    assert_eq!(lines[2], "secret list --name demo --format json");
    assert_eq!(lines[3], "secret put SITE_PASSWORD --name demo");
    assert!(
        stderr.contains("Set the password readers will need for https://demo.me.workers.dev:"),
        "{stderr}"
    );
    assert!(
        stdout.contains("README.md:20: guides/ (not in the vault)"),
        "{stdout}"
    );
    assert!(
        stdout.ends_with("the site is at https://demo.me.workers.dev\n"),
        "{stdout}"
    );
    assert!(site.join("index.html").is_file());
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fake.join("wrangler.json")).unwrap()).unwrap();
    assert_eq!(config["name"], "demo");
    assert_eq!(config["assets"]["directory"], site.to_str().unwrap());
    assert_eq!(config["assets"]["run_worker_first"], true);

    // Again: the password is set, so nobody is asked.
    let (again, log) = deploy("0");
    assert!(again.status.success());
    assert!(!log.contains("secret put"), "{log}");
    assert!(!String::from_utf8_lossy(&again.stderr).contains("Set the password"));

    // Logged out, over ssh: the device login, before anything else.
    let (_, log) = deploy("1");
    assert_eq!(
        log.lines().take(2).collect::<Vec<_>>(),
        ["whoami --json", "login --device"]
    );
}
