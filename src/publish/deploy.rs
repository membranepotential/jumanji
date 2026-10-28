//! Carry out the deploy that `wrangler` describes: run wrangler through
//! `pnpm dlx` as a subprocess, in a fresh directory holding the password
//! script and the generated configuration. jumanji itself opens no sockets
//! and never sees the password: wrangler's own prompt asks for it.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::wrangler::{self, Invocation, Io, Password, WorkerName};

/// A deploy in progress: its directory, removed on drop, and the Worker.
pub struct Deploy {
    dir: PathBuf,
    name: WorkerName,
}

impl Deploy {
    /// Write the script and the configuration for deploying `site` as `name`
    /// into a fresh directory.
    pub fn prepare(name: &WorkerName, site: &Path) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("jumanji-deploy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let deploy = Self {
            dir,
            name: name.clone(),
        };
        let site = std::path::absolute(site)?;
        let config = wrangler::config(name, &site)
            .with_context(|| format!("cannot name {} in the wrangler config", site.display()))?;
        fs::write(deploy.dir.join(wrangler::WORKER_FILE), wrangler::WORKER_JS)?;
        fs::write(deploy.dir.join(wrangler::CONFIG_FILE), config)?;
        Ok(deploy)
    }

    /// Log in to Cloudflare unless wrangler already is.
    pub fn log_in(&self) -> Result<()> {
        let logged_in = self.run(&wrangler::whoami())?.status.success();
        if let Some(login) = wrangler::login(logged_in, graphical()) {
            eprintln!("jumanji publish: logging in to Cloudflare first");
            self.check(&login)?;
        }
        Ok(())
    }

    /// Upload the site and the script; the site's URL, if wrangler named it.
    pub fn deploy(&self) -> Result<Option<String>> {
        self.check(&wrangler::deploy(&self.dir))?;
        let output = fs::read_to_string(self.dir.join(wrangler::OUTPUT_FILE)).unwrap_or_default();
        Ok(wrangler::deployed_url(&output))
    }

    /// Have wrangler ask for the password if the Worker has none yet, or if
    /// `password` asks for a new one. `url` names the site in the question.
    pub fn set_password(&self, password: Password, url: Option<&str>) -> Result<()> {
        let list = self.check(&wrangler::list_secrets(&self.name))?;
        let secrets = String::from_utf8_lossy(&list.stdout);
        let put = wrangler::put_password(&self.name, password, &secrets)
            .context("cannot read wrangler's list of secrets")?;
        if let Some(put) = put {
            let site = url.map_or_else(|| format!("the Worker {}", self.name), str::to_string);
            eprintln!("\nSet the password readers will need for {site}:");
            self.check(&put)?;
        }
        Ok(())
    }

    /// Run `invocation` and fail unless it succeeds.
    fn check(&self, invocation: &Invocation) -> Result<std::process::Output> {
        let output = self.run(invocation)?;
        if !output.status.success() {
            bail!(
                "wrangler {} failed ({})",
                invocation.args[0].to_string_lossy(),
                output.status
            );
        }
        Ok(output)
    }

    fn run(&self, invocation: &Invocation) -> Result<std::process::Output> {
        let mut command = Command::new("pnpm");
        // `--silent` quiets pnpm's own lines (download progress, timing), so
        // standard output is wrangler's alone; wrangler's own still passes.
        command
            .args(["--silent", "dlx"])
            .arg(format!("wrangler@{}", wrangler::VERSION))
            .args(&invocation.args)
            .current_dir(&self.dir);
        if let Some((key, value)) = &invocation.env {
            command.env(key, value);
        }
        let (stdin, stdout, stderr) = match invocation.io {
            Io::Interactive => (Stdio::inherit(), Stdio::inherit(), Stdio::inherit()),
            Io::Silent => (Stdio::null(), Stdio::null(), Stdio::null()),
            Io::Stdout => (Stdio::null(), Stdio::piped(), Stdio::inherit()),
        };
        command.stdin(stdin).stdout(stdout).stderr(stderr);
        match command.output() {
            Err(err) if err.kind() == ErrorKind::NotFound => bail!(
                "deploying needs pnpm, which runs wrangler, and pnpm is not on PATH \
                 (Arch: pacman -S pnpm nodejs)"
            ),
            result => result.context("cannot run pnpm"),
        }
    }
}

impl Drop for Deploy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Whether a browser started here can show itself: an X or Wayland session.
fn graphical() -> bool {
    ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|var| std::env::var_os(var).is_some_and(|v| !v.is_empty()))
}
