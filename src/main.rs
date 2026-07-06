mod core;
mod shell;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use gtk::glib;

use crate::core::config::{self, Config};

/// A zathura-inspired markdown reader.
#[derive(Debug, Parser)]
#[command(name = "jumanji", version, about)]
struct Cli {
    /// The markdown file to open.
    file: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let file = match std::path::absolute(&cli.file) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("jumanji: {}: {err}", cli.file.display());
            return ExitCode::FAILURE;
        }
    };

    if !file.exists() {
        eprintln!("jumanji: {}: no such file", file.display());
        return ExitCode::FAILURE;
    }

    // Malformed config is surfaced but non-fatal: the reader must still open.
    let config = match Config::load(config::xdg_config_dir().as_deref()) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("jumanji: config error, using defaults: {err}");
            Config::default()
        }
    };

    let exit = shell::app::run(file, config);
    if exit == glib::ExitCode::SUCCESS {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
