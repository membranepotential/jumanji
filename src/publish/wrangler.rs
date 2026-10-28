//! The deploy, described as data (DESIGN D15): the wrangler runs a publish
//! needs, the configuration they read, and what to make of their answers.
//! Pure, so every decision is unit-tested without a subprocess; `deploy`
//! carries them out.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The wrangler release every run uses, through `pnpm dlx`. It must be at
/// least 7 days old (the dependency seasoning rule) and at least 4.44.0, so
/// preview URLs follow the `workers_dev` setting.
pub const VERSION: &str = "4.151.3";

/// The Workers runtime version the script is written against.
const COMPATIBILITY_DATE: &str = "2026-10-20";

/// The Worker secret that holds the site's password.
const SECRET: &str = "SITE_PASSWORD";

/// The password script, embedded so an installed binary needs no checkout.
/// `deploy/cloudflare/worker.js` is its source, tested by `worker.test.mjs`.
pub const WORKER_JS: &str = include_str!("../../deploy/cloudflare/worker.js");

/// The file names in the deploy directory.
pub const WORKER_FILE: &str = "worker.js";
pub const CONFIG_FILE: &str = "wrangler.json";
/// Where `deploy` writes its result (wrangler's `WRANGLER_OUTPUT_FILE_PATH`,
/// one JSON object per line), which names the site's URL.
pub const OUTPUT_FILE: &str = "output.ndjson";

/// A Worker name as Cloudflare accepts it: 1 to 63 lowercase letters, digits
/// and dashes, not starting or ending with a dash. It is the first label of
/// the site's `workers.dev` host name and the name of its cache directory,
/// so it can hold no `/` and no `..`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerName(String);

impl FromStr for WorkerName {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if name.is_empty() || name.len() > 63 {
            Err("a Worker name has 1 to 63 characters".into())
        } else if !name.chars().all(allowed) {
            Err("a Worker name has only lowercase letters, digits and dashes".into())
        } else if name.starts_with('-') || name.ends_with('-') {
            Err("a Worker name cannot start or end with a dash".into())
        } else {
            Ok(Self(name.to_string()))
        }
    }
}

impl fmt::Display for WorkerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether to put the password, when the Worker already has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Password {
    /// Ask only on the first deploy, when the Worker has no password yet.
    KeepIfSet,
    /// Ask for a new one (`--password`).
    Replace,
}

/// How a run meets the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Io {
    /// Wrangler owns the terminal: it may prompt, open a browser, ask for a
    /// password without echoing it.
    Interactive,
    /// Nothing in, nothing shown; only the exit status counts.
    Silent,
    /// Nothing in; standard output is read, errors still reach the terminal.
    Stdout,
}

/// One run of `pnpm --silent dlx wrangler@VERSION <args>`, in the deploy
/// directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub args: Vec<OsString>,
    /// An environment variable to set for this run.
    pub env: Option<(&'static str, PathBuf)>,
    pub io: Io,
}

impl Invocation {
    fn new(args: &[&str], io: Io) -> Self {
        Self {
            args: args.iter().map(OsString::from).collect(),
            env: None,
            io,
        }
    }
}

/// `whoami --json`: exits with a non-zero status when not logged in, and
/// never starts a login itself.
pub fn whoami() -> Invocation {
    Invocation::new(&["whoami", "--json"], Io::Silent)
}

/// What to run before building, given whether `whoami` succeeded: nothing,
/// or a login. Without a graphical session (over ssh) the login uses the
/// device flow, since a browser elsewhere cannot reach wrangler's callback
/// on `localhost`.
pub fn login(logged_in: bool, graphical: bool) -> Option<Invocation> {
    match (logged_in, graphical) {
        (true, _) => None,
        (false, true) => Some(Invocation::new(&["login"], Io::Interactive)),
        (false, false) => Some(Invocation::new(&["login", "--device"], Io::Interactive)),
    }
}

/// `deploy`: upload the site and the script as described by the config in
/// `dir`, and record the result in `dir`'s [`OUTPUT_FILE`]. Interactive, as a
/// first deploy may ask to register a `workers.dev` subdomain.
pub fn deploy(dir: &Path) -> Invocation {
    Invocation {
        args: vec![
            "deploy".into(),
            "--config".into(),
            dir.join(CONFIG_FILE).into(),
        ],
        env: Some(("WRANGLER_OUTPUT_FILE_PATH", dir.join(OUTPUT_FILE))),
        io: Io::Interactive,
    }
}

/// `secret list`, as JSON on standard output. Run after the deploy, so the
/// Worker exists and a failure is a real one, not a new Worker.
pub fn list_secrets(name: &WorkerName) -> Invocation {
    Invocation::new(
        &["secret", "list", "--name", &name.0, "--format", "json"],
        Io::Stdout,
    )
}

/// What to run after the deploy, given `secret list`'s output: nothing, or
/// `secret put`, whose own hidden prompt asks the user for the password.
/// jumanji never sees it.
pub fn put_password(
    name: &WorkerName,
    password: Password,
    secrets: &str,
) -> Result<Option<Invocation>, serde_json::Error> {
    #[derive(Deserialize)]
    struct Secret {
        name: String,
    }
    let secrets: Vec<Secret> = serde_json::from_str(secrets)?;
    let set = secrets.iter().any(|s| s.name == SECRET);
    Ok((password == Password::Replace || !set).then(|| {
        Invocation::new(
            &["secret", "put", SECRET, "--name", &name.0],
            Io::Interactive,
        )
    }))
}

/// The wrangler configuration: the script in front of the site in `site`
/// (an absolute path), on the `workers.dev` host name, and every request
/// through the script before any file is served.
pub fn config(name: &WorkerName, site: &Path) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct Config<'a> {
        name: &'a str,
        main: &'a str,
        compatibility_date: &'a str,
        workers_dev: bool,
        assets: Assets<'a>,
    }
    #[derive(Serialize)]
    struct Assets<'a> {
        directory: &'a Path,
        binding: &'a str,
        run_worker_first: bool,
    }
    serde_json::to_string_pretty(&Config {
        name: &name.0,
        main: WORKER_FILE,
        compatibility_date: COMPATIBILITY_DATE,
        workers_dev: true,
        assets: Assets {
            directory: site,
            binding: "ASSETS",
            run_worker_first: true,
        },
    })
}

/// The site's URL from the deploy's [`OUTPUT_FILE`]: the `workers.dev`
/// target of its `deploy` entry (else its first target). Lines that are not
/// such an entry are skipped.
pub fn deployed_url(output: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Entry {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        targets: Vec<String>,
    }
    let targets = output
        .lines()
        .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
        .find(|entry| entry.kind == "deploy")?
        .targets;
    targets
        .iter()
        .find(|t| t.ends_with(".workers.dev"))
        .or(targets.first())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> WorkerName {
        s.parse().unwrap()
    }

    fn args(invocation: &Invocation) -> Vec<&str> {
        invocation
            .args
            .iter()
            .map(|a| a.to_str().unwrap())
            .collect()
    }

    #[test]
    fn a_worker_name_is_a_host_name_label() {
        assert!("sandeep-thesis-wiki".parse::<WorkerName>().is_ok());
        assert!("a".parse::<WorkerName>().is_ok());
        for bad in ["", "Wiki", "my_wiki", "-wiki", "wiki-", "../x", "a b"] {
            assert!(bad.parse::<WorkerName>().is_err(), "{bad:?}");
        }
        assert!("a".repeat(63).parse::<WorkerName>().is_ok());
        assert!("a".repeat(64).parse::<WorkerName>().is_err());
    }

    #[test]
    fn login_runs_only_when_whoami_failed() {
        assert_eq!(login(true, true), None);
        assert_eq!(args(&login(false, true).unwrap()), ["login"]);
        let remote = login(false, false).unwrap();
        assert_eq!(args(&remote), ["login", "--device"]);
        assert_eq!(remote.io, Io::Interactive);
        assert_eq!(whoami().io, Io::Silent);
    }

    #[test]
    fn deploy_reads_the_generated_config_and_records_its_result() {
        let run = deploy(Path::new("/tmp/d"));
        assert_eq!(args(&run), ["deploy", "--config", "/tmp/d/wrangler.json"]);
        assert_eq!(
            run.env,
            Some((
                "WRANGLER_OUTPUT_FILE_PATH",
                PathBuf::from("/tmp/d/output.ndjson")
            ))
        );
        assert_eq!(run.io, Io::Interactive);
    }

    #[test]
    fn the_password_is_asked_on_the_first_deploy_or_on_request() {
        let wiki = name("wiki");
        let none = "[]";
        let set = r#"[{"name": "SITE_PASSWORD", "type": "secret_text"}]"#;
        let put = put_password(&wiki, Password::KeepIfSet, none)
            .unwrap()
            .unwrap();
        assert_eq!(
            args(&put),
            ["secret", "put", "SITE_PASSWORD", "--name", "wiki"]
        );
        assert_eq!(put.io, Io::Interactive);
        assert_eq!(put_password(&wiki, Password::KeepIfSet, set).unwrap(), None);
        assert!(
            put_password(&wiki, Password::Replace, set)
                .unwrap()
                .is_some()
        );
        assert!(put_password(&wiki, Password::KeepIfSet, "not json").is_err());
        assert_eq!(
            args(&list_secrets(&wiki)),
            ["secret", "list", "--name", "wiki", "--format", "json"]
        );
    }

    #[test]
    fn the_config_puts_the_script_in_front_of_the_site() {
        let json = config(
            &name("wiki"),
            Path::new("/home/x/.cache/jumanji/sites/wiki"),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "name": "wiki",
                "main": "worker.js",
                "compatibility_date": "2026-10-20",
                "workers_dev": true,
                "assets": {
                    "directory": "/home/x/.cache/jumanji/sites/wiki",
                    "binding": "ASSETS",
                    "run_worker_first": true
                }
            })
        );
    }

    #[test]
    fn the_url_is_the_deploy_entrys_workers_dev_target() {
        let output = concat!(
            r#"{"type":"wrangler-session","version":1}"#,
            "\n",
            r#"{"type":"deploy","version":1,"worker_name":"wiki","targets":["wiki.example.com/*","https://wiki.me.workers.dev"]}"#,
            "\n",
        );
        assert_eq!(
            deployed_url(output).as_deref(),
            Some("https://wiki.me.workers.dev")
        );
        assert_eq!(deployed_url(r#"{"type":"deploy","targets":[]}"#), None);
        assert_eq!(deployed_url(""), None);
    }
}
