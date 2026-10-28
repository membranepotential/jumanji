//! `jumanji publish`: render a vault into a static website that looks like
//! the reader (DESIGN D15).
//!
//! A second front end over `core`, beside `shell::gtk`: the same pipeline,
//! no window. It imports no `gtk` / `glib` / `gio` / `webkit6`, and `main`
//! hands over to it before anything initialises GTK, so it runs without a
//! display (over ssh). `build` writes the site.

mod build;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;

/// Publish a folder of notes as a website that looks like jumanji.
///
/// The folder's index.md (else README.md) is the home page. Every note it
/// reaches by links, and every file those notes link or embed, is published;
/// nothing else is.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The folder to publish.
    #[arg(value_name = "DIR")]
    vault: PathBuf,

    /// Build the site into DIR. DIR is replaced as a whole, so it must be
    /// missing, empty, or an earlier build.
    #[arg(short, long, value_name = "DIR")]
    out: PathBuf,
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
    build::site(&args.vault, &args.out)?.print();
    Ok(())
}
