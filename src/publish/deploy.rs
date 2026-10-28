//! Carry out the deploy that `wrangler` describes: run wrangler through
//! `pnpm dlx` as a subprocess, in a directory of its own holding the
//! password script and the generated configuration. jumanji itself opens no
//! sockets and never sees the password: wrangler's own prompt asks for it.

use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use super::wrangler::{self, Invocation, Io, Password, Session, WorkerName};

/// A deploy in progress: its directory, removed on drop, and the Worker.
pub struct Deploy {
    dir: PathBuf,
    name: WorkerName,
}

impl Deploy {
    /// Write the script and the configuration for deploying `site` as `name`
    /// into a new directory under `runs` (`$XDG_CACHE_HOME/jumanji/deploy`).
    ///
    /// The directory is the user's alone: `runs` lies in their own cache, not
    /// in a shared temp directory where another user could plant files in it
    /// first or a `pnpm-workspace.yaml` above it (pnpm and wrangler search
    /// the parent directories). It is created mode 0700, under a new name,
    /// and creating fails if that name exists.
    pub fn prepare(name: &WorkerName, site: &Path, runs: &Path) -> Result<Self> {
        if let Some(parent) = runs.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        match DirBuilder::new().mode(0o700).create(runs) {
            Err(err) if err.kind() != ErrorKind::AlreadyExists => {
                return Err(err).with_context(|| format!("cannot create {}", runs.display()));
            }
            _ => {}
        }
        // Also when an earlier tool made it with a wider mode.
        fs::set_permissions(runs, fs::Permissions::from_mode(0o700))?;
        remove_stale(runs);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = runs.join(format!("{}-{nanos}", std::process::id()));
        DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
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

    /// Log in to Cloudflare unless wrangler already is. A `whoami` that
    /// fails for another reason (offline, pnpm unable to fetch wrangler) is
    /// an error, not a reason to log in.
    pub fn log_in(&self) -> Result<()> {
        let whoami = wrangler::whoami();
        let output = self.run(&whoami)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(session) = wrangler::session(output.status.success(), &stdout) else {
            bail!(
                "{} failed ({}): {}",
                whoami.step,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        };
        if session == Session::LoggedOut {
            eprintln!("jumanji publish: not logged in to Cloudflare; logging in first");
        }
        if let Some(login) = wrangler::login(session, graphical()) {
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
    /// `password` asks for a new one. `url` names the site in the question;
    /// `retry` is the command that sets the password, for the error.
    pub fn set_password(&self, password: Password, url: Option<&str>, retry: &str) -> Result<()> {
        let list = wrangler::list_secrets(&self.name);
        let has = self
            .check(&list)
            .and_then(|out| {
                wrangler::has_password(&String::from_utf8_lossy(&out.stdout))
                    .context("cannot read wrangler's list of secrets")
            })
            .with_context(|| {
                format!(
                    "the site is deployed, but whether it has a password is unknown; \
                     without one it lets nobody in. Run `{retry}` to set one"
                )
            })?;
        let Some(put) = wrangler::put_password(&self.name, password, has) else {
            return Ok(());
        };
        let site = url.map_or_else(|| format!("the Worker {}", self.name), str::to_string);
        eprintln!("\nSet the password readers will need for {site}:");
        self.check(&put).with_context(|| {
            if has {
                "the site is deployed; its old password still holds".to_string()
            } else {
                format!(
                    "the site is deployed but locked (no password set). \
                     Run `{retry}` to set it"
                )
            }
        })?;
        Ok(())
    }

    /// Run `invocation` and fail unless it succeeds.
    fn check(&self, invocation: &Invocation) -> Result<Output> {
        let output = self.run(invocation)?;
        if !output.status.success() {
            bail!("{} failed ({})", invocation.step, output.status);
        }
        Ok(output)
    }

    fn run(&self, invocation: &Invocation) -> Result<Output> {
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
            Io::Captured => (Stdio::null(), Stdio::piped(), Stdio::piped()),
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

/// Remove the directories of earlier deploys whose process is gone. `Drop`
/// does not run when Ctrl-C at a wrangler prompt kills the whole process
/// group, so they are cleaned up here, by the next deploy. A directory is
/// named `<pid>-<nanos>`; one whose pid still runs (`/proc/<pid>`) may be a
/// concurrent deploy and stays.
fn remove_stale(runs: &Path) {
    for entry in fs::read_dir(runs).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.split_once('-'))
            .and_then(|(pid, _)| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if !Path::new("/proc").join(pid.to_string()).exists() {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether a browser started here can show itself: an X or Wayland session.
fn graphical() -> bool {
    ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|var| std::env::var_os(var).is_some_and(|v| !v.is_empty()))
}
