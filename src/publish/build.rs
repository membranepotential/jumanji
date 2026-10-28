//! Build the site: render a vault into a static website that looks like the
//! reader (DESIGN D15).
//!
//! The imperative shell around `core::site`: it reads the configuration and
//! the vault, renders every published note in parallel, bundles the
//! configured fonts (found through fontconfig), copies the assets and swaps
//! the result into place. Deciding what is published, and every byte of every
//! page, is the core's. No network: deploying is `deploy`'s.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;

use crate::core::config::{self, Config};
use crate::core::pipeline::{self, LinkStyle, Options, page_path};
use crate::core::site::{self, Dead, DeadLink, FACES, FontFace, FontWeight, SitePlan};
use crate::core::vault::{self, Vault};

/// Marks a directory as this tool's output, so it may be replaced.
const MARKER: &str = ".jumanji-site";

/// Wrangler skips the files this lists when it uploads the site.
const ASSETS_IGNORE: &str = ".assetsignore";

/// Where the bundled fonts go, relative to the site root.
const FONTS_DIR: &str = "fonts";

/// Build the site for the vault at `vault` into `out`, replacing an earlier
/// build there. What was published, for the report.
pub fn site(vault: &Path, out: &Path) -> Result<Report> {
    let root = vault
        .canonicalize()
        .with_context(|| format!("cannot open the vault {}", vault.display()))?;
    let out = real_path(out)?;
    check_output(&out, &root)?;

    let config_dir = config::xdg_config_dir();
    let config = Config::load(config_dir.as_deref()).unwrap_or_else(|err| {
        eprintln!("warning: {err}; publishing with the default configuration");
        Config::default()
    });

    // Every note is read once, during the walk, and rendered from that copy.
    let mut sources: HashMap<PathBuf, String> = HashMap::new();
    let plan = site::plan(&root, vault::scan(&root), |path| {
        let md = read_note(path)?;
        sources.insert(path.to_path_buf(), md.clone());
        Some(md)
    })?;

    let parent = out.parent().context("the output directory has no parent")?;
    fs::create_dir_all(parent)?;
    let staging = sibling(&out, "new");
    remove_dir(&staging)?;
    fs::create_dir(&staging)?;
    let built = match build(&plan, &sources, &config, config_dir.as_deref(), &staging) {
        Ok(built) => built,
        Err(err) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(err);
        }
    };
    if let Err(err) = swap(&staging, &out) {
        let _ = fs::remove_dir_all(&staging);
        return Err(err);
    }
    Ok(Report {
        dead: plan.dead.into_iter().chain(built.dead).collect(),
        notes: plan.notes.len(),
        assets: plan.assets.len(),
        fonts: built.fonts.len(),
        bytes: size(&out),
        out,
    })
}

/// Where `path` really is: absolute, every `..` and symlink of its existing
/// part resolved by the filesystem, and the part that does not exist yet
/// resolved lexically. The output checks compare this with the canonical
/// vault root, so `-o wiki/../wiki/out` or `-o link-to-wiki/out` cannot slip
/// past them.
fn real_path(path: &Path) -> Result<PathBuf> {
    let mut real = PathBuf::new();
    for part in std::path::absolute(path)?.components() {
        let next = match part {
            Component::ParentDir => real.parent().unwrap_or(&real).to_path_buf(),
            Component::CurDir => continue,
            part => real.join(part),
        };
        // Once a component does not exist, nothing after it can, and no
        // symlink is left to follow.
        real = if next.exists() {
            next.canonicalize()?
        } else {
            next
        };
    }
    Ok(real)
}

/// Refuse an output directory whose replacement could lose something: one
/// that holds files but no marker, or one that is (or holds) the vault, or
/// one inside the vault (the next publish would scan its own output).
fn check_output(out: &Path, root: &Path) -> Result<()> {
    if root.starts_with(out) {
        bail!("the output {} contains the vault", out.display());
    }
    if out.starts_with(root) {
        bail!("the output {} lies inside the vault", out.display());
    }
    if !out.exists() {
        return Ok(());
    }
    if !out.is_dir() {
        bail!("the output {} is not a directory", out.display());
    }
    let empty = fs::read_dir(out)?.next().is_none();
    if !empty && !out.join(MARKER).exists() {
        bail!(
            "refusing to replace {}: it is not empty and has no {MARKER} marker, \
             so it is not an earlier jumanji publish output",
            out.display()
        );
    }
    Ok(())
}

/// What [`build`] wrote besides the pages, for the report.
struct Built {
    fonts: Vec<FontFace>,
    /// The URLs the pages' scrub removed, page by page.
    dead: Vec<DeadLink>,
}

/// Write the whole site into `dir`.
fn build(
    plan: &SitePlan,
    sources: &HashMap<PathBuf, String>,
    config: &Config,
    config_dir: Option<&Path>,
    dir: &Path,
) -> Result<Built> {
    let fonts = bundle_fonts(&config.options, dir)?;
    let options = Options {
        links: LinkStyle::Site {
            root: plan.root.clone(),
        },
        ..Options::from(&config.options)
    };
    // Each theme follows the viewer's colour scheme as it follows the
    // reader's `dark` class.
    let themes: Vec<String> = config::load_themes(config_dir)
        .iter()
        .map(|css| pipeline::site_theme(css))
        .collect();

    let pages: Vec<Vec<DeadLink>> = plan
        .notes
        .par_iter()
        .enumerate()
        .map(|(at, note)| -> Result<Vec<DeadLink>> {
            let source = plan.root.join(&note.rel);
            let md = sources.get(&source).map_or("", String::as_str);
            // The site's own CSS goes before the user's themes, so a theme
            // still wins the cascade as it does in the reader.
            let options = Options {
                extra_css: std::iter::once(site::site_css(
                    options.page_width_px,
                    &fonts,
                    &note.rel,
                ))
                .chain(themes.iter().cloned())
                .collect(),
                ..options.clone()
            };
            let doc = pipeline::render(md, &options, &Vault::new(plan.index.clone(), &source));
            let page = site::page(plan, at, &doc);
            write(&dir.join(page_path(&note.rel)), page.html)?;
            Ok(page.dead)
        })
        .collect::<Result<_>>()?;

    // A `README.md` entry note is also the site's front door.
    let entry = page_path(&plan.notes[0].rel);
    if entry != Path::new("index.html") {
        fs::copy(dir.join(&entry), dir.join("index.html"))?;
    }
    for asset in &plan.assets {
        let to = dir.join(asset);
        fs::create_dir_all(to.parent().unwrap_or(dir))?;
        fs::copy(plan.root.join(asset), &to)
            .with_context(|| format!("cannot copy {}", asset.display()))?;
    }
    write(&dir.join(MARKER), String::new())?;
    write(
        &dir.join(ASSETS_IGNORE),
        format!("{MARKER}\n{ASSETS_IGNORE}\n"),
    )?;
    Ok(Built {
        fonts,
        dead: pages.into_iter().flatten().collect(),
    })
}

/// Copy the configured body, code and interface fonts into the site: the
/// files that hold the faces in [`FACES`] the family really has, each once,
/// converted to WOFF2 when `woff2_compress` is installed. A variable font is
/// one file for several of those faces and is declared with its weight range.
/// A family fontconfig does not know is skipped with a warning: the page then
/// falls back to the stylesheet's font stack, as the reader would.
fn bundle_fonts(options: &config::Options, dir: &Path) -> Result<Vec<FontFace>> {
    if options.font_ui.trim().is_empty() {
        eprintln!(
            "hint: font-ui is not set, so headings and other interface text use each \
             viewer's own system font; set it to bundle one"
        );
    }
    let mut faces: Vec<(FontFace, PathBuf)> = Vec::new();
    for (option, family) in [
        ("font-body", &options.font_body),
        ("font-mono", &options.font_mono),
        ("font-ui", &options.font_ui),
    ] {
        let family = family.trim();
        if family.is_empty() {
            continue;
        }
        let mut found: Vec<(u16, bool, PathBuf)> = Vec::new();
        for &(weight, italic) in &FACES {
            match font_file(family, weight, italic) {
                Some(file) if !found.iter().any(|(_, _, f)| *f == file) => {
                    found.push((weight, italic, file))
                }
                _ => {}
            }
        }
        if found.is_empty() {
            eprintln!(
                "warning: {option} \"{family}\" is not installed (fontconfig); \
                 the site falls back to the stylesheet's font stack"
            );
        }
        for (weight, italic, file) in found {
            let name = file.file_name().context("a font path without a name")?;
            let face = FontFace {
                family: family.to_string(),
                weight: variable_weights(&file).unwrap_or(FontWeight::Single(weight)),
                italic,
                path: Path::new(FONTS_DIR).join(name),
            };
            faces.push((face, file));
        }
    }
    if faces.is_empty() {
        return Ok(Vec::new());
    }
    fs::create_dir_all(dir.join(FONTS_DIR))?;
    faces
        .into_par_iter()
        .map(|(face, file)| {
            let copy = dir.join(&face.path);
            fs::copy(&file, &copy).with_context(|| format!("cannot copy {}", file.display()))?;
            Ok(FontFace {
                path: woff2(&copy).map_or(face.path.clone(), |woff2| {
                    face.path
                        .with_file_name(woff2.file_name().unwrap_or_default())
                }),
                ..face
            })
        })
        .collect()
}

/// The weight range of `file` if it is a variable font, read with `fc-scan`
/// (fontconfig lists a variable font once with its range, `[0 210]`, besides
/// its named instances).
fn variable_weights(file: &Path) -> Option<FontWeight> {
    let output = Command::new("fc-scan")
        .arg("--format=%{variable}\t%{weight}\n")
        .arg(file)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .find_map(|line| fc_weight_range(line.strip_prefix("True\t")?))
}

/// A fontconfig weight range, `[0 210]`, in CSS weights.
fn fc_weight_range(range: &str) -> Option<FontWeight> {
    let (min, max) = range
        .strip_prefix('[')?
        .strip_suffix(']')?
        .split_once(' ')?;
    Some(FontWeight::Range {
        min: css_weight_of(min.parse().ok()?),
        max: css_weight_of(max.parse().ok()?),
    })
}

/// The file fontconfig holds for `family` at CSS `weight` and `italic`, if
/// the family has that face itself. fontconfig answers every query with its
/// closest match, so the answer is checked: another family, a regular face
/// for an italic, or a regular weight for a bold would make the browser show
/// the wrong face instead of synthesizing the right one.
fn font_file(family: &str, weight: u16, italic: bool) -> Option<PathBuf> {
    let output = Command::new("fc-match")
        .arg("--format=%{file}\n%{family}\n%{slant}\n%{weight}\n")
        .arg(fc_pattern(family, weight, italic))
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines = text.lines();
    let (file, families, slant, fc_weight) =
        (lines.next()?, lines.next()?, lines.next()?, lines.next()?);
    let same_family = families
        .split(',')
        .any(|name| name.trim().eq_ignore_ascii_case(family));
    let same_slant = (slant.parse::<u32>().ok()? > 0) == italic;
    let same_weight = fc_weight
        .parse::<i32>()
        .ok()?
        .abs_diff(fc_weight_of(weight))
        <= 20;
    (same_family && same_slant && same_weight).then(|| PathBuf::from(file))
}

/// A fontconfig pattern for one face. `-`, `:`, `,` and `\` are syntax in a
/// family name, so they are escaped.
fn fc_pattern(family: &str, weight: u16, italic: bool) -> String {
    let escaped: String = family
        .chars()
        .flat_map(|c| match c {
            '-' | ':' | ',' | '\\' => vec!['\\', c],
            c => vec![c],
        })
        .collect();
    let slant = if italic { 100 } else { 0 };
    format!("{escaped}:weight={}:slant={slant}", fc_weight_of(weight))
}

/// CSS weights and fontconfig's (`FC_WEIGHT_*`), thin to black.
const WEIGHTS: [(u16, i32); 9] = [
    (100, 0),
    (200, 40),
    (300, 50),
    (400, 80),
    (500, 100),
    (600, 180),
    (700, 200),
    (800, 205),
    (900, 210),
];

/// fontconfig's weight for a CSS weight: the named weight nearest to it.
fn fc_weight_of(css: u16) -> i32 {
    WEIGHTS
        .iter()
        .min_by_key(|(c, _)| c.abs_diff(css))
        .map_or(80, |&(_, fc)| fc)
}

/// The CSS weight for a fontconfig weight (which `fc-scan` may print as a
/// fraction): the named weight nearest to it.
fn css_weight_of(fc: f64) -> u16 {
    WEIGHTS
        .iter()
        .min_by(|(_, a), (_, b)| {
            (f64::from(*a) - fc)
                .abs()
                .total_cmp(&(f64::from(*b) - fc).abs())
        })
        .map_or(400, |&(css, _)| css)
}

/// Compress `font` to WOFF2 next to it with `woff2_compress`, and remove the
/// original. `None` (the original stays) when the tool is missing or fails.
fn woff2(font: &Path) -> Option<PathBuf> {
    let target = font.with_extension("woff2");
    let done = Command::new("woff2_compress")
        .arg(font)
        .output()
        .is_ok_and(|o| o.status.success())
        && target.exists();
    if !done {
        return None;
    }
    fs::remove_file(font).ok()?;
    Some(target)
}

/// Replace `out` with `staging`. Two renames, so a reader of `out` sees the
/// old site or the new one, never half of either, except for the instant
/// between them.
fn swap(staging: &Path, out: &Path) -> Result<()> {
    if !out.exists() {
        return Ok(fs::rename(staging, out)?);
    }
    let old = sibling(out, "old");
    remove_dir(&old)?;
    fs::rename(out, &old)?;
    if let Err(err) = fs::rename(staging, out) {
        let _ = fs::rename(&old, out);
        return Err(err.into());
    }
    Ok(fs::remove_dir_all(&old)?)
}

/// A hidden directory next to `out`, named after it, for staging a swap.
fn sibling(out: &Path, tag: &str) -> PathBuf {
    let name = out.file_name().unwrap_or_default().to_string_lossy();
    out.with_file_name(format!(".{name}.{tag}-{}", std::process::id()))
}

fn remove_dir(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
        _ => Ok(()),
    }
}

fn read_note(path: &Path) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(err) => {
            eprintln!("warning: cannot read {}: {err}", path.display());
            None
        }
    }
}

fn write(path: &Path, contents: String) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents).with_context(|| format!("cannot write {}", path.display()))
}

/// What a build published: the dead links and a summary.
pub struct Report {
    dead: Vec<DeadLink>,
    notes: usize,
    assets: usize,
    fonts: usize,
    bytes: u64,
    out: PathBuf,
}

impl Report {
    /// Print every dead link and the summary on stdout.
    pub fn print(&self) {
        if !self.dead.is_empty() {
            println!("dead links ({}):", self.dead.len());
            for dead in &self.dead {
                let why = match dead.reason {
                    Dead::Outside => "outside the vault",
                    Dead::NotFound => "not in the vault",
                    Dead::Unpublished => "raw HTML or a diagram naming no published file",
                };
                println!(
                    "  {}:{}: {} ({why})",
                    dead.source.display(),
                    dead.line,
                    dead.written
                );
            }
        }
        println!(
            "published {} notes, {} assets and {} font files ({:.1} MB) to {}",
            self.notes,
            self.assets,
            self.fonts,
            self.bytes as f64 / 1_000_000.0,
            self.out.display()
        );
    }
}

/// The total size of the files under `dir`, in bytes.
fn size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => size(&entry.path()),
            _ => entry.metadata().map_or(0, |m| m.len()),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fontconfig_pattern_escapes_the_family_and_maps_the_weight() {
        assert_eq!(
            fc_pattern("Source Serif 4", 400, false),
            "Source Serif 4:weight=80:slant=0"
        );
        assert_eq!(
            fc_pattern("A-B:C", 700, true),
            "A\\-B\\:C:weight=200:slant=100"
        );
    }

    #[test]
    fn a_variable_fonts_range_is_read_in_css_weights() {
        assert_eq!(
            fc_weight_range("[0 210]"),
            Some(FontWeight::Range { min: 100, max: 900 })
        );
        assert_eq!(
            fc_weight_range("[50 200]"),
            Some(FontWeight::Range { min: 300, max: 700 })
        );
        assert_eq!(fc_weight_range("80"), None);
    }

    #[test]
    fn the_output_may_not_hold_or_sit_inside_the_vault() {
        let root = Path::new("/home/x/wiki");
        assert!(check_output(Path::new("/home/x"), root).is_err());
        assert!(check_output(root, root).is_err());
        assert!(check_output(&root.join("site"), root).is_err());
        assert!(check_output(Path::new("/nonexistent-jumanji/site"), root).is_ok());
    }
}
