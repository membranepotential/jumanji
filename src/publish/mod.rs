//! `jumanji publish`: render a vault into a static website that looks like
//! the reader and, given `--name`, deploy it to Cloudflare behind a password
//! (DESIGN D15).
//!
//! A second front end over `core`, beside `shell::gtk`: the same pipeline,
//! no window. It imports no `gtk` / `glib` / `gio` / `webkit6`, and `main`
//! hands over to it before anything initialises GTK, so it runs without a
//! display (over ssh). `wrangler` describes the deploy as data; `deploy`
//! carries it out; `build` writes the site.

mod build;
mod deploy;
mod wrangler;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::ArgGroup;

use wrangler::Password;
pub use wrangler::WorkerName;

/// Publish a folder of notes as a website that looks like jumanji, behind a
/// password on Cloudflare.
///
/// The folder's index.md (else README.md) is the home page. Every note it
/// reaches by links, and every file those notes link or embed, is published;
/// nothing else is.
#[derive(Debug, clap::Args)]
#[command(group(ArgGroup::new("target").args(["name", "out"]).required(true).multiple(true)))]
#[command(after_help = "\
Examples:
  jumanji publish ./wiki --name thesis-wiki   build and deploy as the Worker thesis-wiki
  jumanji publish ./wiki --out ./site         build into ./site only, to look at locally

Deploying runs wrangler through pnpm (Arch: pacman -S pnpm nodejs). The first
deploy of a name asks for the password readers will need; later ones do not.")]
pub struct Args {
    /// The folder to publish.
    #[arg(value_name = "DIR")]
    vault: PathBuf,

    /// Deploy the site to Cloudflare as the Worker NAME, served at
    /// https://NAME.<your-subdomain>.workers.dev. Logs in to Cloudflare first
    /// if needed.
    #[arg(long, value_name = "NAME")]
    name: Option<WorkerName>,

    /// Build the site into DIR. Without --name, that is all it does. DIR is
    /// replaced as a whole, so it must be missing, empty, or an earlier
    /// build. [default with --name: $XDG_CACHE_HOME/jumanji/sites/NAME]
    #[arg(short, long, value_name = "DIR")]
    out: Option<PathBuf>,

    /// Ask for a new password, even though the site has one. Every reader
    /// then needs the new one.
    #[arg(long, requires = "name")]
    password: bool,
}

/// Run `jumanji publish`; report a failure on stderr.
pub fn run(args: &Args) -> ExitCode {
    match publish(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jumanji publish: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn publish(args: &Args) -> Result<()> {
    let (name, out) = match (&args.name, &args.out) {
        (Some(name), Some(out)) => (name, out.clone()),
        (Some(name), None) => (name, cache_dir()?.join(name.to_string())),
        (None, Some(out)) => {
            build::site(&args.vault, out)?.print();
            return Ok(());
        }
        (None, None) => unreachable!("the `target` group requires --name or --out"),
    };
    let password = if args.password {
        Password::Replace
    } else {
        Password::KeepIfSet
    };

    // Log in before the build, so a missing pnpm or a login surfaces before
    // the wait, not after it.
    let deploy = deploy::Deploy::prepare(name, &out)?;
    deploy.log_in()?;
    let report = build::site(&args.vault, &out)?;
    // The site goes up before the password: with no password set, the
    // Worker lets nobody in, so nothing is exposed in between.
    let url = deploy.deploy()?;
    deploy.set_password(password, url.as_deref())?;
    report.print();
    match url {
        Some(url) => println!("the site is at {url}"),
        None => println!("the site's URL is in wrangler's output above"),
    }
    Ok(())
}

/// `$XDG_CACHE_HOME/jumanji/sites` (or `$HOME/.cache/jumanji/sites`).
fn cache_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .context("no cache directory (neither XDG_CACHE_HOME nor HOME is set); pass --out")?;
    Ok(base.join("jumanji").join("sites"))
}
