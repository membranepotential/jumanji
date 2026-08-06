//! Headless end-to-end tests for jumanji.
//!
//! These drive the *real* application — a real (virtual) X server, real GTK key
//! events, real WebKit — and assert on state read back over the per-instance
//! D-Bus interface (`src/shell/dbus.rs`). Nothing here touches the developer's
//! live desktop: each test spins up its own `Xvfb` display and its own private
//! session bus, and tears them down (even on panic) via RAII.
//!
//! If `Xvfb`, `xdotool`, or `dbus-daemon` are missing the whole suite skips
//! (prints a notice and passes), so machines without them — CI included — don't
//! fail. See `docs/TESTING.md`.
//!
//! The tests are serialized behind a global mutex: spinning up seven WebKit
//! instances at once thrashes a loaded machine and makes timing flaky. Each
//! still gets a fully isolated harness; they just don't overlap.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use gtk::gio;
use gtk::glib;
use gtk::glib::variant::ToVariant;

const INTERFACE: &str = "org.membranepotential.jumanji";
const OBJECT_PATH: &str = "/org/membranepotential/jumanji";

/// Serializes test bodies (see module docs).
fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Hands out distinct X display numbers so parallel-*spawned* harnesses (the
/// lock only serializes bodies) never collide on a display.
fn next_display() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    // Base well clear of a real :0..:9; offset by pid to avoid clashing with a
    // concurrent `cargo test` invocation on the same machine.
    let base = 80 + (std::process::id() % 40);
    base + NEXT.fetch_add(1, Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// Environment gate
// ---------------------------------------------------------------------------

/// True when every external tool the harness needs is on `PATH`.
fn tools_available() -> bool {
    ["Xvfb", "xdotool", "dbus-daemon"]
        .iter()
        .all(|t| which(t).is_some())
}

fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|p| p.is_file())
}

/// Print a skip notice and return `true` when the environment can't run e2e.
fn should_skip() -> bool {
    if tools_available() {
        return false;
    }
    eprintln!(
        "e2e: skipping — need Xvfb, xdotool and dbus-daemon on PATH \
         (Arch: pacman -S xorg-server-xvfb xdotool dbus). Test passes as a no-op."
    );
    true
}

// ---------------------------------------------------------------------------
// Reader state (mirror of the GetState JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct State {
    #[allow(dead_code)]
    file: String,
    /// The statusbar breadcrumb: the jumplist's route to the current document
    /// (`a.md > b.md`), untruncated — the fitting to the bar's width is unit
    /// tested in `core::jumplist`.
    trail: String,
    scroll_y: f64,
    scroll_percent: u32,
    /// Layout width of the content column in CSS px. Reflows with geometric
    /// zoom now: it tracks the CSS viewport when the window is narrower than the
    /// column.
    content_width: f64,
    /// CSS viewport width (`window.innerWidth`).
    viewport_width: f64,
    /// `document.scrollWidth`; must stay ≤ `viewport_width` (+1) — no page
    /// horizontal scroll at any zoom.
    doc_scroll_width: f64,
    /// First `.mermaid svg` rendered width in CSS px (0 if none). Device size is
    /// `diagram_width × zoom`.
    diagram_width: f64,
    /// First `<math>` rendered width in CSS px (0 if none). Nonzero proves the
    /// MathML actually laid out.
    math_width: f64,
    /// First `<msup>` superscript shift as a fraction of the base-box height
    /// (0 if none). A sane superscript is a small positive number (< 1); the
    /// `mathjax2`-font-shadowing bug drove it to ~6.
    msup_shift_ratio: f64,
    /// First `.rendered-fence svg` width in CSS px (0 if none). Nonzero proves a
    /// configured external fence renderer (DESIGN D6.2) produced visible output.
    fence_width: f64,
    /// Rendered width of the `.frontmatter` panel in CSS px (0 when hidden,
    /// which is the default). The `:frontmatter` toggle's observable.
    frontmatter_width: f64,
    /// The scroll offset the **first painted frame** of the current document
    /// was placed at, recorded from inside the page before that frame went out
    /// (`-1` when this load opened at the top and installed no restore script).
    ///
    /// The one observable that can distinguish "ends up in the right place"
    /// from "gets there without flashing the top first" — the old,
    /// load-finished restore passed the former and failed the latter.
    first_frame_scroll_y: f64,
    /// Whether the body is still hidden waiting for its opening position. Must
    /// be false on any settled document — the guard on the hide-until-restored
    /// gate's one catastrophic failure mode, a permanently blank page.
    restoring: bool,
    /// Computed `color` of the first python function-name span, as a CSS
    /// `rgb(...)` string ("" if the document has no python). In dark mode it must
    /// not be `InspiredGithub`'s near-black light colour (`rgb(50, 50, 50)`).
    fn_color: String,
    dark: bool,
    zoom: f64,
    text_zoom: f64,
    mode: String,
    section: usize,
    toc_len: usize,
    loaded: bool,
    /// Files in the vault index (DESIGN D11). Built off-thread, so this is also
    /// how a test waits for a background rescan to land rather than sleeping.
    vault_files: usize,
}

impl State {
    /// Parse the flat JSON object `GetState` returns. Deliberately tiny: the
    /// object is flat, so a real JSON dependency would be over-engineering.
    fn parse(json: &str) -> Option<Self> {
        Some(State {
            file: field_str(json, "file")?,
            trail: field_str(json, "trail")?,
            scroll_y: field(json, "scroll_y")?.parse().ok()?,
            scroll_percent: field(json, "scroll_percent")?.parse().ok()?,
            content_width: field(json, "content_width")?.parse().ok()?,
            viewport_width: field(json, "viewport_width")?.parse().ok()?,
            doc_scroll_width: field(json, "doc_scroll_width")?.parse().ok()?,
            diagram_width: field(json, "diagram_width")?.parse().ok()?,
            math_width: field(json, "math_width")?.parse().ok()?,
            msup_shift_ratio: field(json, "msup_shift_ratio")?.parse().ok()?,
            fence_width: field(json, "fence_width")?.parse().ok()?,
            frontmatter_width: field(json, "frontmatter_width")?.parse().ok()?,
            first_frame_scroll_y: field(json, "first_frame_scroll_y")?.parse().ok()?,
            restoring: field(json, "restoring")? == "true",
            fn_color: field_str(json, "fn_color")?,
            dark: field(json, "dark")? == "true",
            zoom: field(json, "zoom")?.parse().ok()?,
            text_zoom: field(json, "text_zoom")?.parse().ok()?,
            mode: field_str(json, "mode")?,
            section: field(json, "section")?.parse().ok()?,
            toc_len: field(json, "toc_len")?.parse().ok()?,
            loaded: field(json, "loaded")? == "true",
            vault_files: field(json, "vault_files")?.parse().ok()?,
        })
    }
}

/// Raw token for `"key":<token>` up to the next `,` or `}`.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let start = json.find(&pat)? + pat.len();
    let rest = &json[start..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim())
}

/// Same, for a string value: scans to the closing quote, so values containing
/// commas (e.g. `fn_color` = `"rgb(50, 50, 50)"`) parse correctly. The emitted
/// strings contain no escaped quotes, so no escape handling is needed.
fn field_str(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = json.find(&pat)? + pat.len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A running reader under an isolated Xvfb + private bus. Drop kills everything.
struct Harness {
    display: u32,
    dbus_addr: String,
    conn: gio::DBusConnection,
    dest: String,
    window_id: String,
    /// The document the reader was launched on (used by reload tests).
    file: PathBuf,
    app: Child,
    dbus: Child,
    xvfb: Child,
}

impl Harness {
    /// Bring up Xvfb, a private session bus, and the reader on `demo/demo.md`;
    /// block until the initial load has finished and the window is focusable.
    fn launch() -> Self {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let demo = Path::new(manifest).join("demo").join("demo.md");
        Self::launch_file(demo)
    }

    /// As [`launch`](Self::launch), but against an arbitrary document — used by
    /// the live-reload test, which mutates a throwaway copy. Fresh private XDG
    /// dirs are allocated per launch.
    fn launch_file(file: PathBuf) -> Self {
        Self::launch_file_in_dir(file, None)
    }

    /// As [`launch_file`](Self::launch_file), but with an explicit working
    /// directory for the child. The vault root comes from the *document*, not
    /// the CWD (DESIGN D11), so this exists precisely to prove the CWD is
    /// irrelevant — see `vault_root_follows_the_marker_not_the_working_directory`.
    fn launch_file_in_dir(file: PathBuf, cwd: Option<PathBuf>) -> Self {
        let id = next_display();
        let config_home = std::env::temp_dir().join(format!("jumanji-e2e-xdg-{id}"));
        let data_home = std::env::temp_dir().join(format!("jumanji-e2e-data-{id}"));
        Self::launch_in_forward(file, config_home, data_home, None, cwd)
    }

    /// Launch on `file` with explicit private `config_home`/`data_home` dirs. The
    /// data home holds `history.toml`, so a relaunch on the same data home
    /// exercises window-state persistence.
    fn launch_in(file: PathBuf, config_home: PathBuf, data_home: PathBuf) -> Self {
        Self::launch_in_forward(file, config_home, data_home, None, None)
    }

    /// As [`launch_in`](Self::launch_in), but optionally passing `--forward
    /// <line>` (DESIGN D7 forward sync on a fresh launch).
    fn launch_in_forward(
        file: PathBuf,
        config_home: PathBuf,
        data_home: PathBuf,
        forward: Option<u32>,
        cwd: Option<PathBuf>,
    ) -> Self {
        let display = next_display();
        let display_arg = format!(":{display}");

        let xvfb = Command::new("Xvfb")
            .arg(&display_arg)
            .args(["-screen", "0", "1280x1024x24"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn Xvfb");
        wait_for(Duration::from_secs(10), || {
            Path::new(&format!("/tmp/.X11-unix/X{display}")).exists()
        })
        .expect("Xvfb socket did not appear");

        let (dbus, dbus_addr) = spawn_private_bus();

        // Isolate the app from the developer's real ~/.config and ~/.local/share:
        // a private XDG_CONFIG_HOME means default options; a private
        // XDG_DATA_HOME means the history file never touches the real one.
        let _ = fs::create_dir_all(&config_home);
        let _ = fs::create_dir_all(&data_home);

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_jumanji"));
        cmd.arg(&file);
        if let Some(line) = forward {
            cmd.args(["--forward", &line.to_string()]);
        }
        // The vault index is rooted at the child's working directory (D11).
        if let Some(cwd) = &cwd {
            cmd.current_dir(cwd);
        }
        let app = cmd
            .env("DISPLAY", &display_arg)
            .env("DBUS_SESSION_BUS_ADDRESS", &dbus_addr)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_DATA_HOME", &data_home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn jumanji");
        let dest = format!("{INTERFACE}.PID-{}", app.id());

        let conn = gio::DBusConnection::for_address_sync(
            &dbus_addr,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            gio::Cancellable::NONE,
        )
        .expect("connect to private session bus");

        let mut h = Harness {
            display,
            dbus_addr,
            conn,
            dest,
            window_id: String::new(),
            file,
            app,
            dbus,
            xvfb,
        };

        // Wait for the initial LoadEvent::Finished — this is exactly why the
        // `loaded` flag exists: keys/actions before it are no-ops.
        h.wait_for_state("initial load", Duration::from_secs(20), |s| s.loaded);

        // Resolve the window (matched by WM_CLASS, not the ambiguous title) and
        // give it X input focus. Under a bare Xvfb there is no window manager,
        // so GTK only receives key events once we set the input focus ourselves;
        // synthetic `key --window` events to an unfocused window are dropped.
        h.window_id = h.find_window();
        h.xdotool(["windowfocus", "--sync", &h.window_id]);

        h
    }

    /// Launch `jumanji -` with a piped stdin, returning the harness and the
    /// child's stdin handle so the test can stream markdown in (and drop it to
    /// signal EOF). Mirrors [`launch_in_forward`](Self::launch_in_forward) but
    /// passes `-` and pipes stdin instead of a file argument. Blocks until the
    /// initial load finishes (an empty stream still renders and reports loaded).
    fn launch_stdin() -> (Self, std::process::ChildStdin) {
        let id = next_display();
        let config_home = std::env::temp_dir().join(format!("jumanji-e2e-xdg-stdin-{id}"));
        let data_home = std::env::temp_dir().join(format!("jumanji-e2e-data-stdin-{id}"));
        let _ = fs::create_dir_all(&config_home);
        let _ = fs::create_dir_all(&data_home);

        let display = next_display();
        let display_arg = format!(":{display}");

        let xvfb = Command::new("Xvfb")
            .arg(&display_arg)
            .args(["-screen", "0", "1280x1024x24"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn Xvfb");
        wait_for(Duration::from_secs(10), || {
            Path::new(&format!("/tmp/.X11-unix/X{display}")).exists()
        })
        .expect("Xvfb socket did not appear");

        let (dbus, dbus_addr) = spawn_private_bus();

        let mut app = Command::new(env!("CARGO_BIN_EXE_jumanji"))
            .arg("-")
            .env("DISPLAY", &display_arg)
            .env("DBUS_SESSION_BUS_ADDRESS", &dbus_addr)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_DATA_HOME", &data_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn jumanji -");
        let stdin = app.stdin.take().expect("child stdin pipe");
        let dest = format!("{INTERFACE}.PID-{}", app.id());

        let conn = gio::DBusConnection::for_address_sync(
            &dbus_addr,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            gio::Cancellable::NONE,
        )
        .expect("connect to private session bus");

        let mut h = Harness {
            display,
            dbus_addr,
            conn,
            dest,
            window_id: String::new(),
            file: PathBuf::from("-"),
            app,
            dbus,
            xvfb,
        };

        h.wait_for_state("initial stdin load", Duration::from_secs(20), |s| s.loaded);
        h.window_id = h.find_window();
        h.xdotool(["windowfocus", "--sync", &h.window_id]);
        (h, stdin)
    }

    /// Type a UTF-8 string into the focused widget (the input bar), for driving
    /// the `:` command line.
    fn type_text(&self, text: &str) {
        self.xdotool(["type", "--window", &self.window_id, text]);
    }

    /// Send `q` and wait for the app to exit cleanly, so the window-close
    /// handler flushes `history.toml` (a SIGKILL via Drop would skip it).
    fn clean_quit(&mut self) {
        self.key(&["q"]);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if matches!(self.app.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Run `xdotool` against this harness's display.
    fn xdotool<I, S>(&self, args: I) -> std::process::Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        Command::new("xdotool")
            .args(args)
            .env("DISPLAY", format!(":{}", self.display))
            .output()
            .expect("run xdotool")
    }

    fn find_window(&self) -> String {
        let out = self.xdotool(["search", "--sync", "--onlyvisible", "--class", "jumanji"]);
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines()
            .next()
            .map(str::to_string)
            .expect("jumanji window not found by WM_CLASS")
    }

    /// Inject keys into the focused window (XTEST-style, delivered to the window
    /// that holds input focus). Accepts xdotool key syntax, e.g. `["shift+g"]`.
    fn key(&self, keys: &[&str]) {
        let mut args = vec!["key", "--window", &self.window_id];
        args.extend_from_slice(keys);
        self.xdotool(args);
    }

    /// Move the pointer to window-relative `(x, y)`.
    fn mouse_move(&self, x: i32, y: i32) {
        self.xdotool([
            "mousemove".into(),
            "--window".into(),
            self.window_id.clone(),
            x.to_string(),
            y.to_string(),
        ]);
    }

    /// Synthesize a Ctrl+wheel burst at the current pointer: `up` scrolls the
    /// wheel up (button 4 → zoom in) else down (button 5 → zoom out), `count`
    /// ticks `delay_ms` apart. Delivered via XTEST **at the pointer** (not
    /// `--window`, which drops synthetic button-4/5 events under bare Xvfb).
    fn ctrl_wheel(&self, up: bool, count: u32, delay_ms: u32) {
        let button = if up { "4" } else { "5" };
        self.xdotool(["keydown".to_string(), "ctrl".to_string()]);
        self.xdotool([
            "click".to_string(),
            "--repeat".to_string(),
            count.to_string(),
            "--delay".to_string(),
            delay_ms.to_string(),
            button.to_string(),
        ]);
        self.xdotool(["keyup".to_string(), "ctrl".to_string()]);
    }

    /// Click a mouse side button (8 = back, 9 = forward) at the current pointer.
    /// Delivered via XTEST at the pointer for the same reason as [`click`]:
    /// bare Xvfb drops synthetic `--window` button events.
    fn side_click(&self, button: u32) {
        self.xdotool(["click".to_string(), button.to_string()]);
    }

    /// Ctrl + left-click at window-relative `(x, y)` (reverse editor sync). Like
    /// [`ctrl_wheel`](Self::ctrl_wheel), the click is delivered via XTEST at the
    /// pointer (bare Xvfb drops synthetic `--window` button events).
    fn ctrl_click(&self, x: i32, y: i32) {
        self.mouse_move(x, y);
        self.xdotool(["keydown".to_string(), "ctrl".to_string()]);
        self.xdotool(["click".to_string(), "1".to_string()]);
        self.xdotool(["keyup".to_string(), "ctrl".to_string()]);
    }

    /// Plain left-click at window-relative `(x, y)`. Like [`ctrl_click`], the
    /// button event is delivered via XTEST at the pointer, since bare Xvfb drops
    /// synthetic `--window` button events.
    fn click(&self, x: i32, y: i32) {
        self.mouse_move(x, y);
        self.xdotool(["click".to_string(), "1".to_string()]);
    }

    /// Double left-click at window-relative `(x, y)` (two clicks close enough to
    /// register as an activation). Delivered at the pointer, as with [`click`].
    fn double_click(&self, x: i32, y: i32) {
        self.mouse_move(x, y);
        self.xdotool([
            "click".to_string(),
            "--repeat".to_string(),
            "2".to_string(),
            "--delay".to_string(),
            "25".to_string(),
            "1".to_string(),
        ]);
    }

    /// Forward editor sync over D-Bus: `GotoLine(line)`.
    fn goto_line(&self, line: u32) {
        self.call("GotoLine", Some(&(line,).to_variant()))
            .unwrap_or_else(|e| panic!("GotoLine({line}) failed: {e}"));
    }

    fn call(
        &self,
        method: &str,
        params: Option<&glib::Variant>,
    ) -> Result<glib::Variant, glib::Error> {
        self.conn.call_sync(
            Some(&self.dest),
            OBJECT_PATH,
            INTERFACE,
            method,
            params,
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
    }

    /// Run an action string via the pure D-Bus path (no key injection).
    fn execute_action(&self, action: &str, count: u32) {
        self.call("ExecuteAction", Some(&(action, count).to_variant()))
            .unwrap_or_else(|e| panic!("ExecuteAction({action}, {count}) failed: {e}"));
    }

    fn try_get_state(&self) -> Option<State> {
        let reply = self.call("GetState", None).ok()?;
        let (json,) = reply.get::<(String,)>()?;
        State::parse(&json)
    }

    fn get_state(&self) -> State {
        self.try_get_state().expect("GetState")
    }

    /// Poll `GetState` until `pred` holds or `timeout` elapses; return the last
    /// observed state either way so callers can assert with a useful message.
    fn wait_for_state<F>(&self, what: &str, timeout: Duration, pred: F) -> State
    where
        F: Fn(&State) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut last = None;
        loop {
            if let Some(s) = self.try_get_state() {
                if pred(&s) {
                    return s;
                }
                last = Some(s);
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {what}; last state = {last:?}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.app.kill();
        let _ = self.app.wait();
        let _ = self.dbus.kill();
        let _ = self.dbus.wait();
        let _ = self.xvfb.kill();
        let _ = self.xvfb.wait();
        // Best-effort: leave no stale X socket for the reused display number.
        let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{}", self.display));
        // Touch `dbus_addr` so the field isn't flagged unused; it's kept for
        // debuggability when a test fails.
        let _ = &self.dbus_addr;
    }
}

/// Spawn `dbus-daemon --session` and read its address off stdout. The child is
/// kept (not `--fork`ed) so Drop can kill it directly.
fn spawn_private_bus() -> (Child, String) {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--print-address=1", "--nofork", "--nopidfile"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn dbus-daemon");
    let stdout = child.stdout.take().expect("dbus-daemon stdout");
    let mut reader = BufReader::new(stdout);
    let mut addr = String::new();
    reader.read_line(&mut addr).expect("read dbus address");
    let addr = addr.trim().to_string();
    assert!(!addr.is_empty(), "dbus-daemon produced no address");
    (child, addr)
}

/// Busy-wait for `cond`, polling every 50 ms up to `timeout`.
fn wait_for<F: Fn() -> bool>(timeout: Duration, cond: F) -> Result<(), ()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(())
}

/// Acquire the serialization lock (ignoring poisoning from a panicked test) and,
/// if the environment supports it, launch a harness. `None` ⇒ skip the test.
fn setup() -> Option<(std::sync::MutexGuard<'static, ()>, Harness)> {
    let guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    if should_skip() {
        return None;
    }
    Some((guard, Harness::launch()))
}

/// As [`setup`], but launches the reader against `file` (a caller-owned copy the
/// test may mutate to exercise live reload).
fn setup_file(file: PathBuf) -> Option<(std::sync::MutexGuard<'static, ()>, Harness)> {
    let guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    if should_skip() {
        return None;
    }
    Some((guard, Harness::launch_file(file)))
}

/// Acquire the serialization lock without launching, for tests that manage their
/// own harness lifecycle (e.g. relaunch across a clean quit). `None` ⇒ skip.
fn setup_guard() -> Option<std::sync::MutexGuard<'static, ()>> {
    let guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    if should_skip() {
        return None;
    }
    Some(guard)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const SETTLE: Duration = Duration::from_secs(5);

// Note: copy-on-select is intentionally *not* covered here. Exercising it needs a
// real selection drag (mouse press-move-release across glyphs under Xvfb), which
// is inherently flaky and slow. The clipboard *target* selection (primary vs
// clipboard) is unit-tested in `core::config` (`selection_clipboard_parses_*`);
// the JS→Rust write path is a thin `Clipboard::set_text` call in `shell::view`.

#[test]
fn j_and_k_scroll() {
    let Some((_g, h)) = setup() else { return };
    assert_eq!(h.get_state().scroll_y, 0.0, "starts at top");

    h.key(&["j"]);
    let down = h.wait_for_state("j scrolls down", SETTLE, |s| s.scroll_y > 0.0);

    h.key(&["k"]);
    h.wait_for_state("k scrolls back up", SETTLE, |s| s.scroll_y < down.scroll_y);
}

#[test]
fn count_multiplies_scroll() {
    let Some((_g, h)) = setup() else { return };

    h.key(&["j"]);
    let one = h
        .wait_for_state("single j", SETTLE, |s| s.scroll_y > 0.0)
        .scroll_y;

    h.key(&["g", "g"]);
    h.wait_for_state("gg resets to top", SETTLE, |s| s.scroll_y == 0.0);

    h.key(&["5", "j"]);
    let five = h
        .wait_for_state("5j scrolls", SETTLE, |s| s.scroll_y > one)
        .scroll_y;

    // ~5×, tolerant of any sub-pixel rounding: within one step of 5×.
    let expected = 5.0 * one;
    assert!(
        (five - expected).abs() <= one,
        "5j ({five}) should be ~5× one j ({one}); expected ~{expected}"
    );
}

#[test]
fn g_jumps_to_bottom_and_top() {
    let Some((_g, h)) = setup() else { return };

    h.key(&["shift+g"]);
    let bottom = h.wait_for_state("G goes to bottom", SETTLE, |s| s.scroll_percent == 100);
    assert!(bottom.scroll_y > 1000.0, "bottom scroll_y should be large");

    h.key(&["g", "g"]);
    h.wait_for_state("gg returns to top", SETTLE, |s| {
        s.scroll_y == 0.0 && s.scroll_percent == 0
    });
}

#[test]
fn ctrl_r_toggles_dark() {
    let Some((_g, h)) = setup() else { return };
    assert!(!h.get_state().dark, "starts light");

    h.key(&["ctrl+r"]);
    h.wait_for_state("Ctrl-r enables dark", SETTLE, |s| s.dark);

    h.key(&["ctrl+r"]);
    h.wait_for_state("Ctrl-r disables dark", SETTLE, |s| !s.dark);
}

#[test]
fn dark_mode_python_function_name_is_readable() {
    // Regression for the dark-mode syntax-CSS bug: `InspiredGithub`'s
    // deeply-scoped `.source.python .entity.name.function { color:#323232 }`
    // (rgb(50,50,50), near-black) used to outrank the `html.dark`-nested dark
    // rule and leak onto the dark background, making a python function name
    // unreadable. With the light block now scoped under `html:not(.dark)`, it
    // cannot apply in dark mode at all. The demo has a `def fib(...)` python
    // fence; assert its function-name colour is not the light near-black once
    // dark mode is on.
    let Some((_g, h)) = setup() else { return };

    // Sanity: in light mode the demo's python function name *is* the light
    // near-black — proves the probe targets the right span.
    let light = h.get_state();
    assert_eq!(
        light.fn_color, "rgb(50, 50, 50)",
        "light-mode python function name should be InspiredGithub near-black \
         (probe target check); got {:?}",
        light.fn_color
    );

    h.key(&["ctrl+r"]);
    let dark = h.wait_for_state("Ctrl-r enables dark", SETTLE, |s| s.dark);
    assert_ne!(
        dark.fn_color, "rgb(50, 50, 50)",
        "in dark mode the python function name must not be the light theme's \
         near-black (rgb(50, 50, 50)) — it would be unreadable on #1a1a1a"
    );
    assert!(
        !dark.fn_color.is_empty(),
        "expected a computed colour for the python function-name span"
    );
}

#[test]
fn math_renders_as_mathml_with_width() {
    // The demo's Math section has inline and display LaTeX. This asserts WebKit
    // actually laid out a `<math>` element (nonzero width) — i.e. the MathML the
    // pipeline emits renders natively, no JavaScript involved.
    let Some((_g, h)) = setup() else { return };
    let s = h.get_state();
    assert!(
        s.math_width > 0.0,
        "expected a rendered <math> element with nonzero width, got {}",
        s.math_width
    );
}

#[test]
fn math_superscript_vertical_offset_is_sane() {
    // Regression for the mathjax2 font-shadowing bug: the demo's `$E = mc^2$`
    // has a `<msup>` (the `c^2`). A healthy superscript sits just above the base
    // top, so `(base.top - sup.top) / base.height` is a small positive number
    // well under one base-height. The bug (WebKit resolving `local('Latin Modern
    // Math')` to mathjax2's MATH-table-less, huge-ascent subset) flung the
    // superscript ~6 base-heights up. The unique-family + no-local() fix keeps
    // layout deterministic; assert the shift stays sane (< 1 base-height).
    let Some((_g, h)) = setup() else { return };
    let s = h.get_state();
    assert!(
        s.msup_shift_ratio > 0.0 && s.msup_shift_ratio < 1.0,
        "msup superscript shift must be a sane fraction of the base height \
         (0 < r < 1), got {}",
        s.msup_shift_ratio
    );
}

#[test]
fn geometric_zoom_in_and_reset() {
    // `+` is the geometric-zoom default (zathura muscle memory); `zoom reset`
    // clears it via the pure D-Bus path.
    let Some((_g, h)) = setup() else { return };
    assert_eq!(h.get_state().zoom, 1.0, "starts at 1.0");

    h.key(&["plus"]);
    h.wait_for_state("+ raises geometric zoom", SETTLE, |s| s.zoom > 1.0);

    h.execute_action("zoom reset", 1);
    h.wait_for_state("zoom reset clears zoom", SETTLE, |s| {
        (s.zoom - 1.0).abs() < 1e-9
    });
}

#[test]
fn narrow_viewport_zoom_reflows_without_page_overflow() {
    // The v0.3 zoom redesign: geometric zoom *reflows* the prose into the
    // viewport (no page horizontal scroll ever), while diagrams keep growing
    // with zoom (their device size scales). Replaces the old reflow-free test,
    // whose no-reflow invariant is now intentionally dead.
    let Some((_g, h)) = setup() else { return };

    // Shrink the window well below page-width (720) so the column is
    // viewport-constrained. No WM under Xvfb, so resize the X window directly.
    h.xdotool(["windowsize", "--sync", &h.window_id, "500", "800"]);
    // Let the resize settle: the baseline must be the final width, not a
    // mid-resize snapshot.
    let narrow = {
        let prev = std::cell::Cell::new(-1.0_f64);
        h.wait_for_state("width stable after resize", SETTLE, move |s| {
            let stable = s.content_width > 0.0 && (s.content_width - prev.get()).abs() < 1.0;
            prev.set(s.content_width);
            stable
        })
    };
    // Baseline invariants at zoom 1: the column is viewport-bound (tracks
    // innerWidth), there is no page horizontal scroll, and a diagram exists.
    assert!(
        (narrow.content_width - narrow.viewport_width).abs() <= 3.0,
        "column should track the narrow viewport: content {} vs viewport {}",
        narrow.content_width,
        narrow.viewport_width
    );
    assert!(
        narrow.doc_scroll_width <= narrow.viewport_width + 1.0,
        "no page h-scroll at zoom 1: scrollWidth {} vs viewport {}",
        narrow.doc_scroll_width,
        narrow.viewport_width
    );
    assert!(narrow.diagram_width > 0.0, "demo has a mermaid diagram");
    // New user-visible behaviour (intrinsic-size model): at zoom 1 a big diagram
    // renders at its *natural* width, which exceeds the narrow viewport — the
    // overflow scrolls inside the `.mermaid` box (overflow-x: auto), never the
    // page (doc_scroll_width ≤ viewport, asserted just above). The first demo
    // diagram is ~1200 px intrinsic, well past the 500 px window.
    assert!(
        narrow.diagram_width > narrow.viewport_width,
        "at zoom 1 the wide diagram should exceed the narrow viewport (intrinsic \
         size, overflow inside its box): diagram {} vs viewport {}",
        narrow.diagram_width,
        narrow.viewport_width
    );

    // Scroll into the document, then zoom in hard (~1.5×).
    h.execute_action("scroll down", 10);
    h.wait_for_state("scrolled", SETTLE, |s| s.scroll_y > 0.0);
    h.execute_action("zoom in", 5);
    h.wait_for_state("zoom applied", SETTLE, |s| s.zoom > 1.4);
    // Wait for the reflow to settle: the zoom level lands before the layout has
    // finished reflowing (the native zoom + anchor restore run in an async JS
    // callback), so read a *stable* width, not the first mid-transition snapshot.
    let zoomed = {
        let prev = std::cell::Cell::new(-1.0_f64);
        h.wait_for_state("width settled after zoom", SETTLE, move |s| {
            let stable =
                s.zoom > 1.4 && s.content_width > 0.0 && (s.content_width - prev.get()).abs() < 1.0;
            prev.set(s.content_width);
            stable
        })
    };

    // 1) No page horizontal scroll at zoom — the whole point of reflow.
    assert!(
        zoomed.doc_scroll_width <= zoomed.viewport_width + 1.0,
        "no page h-scroll under zoom: scrollWidth {} vs viewport {}",
        zoomed.doc_scroll_width,
        zoomed.viewport_width
    );
    // 2) Column re-fit: it still tracks the (now-shrunken CSS) viewport, and is
    //    genuinely narrower than the zoom-1 column.
    assert!(
        (zoomed.content_width - zoomed.viewport_width).abs() <= 3.0,
        "column should re-fit the viewport under zoom: content {} vs viewport {}",
        zoomed.content_width,
        zoomed.viewport_width
    );
    assert!(
        zoomed.content_width < narrow.content_width - 1.0,
        "column should reflow narrower under zoom: {} -> {}",
        narrow.content_width,
        zoomed.content_width
    );
    // 3) The diagram really zooms: its *device* width (CSS width × zoom) grew
    //    ≥1.3× its zoom-1 device size even as the column shrank.
    let dev0 = narrow.diagram_width * narrow.zoom;
    let dev1 = zoomed.diagram_width * zoomed.zoom;
    assert!(
        dev1 >= dev0 * 1.3,
        "diagram device width should grow with zoom: {dev0} -> {dev1}"
    );
}

#[test]
fn key_zoom_keeps_reading_position_anchored() {
    // Geometric zoom reflows, so without anchoring the reading position would
    // drift. Keyboard/D-Bus zoom anchors at the top of the viewport: the scroll
    // percentage should stay in a sane band across several zoom steps.
    let Some((_g, h)) = setup() else { return };

    // The showcase renders every diagram at full intrinsic height, so it is very
    // tall; scroll well in to land comfortably mid-document (not near an edge,
    // where the top anchor has nothing to hold).
    h.execute_action("scroll down", 55);
    let mid = h.wait_for_state("scrolled into document", SETTLE, |s| {
        s.scroll_percent > 8 && s.scroll_percent < 88
    });

    for _ in 0..4 {
        h.execute_action("zoom in", 1);
    }
    let zoomed = h.wait_for_state("zoomed in several steps", SETTLE, |s| s.zoom > 1.3);
    let drift = (zoomed.scroll_percent as i64 - mid.scroll_percent as i64).abs();
    assert!(
        drift <= 8,
        "top-anchored zoom should hold the reading position: {}% -> {}% (drift {drift})",
        mid.scroll_percent,
        zoomed.scroll_percent
    );
}

#[test]
fn ctrl_wheel_zooms_towards_cursor_without_overflow() {
    // Ctrl+wheel is cursor-anchored geometric zoom. Under bare Xvfb this needs a
    // real pointer + XTEST wheel (see `ctrl_wheel`); if a machine can't deliver
    // synthetic wheel events this will time out — acceptable, e2e already gates
    // on tool availability and never runs in CI.
    let Some((_g, h)) = setup() else { return };
    assert_eq!(h.get_state().zoom, 1.0, "starts at 1.0");

    h.mouse_move(200, 300);
    h.ctrl_wheel(true, 3, 5); // three ticks in
    let zoomed = h.wait_for_state("ctrl+wheel raises zoom", SETTLE, |s| s.zoom > 1.0);
    // The cursor-anchored reflow must not introduce page horizontal scroll.
    assert!(
        zoomed.doc_scroll_width <= zoomed.viewport_width + 1.0,
        "no page h-scroll after cursor-anchored zoom: scrollWidth {} vs viewport {}",
        zoomed.doc_scroll_width,
        zoomed.viewport_width
    );

    h.ctrl_wheel(false, 3, 5); // three ticks back out
    h.wait_for_state("ctrl+wheel lowers zoom back", SETTLE, |s| {
        (s.zoom - 1.0).abs() < 0.05
    });
}

#[test]
fn ctrl_wheel_zoom_anchors_with_cursor_near_bottom() {
    // Regression for the cursor-near-bottom anchor bug: the capture JS runs while
    // the page is still laid out at the OLD zoom, so the window→CSS px conversion
    // (`cursor_anchor` divides by the zoom) must use the OLD zoom. The bug set
    // `s.zoom` to the new level *before* computing the anchor, so the divisor was
    // wrong and the error grew with distance from the origin — worst with the
    // cursor low in the viewport. Precise element-level anchoring isn't
    // observable over the GetState surface, so we assert the robust invariants
    // the brief calls for: no page h-scroll, and the reflow moves the reading
    // position in the correct direction (zoom-in anchored below the top scrolls
    // down) without flinging to the top.
    let Some((_g, h)) = setup() else { return };

    // Scroll into the middle of the document first.
    h.execute_action("scroll down", 12);
    let before = h.wait_for_state("scrolled to mid", SETTLE, |s| {
        s.scroll_y > 0.0 && s.scroll_percent > 5 && s.scroll_percent < 90
    });

    // Pointer ~80% down the ~800 px-tall window, then three ticks in.
    h.mouse_move(200, 620);
    h.ctrl_wheel(true, 3, 5);
    let after = h.wait_for_state("cursor-near-bottom zoom applied", SETTLE, |s| s.zoom > 1.0);

    assert!(
        after.doc_scroll_width <= after.viewport_width + 1.0,
        "no page h-scroll after cursor-near-bottom zoom: scrollWidth {} vs viewport {}",
        after.doc_scroll_width,
        after.viewport_width
    );
    // Correct direction: anchoring a point below the top while zooming in keeps
    // that lower point fixed, which scrolls the viewport *down*, never up.
    assert!(
        after.scroll_y > before.scroll_y - 1.0,
        "zoom-in anchored near the bottom must not scroll upward: {} -> {}",
        before.scroll_y,
        after.scroll_y
    );
    // And it must stay in the document, not fling back to the top.
    assert!(
        after.scroll_y > 0.0,
        "reading position should stay in the document, not jump to the top: y={}",
        after.scroll_y
    );
}

#[test]
fn ctrl_wheel_burst_coalesces_without_losing_steps() {
    // A rapid 10-tick burst must apply as one coalesced anchored zoom yet lose no
    // step: zoom-step 0.1 × 10 ≈ +1.0, so it settles near 2.0.
    let Some((_g, h)) = setup() else { return };

    h.mouse_move(200, 300);
    h.ctrl_wheel(true, 10, 2); // ten ticks, 2 ms apart → within one coalesce window
    let z = h.wait_for_state("burst settles near 2.0", SETTLE, |s| s.zoom > 1.85);
    assert!(
        (z.zoom - 2.0).abs() < 0.2,
        "10-tick burst should reach ~2.0 with no lost steps, got {}",
        z.zoom
    );
    assert!(
        z.doc_scroll_width <= z.viewport_width + 1.0,
        "no page h-scroll after burst"
    );
}

#[test]
fn section_next_and_previous() {
    let Some((_g, h)) = setup() else { return };
    let start = h.get_state();
    assert!(start.toc_len > 1, "demo needs multiple sections");
    assert_eq!(start.section, 0);

    h.key(&["shift+j"]);
    h.wait_for_state("J advances section", SETTLE, |s| s.section == 1);

    h.key(&["shift+k"]);
    h.wait_for_state("K goes back a section", SETTLE, |s| s.section == 0);
}

#[test]
fn execute_action_scrolls_without_keys() {
    let Some((_g, h)) = setup() else { return };
    assert_eq!(h.get_state().scroll_y, 0.0);

    // Pure D-Bus path — no key injection, no window focus needed.
    h.execute_action("scroll down", 3);
    h.wait_for_state("ExecuteAction scrolls", SETTLE, |s| s.scroll_y > 0.0);
}

#[test]
fn text_zoom_changes_and_reset_clears_both_axes() {
    let Some((_g, h)) = setup() else { return };
    let start = h.get_state();
    assert!(
        (start.text_zoom - 1.0).abs() < 1e-9,
        "text zoom starts at 1.0"
    );
    assert!(
        (start.zoom - 1.0).abs() < 1e-9,
        "geometric zoom starts at 1.0"
    );

    // Text zoom has no default key (it's Ctrl+Shift+wheel / config); drive it
    // via the D-Bus action.
    h.execute_action("text zoom in", 1);
    h.wait_for_state("text zoom in raises text zoom", SETTLE, |s| {
        s.text_zoom > 1.0
    });

    // Also push geometric zoom via the pure D-Bus path, then assert `=` resets
    // *both* axes to 100%.
    h.execute_action("zoom in", 2);
    h.wait_for_state("zoom in raises geometric zoom", SETTLE, |s| s.zoom > 1.0);

    h.key(&["equal"]);
    h.wait_for_state("= resets both axes", SETTLE, |s| {
        (s.zoom - 1.0).abs() < 1e-9 && (s.text_zoom - 1.0).abs() < 1e-9
    });
}

#[test]
fn external_reads_do_not_storm_reload() {
    // Regression for the self-sustaining reload loop: an *external read* of the
    // document must not trigger a reload (a storm would reset scroll to the top).
    let Some((_g, h)) = setup() else { return };

    h.execute_action("scroll down", 5);
    let scrolled = h
        .wait_for_state("scrolled down", SETTLE, |s| s.scroll_y > 0.0)
        .scroll_y;

    // Read the file several times, exactly as the buggy reload handler did.
    for _ in 0..5 {
        let _ = std::fs::read(&h.file).expect("read demo file");
    }
    std::thread::sleep(Duration::from_millis(1500));

    let after = h.get_state();
    assert!(after.loaded, "still loaded after external reads");
    assert!(
        (after.scroll_y - scrolled).abs() < 1.0,
        "scroll must be unchanged by external reads (a reload storm would reset it): \
         was {scrolled}, now {}",
        after.scroll_y
    );
}

#[test]
fn live_reload_grows_toc_and_preserves_dark() {
    // Launch against a throwaway copy so we can mutate it. A genuine content
    // change (appending a heading) must reload — observed as the TOC growing —
    // and dark mode must survive the reload (no light flash, stays dark).
    let manifest = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest).join("demo").join("demo.md");
    let dir = std::env::temp_dir().join(format!("jumanji-e2e-reload-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let copy = dir.join("live.md");
    std::fs::copy(&src, &copy).expect("copy demo");

    let Some((_g, h)) = setup_file(copy.clone()) else {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };

    let start = h.get_state();
    let toc0 = start.toc_len;

    // Turn on dark mode for the persistence check.
    h.execute_action("recolor", 1);
    h.wait_for_state("dark enabled", SETTLE, |s| s.dark);

    // Verify the *native* geometric zoom survives a reload — the open question
    // behind dropping the `--zoom`/set_zoom re-apply from the load-finished
    // handler. Shrink the window and zoom in: the CSS viewport width
    // (innerWidth = deviceWidth / zoom) collapses well below the device 500 px.
    // If native zoom did NOT survive the reload it would snap back toward 500.
    h.xdotool(["windowsize", "--sync", &h.window_id, "500", "800"]);
    h.execute_action("zoom in", 5); // +0.5 → ~1.5×
    let zoomed = {
        let prev = std::cell::Cell::new(-1.0_f64);
        h.wait_for_state("viewport collapses under zoom", SETTLE, move |s| {
            let stable = s.zoom > 1.4
                && s.viewport_width > 0.0
                && (s.viewport_width - prev.get()).abs() < 1.0;
            prev.set(s.viewport_width);
            stable
        })
    };
    assert!(
        zoomed.viewport_width < 450.0,
        "zoomed CSS viewport should collapse below the device width: {}",
        zoomed.viewport_width
    );

    // Append a new heading to trigger a real reload.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&copy)
            .expect("open copy for append");
        writeln!(f, "\n\n## Appended Section For Reload\n\nBody text.\n").expect("append");
    }

    let reloaded = h.wait_for_state("reload grows the TOC", Duration::from_secs(10), |s| {
        s.toc_len > toc0
    });
    assert!(reloaded.dark, "dark mode must persist across a live reload");
    // The native zoom survived: the CSS viewport is still collapsed (not snapped
    // back to ~500), so no load-finished re-apply is needed.
    assert!(
        reloaded.viewport_width < 450.0,
        "native geometric zoom must survive the reload (CSS viewport stays \
         collapsed): {} (pre-reload {})",
        reloaded.viewport_width,
        zoomed.viewport_width
    );

    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn live_reload_keeps_text_zoom_and_position_from_the_first_frame() {
    // Saving the file you are reading is the most-seen instance of the flash:
    // every write re-renders and reloads, and the position and the text zoom
    // both used to be re-applied *after* `LoadEvent::Finished` — one painted
    // frame of the base-size, unscrolled top each time. Both now ride into the
    // load, so the first frame is already right.
    let manifest = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest).join("demo").join("demo.md");
    let dir = std::env::temp_dir().join(format!("jumanji-e2e-tzreload-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let copy = dir.join("live.md");
    std::fs::copy(&src, &copy).expect("copy demo");

    let Some((_g, h)) = setup_file(copy.clone()) else {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };

    // The maximum scroll offset is a direct proxy for the document's height, and
    // therefore for the font size actually in effect — unlike `text_zoom`, which
    // reports the shell's *intent* and survives a reload either way.
    h.key(&["shift+g"]);
    let base_bottom = h
        .wait_for_state("scrolled to the bottom", SETTLE, |s| s.scroll_y > 100.0)
        .scroll_y;

    h.execute_action("text zoom in", 4);
    h.wait_for_state("text zoom raises the factor", SETTLE, |s| s.text_zoom > 1.0);
    h.key(&["shift+g"]);
    let zoomed_bottom = h
        .wait_for_state("larger prose pushes the bottom down", SETTLE, |s| {
            s.scroll_y > base_bottom * 1.05
        })
        .scroll_y;

    // Touch the file — a trailing newline changes nothing about the render, so
    // any height difference below is the font size, not the content.
    let content = std::fs::read_to_string(&copy).expect("read copy");
    std::fs::write(&copy, format!("{content}\n")).expect("touch copy");

    // The reload announces itself through the restore script it installs: a
    // position-preserving reload records the offset its first frame painted at,
    // and a fresh document (this one, at launch) records nothing (-1).
    let reloaded = h.wait_for_state(
        "live reload restores through the load",
        Duration::from_secs(10),
        |s| s.first_frame_scroll_y > 0.0,
    );
    assert!(
        (reloaded.first_frame_scroll_y - zoomed_bottom).abs() < 20.0,
        "the first painted frame of a live reload must already be at the \
         preserved offset {zoomed_bottom}, got {}",
        reloaded.first_frame_scroll_y
    );
    // Had the text zoom not been pre-applied into the HTML, the reloaded
    // document would be base-sized: shorter, so the restore would clamp to
    // `base_bottom` and this would fail — which is exactly what makes the
    // dropped load-finished re-apply safe to drop.
    assert!(
        reloaded.first_frame_scroll_y > base_bottom * 1.05,
        "text zoom must be in effect from the first frame: first-frame offset \
         {} should exceed the base-font document height {base_bottom}",
        reloaded.first_frame_scroll_y
    );
    h.wait_for_state("body revealed after the live reload", SETTLE, |s| {
        !s.restoring
    });

    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tab_enters_and_leaves_toc_mode() {
    let Some((_g, h)) = setup() else { return };
    assert_eq!(h.get_state().mode, "normal", "starts in normal mode");

    h.key(&["Tab"]);
    h.wait_for_state("Tab enters TOC mode", SETTLE, |s| s.mode == "toc");

    h.key(&["Tab"]);
    h.wait_for_state("Tab leaves TOC mode", SETTLE, |s| s.mode == "normal");
}

#[test]
fn toc_select_jumps_and_returns_to_normal() {
    let Some((_g, h)) = setup() else { return };
    assert_eq!(h.get_state().scroll_y, 0.0, "starts at top");

    h.key(&["Tab"]);
    h.wait_for_state("TOC mode", SETTLE, |s| s.mode == "toc");

    // Move the selection down to a heading below the fold, then select it.
    h.key(&["j"]);
    h.key(&["j"]);
    h.key(&["Return"]);
    h.wait_for_state("TOC select jumps and exits", SETTLE, |s| {
        s.mode == "normal" && s.scroll_y > 1.0
    });
}

#[test]
fn toc_click_then_return_jumps_to_clicked_row() {
    // Regression: a mouse click on a TOC row must move the *selection* that
    // Enter jumps to. Before the fix, j/k moved a shell-internal index while a
    // click only changed the visual selection, so click+Enter jumped to the
    // stale internal entry (the first heading) instead of the clicked row.
    let Some((_g, h)) = setup() else { return };
    assert_eq!(h.get_state().scroll_y, 0.0, "starts at top");

    // Baseline: enter TOC and select the top entry (no movement) with Enter.
    // This is where the buggy code jumped for *any* click — the first heading.
    h.key(&["Tab"]);
    h.wait_for_state("TOC mode", SETTLE, |s| s.mode == "toc");
    h.key(&["Return"]);
    let first = h.wait_for_state("first-entry jump", SETTLE, |s| s.mode == "normal");

    // Now re-enter TOC, click a row well below the fold, and press Enter. The
    // jump must land at the clicked section, far below the first heading.
    h.key(&["Tab"]);
    h.wait_for_state("TOC mode again", SETTLE, |s| s.mode == "toc");
    h.click(200, 220);
    h.key(&["Return"]);
    let clicked = h.wait_for_state("click+Enter jumps to clicked row", SETTLE, |s| {
        s.mode == "normal" && s.scroll_y > first.scroll_y + 50.0
    });
    assert!(
        clicked.scroll_y > first.scroll_y + 50.0,
        "click+Enter should jump to the clicked entry (well below the first \
         heading at {}), got {}",
        first.scroll_y,
        clicked.scroll_y
    );
}

#[test]
fn toc_double_click_activates_and_jumps() {
    // Double-clicking a row activates it: it jumps directly (no Enter) and
    // returns to normal mode, matching GTK convention.
    let Some((_g, h)) = setup() else { return };
    assert_eq!(h.get_state().scroll_y, 0.0, "starts at top");

    h.key(&["Tab"]);
    h.wait_for_state("TOC mode", SETTLE, |s| s.mode == "toc");

    h.double_click(200, 220);
    let jumped = h.wait_for_state("double-click activates and jumps", SETTLE, |s| {
        s.mode == "normal" && s.scroll_y > 50.0
    });
    assert!(
        jumped.scroll_y > 50.0,
        "double-click should jump below the first heading, got {}",
        jumped.scroll_y
    );
}

#[test]
fn command_set_recolor_enables_dark() {
    let Some((_g, h)) = setup() else { return };
    assert!(!h.get_state().dark, "starts light");

    // `:` opens the command line; type the set command and submit.
    h.key(&["colon"]);
    h.wait_for_state("command line open", SETTLE, |s| s.mode == "command");
    h.type_text("set default-recolor true");
    h.key(&["Return"]);

    h.wait_for_state("`:set default-recolor true` turns dark on", SETTLE, |s| {
        s.dark && s.mode == "normal"
    });
}

#[test]
fn quickmark_set_and_jump_round_trip() {
    let Some((_g, h)) = setup() else { return };

    // Scroll to a position, mark it, jump to the top, then jump back to the mark.
    h.execute_action("scroll down", 8);
    let marked = h
        .wait_for_state("scrolled to mark position", SETTLE, |s| s.scroll_y > 0.0)
        .scroll_y;

    // `mark set a` / `mark jump a` via D-Bus for a deterministic register.
    h.execute_action("mark set a", 1);
    h.execute_action("goto top", 1);
    h.wait_for_state("back at top", SETTLE, |s| s.scroll_y == 0.0);

    h.execute_action("mark jump a", 1);
    h.wait_for_state("mark jump restores position", SETTLE, |s| {
        (s.scroll_y - marked).abs() < 5.0
    });
}

#[test]
fn ctrl_o_returns_after_g_jump() {
    let Some((_g, h)) = setup() else { return };
    assert_eq!(h.get_state().scroll_y, 0.0, "starts at top");

    h.key(&["shift+g"]);
    h.wait_for_state("G jumps to bottom", SETTLE, |s| s.scroll_percent == 100);

    // The jumplist recorded the pre-jump position (top); Ctrl-o returns to it.
    h.key(&["ctrl+o"]);
    h.wait_for_state("Ctrl-o returns to the pre-G position", SETTLE, |s| {
        s.scroll_y == 0.0
    });
}

#[test]
fn hint_follow_scrolls_to_fragment() {
    // A fixture with exactly one internal link → the hint label is a single `a`,
    // so `f` then `a` deterministically follows it and scrolls to the anchor.
    let manifest = env!("CARGO_MANIFEST_DIR");
    let fixture = Path::new(manifest).join("demo").join("links.md");
    let Some((_g, h)) = setup_file(fixture) else {
        return;
    };
    assert_eq!(h.get_state().scroll_y, 0.0, "starts at top");

    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");

    h.key(&["a"]);
    h.wait_for_state("fragment link scrolls to target", SETTLE, |s| {
        s.mode == "normal" && s.scroll_y > 1.0
    });
}

#[test]
fn back_and_forward_cross_documents_after_following_a_link() {
    // Two files: a.md has a single link to b.md. Following it opens b.md; the
    // document-spanning jumplist must let us walk back into a.md (`Backspace`)
    // at the position we left it, then forward again (`Ctrl-i`) into b.md. The
    // scroll offset a location carries is covered by the jumplist unit tests;
    // here we assert the shell wiring — which document each step lands on, and
    // that the departure position (a.md's top) is restored.
    let Some(_g) = setup_guard() else { return };

    let dir = std::env::temp_dir().join(format!("jumanji-e2e-back-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    // Enough filler that b.md scrolls well past the fold.
    let filler: String = (1..=80)
        .map(|i| format!("Filler paragraph {i} padding the document past the fold.\n\n"))
        .collect();
    let a = dir.join("a.md");
    let b = dir.join("b.md");
    std::fs::write(&a, format!("# Doc A\n\n[open b](b.md)\n\n{filler}")).expect("write a.md");
    std::fs::write(&b, format!("# Doc B\n\n{filler}## Bottom of B\n")).expect("write b.md");

    let h = Harness::launch_file(a.clone());
    let start = h.get_state();
    assert!(start.file.ends_with("a.md"), "starts on a.md");
    assert_eq!(start.trail, "a.md", "breadcrumb starts at the opened file");

    // Follow the single visible link (hint label `a`) → opens b.md at its top.
    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");
    h.key(&["a"]);
    let opened = h.wait_for_state("link opens b.md", SETTLE, |s| {
        s.mode == "normal" && s.file.ends_with("b.md")
    });
    // The statusbar breadcrumb grows with the route taken to get here.
    assert_eq!(opened.trail, "a.md > b.md");

    // Scroll down inside b.md so the live position we walk back from is real.
    h.execute_action("scroll down", 12);
    h.wait_for_state("scrolled within b.md", SETTLE, |s| s.scroll_y > 10.0);

    // `Backspace` walks the jumplist back into a.md at the departure (the top).
    h.key(&["BackSpace"]);
    let back = h.wait_for_state("Backspace returns to a.md at the top", SETTLE, |s| {
        s.file.ends_with("a.md") && s.scroll_y == 0.0
    });
    assert_eq!(back.trail, "a.md", "walking back shortens the breadcrumb");

    // `Ctrl-i` walks forward again into b.md.
    h.key(&["ctrl+i"]);
    let forward = h.wait_for_state("Ctrl-i returns forward to b.md", SETTLE, |s| {
        s.file.ends_with("b.md")
    });
    assert_eq!(forward.trail, "a.md > b.md", "and forward re-extends it");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn jumping_back_paints_no_unscrolled_frame() {
    // The no-flash property, which is *not* the same as "ends up in the right
    // place": the reading position now rides into the load as a document-start
    // user-script, so the first frame WebKit paints of the returned-to document
    // is already at the restored offset. The old load-finished restore painted
    // the top first and snapped afterwards — visible as a flash when walking the
    // jumplist, and here as a `first_frame_scroll_y` of 0.
    let Some(_g) = setup_guard() else { return };

    let dir = std::env::temp_dir().join(format!("jumanji-e2e-flash-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let filler: String = (1..=80)
        .map(|i| format!("Filler paragraph {i} padding the document past the fold.\n\n"))
        .collect();
    let a = dir.join("a.md");
    let b = dir.join("b.md");
    // The link sits at the *bottom* of a.md, so following it departs from a real
    // scroll offset — a departure at the top would make the assertion vacuous.
    std::fs::write(&a, format!("# Doc A\n\n{filler}[open b](b.md)\n")).expect("write a.md");
    std::fs::write(&b, format!("# Doc B\n\n{filler}")).expect("write b.md");

    let h = Harness::launch_file(a.clone());
    // A fresh document opens at the top, so no restore script runs at all.
    assert_eq!(
        h.get_state().first_frame_scroll_y,
        -1.0,
        "an unread document installs no restore script"
    );

    h.key(&["shift+g"]);
    let departed = h
        .wait_for_state("scrolled to the bottom of a.md", SETTLE, |s| {
            s.scroll_y > 200.0
        })
        .scroll_y;

    // Follow the now-visible link out of a.md.
    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");
    h.key(&["a"]);
    h.wait_for_state("link opens b.md", SETTLE, |s| {
        s.mode == "normal" && s.file.ends_with("b.md") && s.loaded
    });

    // Back into a.md at the departure offset — the load under test.
    h.key(&["ctrl+o"]);
    let back = h.wait_for_state("Ctrl-o returns to a.md", SETTLE, |s| {
        s.file.ends_with("a.md") && s.loaded && s.scroll_y > 1.0
    });
    assert!(
        (back.scroll_y - departed).abs() < 5.0,
        "returns to the departure offset {departed}, got {}",
        back.scroll_y
    );
    // The assertion this test exists for.
    assert!(
        back.first_frame_scroll_y > 0.0,
        "the first painted frame of the returned-to document must not be the \
         unscrolled top; got first_frame_scroll_y = {} (departure was {departed})",
        back.first_frame_scroll_y
    );
    assert!(
        (back.first_frame_scroll_y - departed).abs() < 5.0,
        "the first painted frame must already be at the restored offset \
         {departed}, got {}",
        back.first_frame_scroll_y
    );
    // The hide-until-restored gate must let go again — a page stuck hidden would
    // be a far worse bug than the flash it prevents. A bounded wait rather than
    // a bare assert on the snapshot above: the reveal is paced by the page's own
    // `requestAnimationFrame`, so it can legitimately land a frame after the
    // `loaded` flag this test waited on. What must hold is that it lands at all,
    // which the script's unconditional failsafe timer guarantees.
    h.wait_for_state("body revealed after the restore", SETTLE, |s| !s.restoring);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zoom_carries_across_document_switches() {
    // DESIGN D5a: zoom is a live *session* view setting, not a per-document one.
    // Following a link out of a document you were reading at 130% used to land
    // you at 100% — `load_document` restored the *target's* saved zoom, and hard
    // reset when the target had never been opened. Both axes now carry over
    // untouched, across a link follow, `Ctrl-o` and `:open` alike.
    let Some(_g) = setup_guard() else { return };

    let dir = std::env::temp_dir().join(format!("jumanji-e2e-zoomstick-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let a = dir.join("a.md");
    let b = dir.join("b.md");
    // Identical filler in both, so the two documents are the same height at the
    // same zoom — that equivalence is what lets a.md's base height below serve
    // as the baseline the *rendered* size of b.md is judged against.
    let filler = filler();
    std::fs::write(&a, format!("# Doc A\n\n{filler}[open b](b.md)\n")).expect("write a.md");
    std::fs::write(&b, format!("# Doc B\n\n{filler}")).expect("write b.md");

    // A fresh XDG data home: neither file has any history, so nothing but the
    // session could supply the zoom b.md ends up rendering at.
    let h = Harness::launch_file(a.clone());
    assert_eq!(h.get_state().zoom, 1.0, "starts at geometric 100%");
    assert_eq!(h.get_state().text_zoom, 1.0, "starts at text 100%");

    // The base-font height of the shared filler, measured on a.md.
    h.key(&["shift+g"]);
    let base_bottom = h
        .wait_for_state("scrolled to the bottom of a.md", SETTLE, |s| {
            s.scroll_y > 200.0
        })
        .scroll_y;

    h.execute_action("text zoom in", 4);
    h.execute_action("zoom in", 3);
    let zoomed = h.wait_for_state("both axes raised", SETTLE, |s| {
        s.text_zoom > 1.0 && s.zoom > 1.0
    });

    // Follow the link into a document that has never been opened.
    h.key(&["shift+g"]);
    h.wait_for_state("link back in view after the zoom", SETTLE, |s| {
        s.scroll_y > base_bottom
    });
    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");
    h.key(&["a"]);
    let switched = h.wait_for_state("link opens b.md", SETTLE, |s| {
        s.mode == "normal" && s.file.ends_with("b.md") && s.loaded
    });
    assert!(
        (switched.zoom - zoomed.zoom).abs() < 1e-9,
        "geometric zoom must survive a link follow: was {}, got {}",
        zoomed.zoom,
        switched.zoom
    );
    assert!(
        (switched.text_zoom - zoomed.text_zoom).abs() < 1e-9,
        "text zoom must survive a link follow: was {}, got {}",
        zoomed.text_zoom,
        switched.text_zoom
    );

    // …and the zoom is genuinely *rendered*, from the first painted frame. The
    // load-finished path deliberately re-applies neither axis (the native
    // `zoom_level` survives the load; the text axis is inlined as `--font-size`
    // when the HTML is generated), so a b.md that measures taller than the same
    // filler at base size can only have been built with the session's zoom
    // already in it — which is exactly what the first frame paints. Reached by
    // over-scrolling rather than `G`: a goto is a *jump*, and the jumplist entry
    // it would push inside b.md is one the `Ctrl-o` below would consume first.
    h.execute_action("scroll down", 500);
    let b_bottom = h
        .wait_for_state("scrolled to the bottom of b.md", SETTLE, |s| {
            s.scroll_y > 200.0
        })
        .scroll_y;
    assert!(
        b_bottom > base_bottom * 1.05,
        "b.md must be laid out at the inherited zoom from its first frame: \
         bottom {b_bottom} should exceed the base-size height {base_bottom}"
    );

    // `Ctrl-o` back across the file boundary keeps it too.
    h.key(&["ctrl+o"]);
    let back = h.wait_for_state("Ctrl-o returns to a.md", SETTLE, |s| {
        s.file.ends_with("a.md") && s.loaded && s.scroll_y > 1.0
    });
    assert!(
        (back.zoom - zoomed.zoom).abs() < 1e-9 && (back.text_zoom - zoomed.text_zoom).abs() < 1e-9,
        "zoom must survive jumplist navigation: was {}/{}, got {}/{}",
        zoomed.zoom,
        zoomed.text_zoom,
        back.zoom,
        back.text_zoom
    );

    // As does `Ctrl-i` forward again.
    h.key(&["ctrl+i"]);
    let forward = h.wait_for_state("Ctrl-i returns to b.md", SETTLE, |s| {
        s.file.ends_with("b.md") && s.loaded
    });
    assert!(
        (forward.zoom - zoomed.zoom).abs() < 1e-9
            && (forward.text_zoom - zoomed.text_zoom).abs() < 1e-9,
        "zoom must survive a forward jump: was {}/{}, got {}/{}",
        zoomed.zoom,
        zoomed.text_zoom,
        forward.zoom,
        forward.text_zoom
    );

    // And so does `:open`.
    h.key(&["colon"]);
    h.wait_for_state("command line open", SETTLE, |s| s.mode == "command");
    h.type_text("open a.md");
    h.key(&["Return"]);
    let opened = h.wait_for_state("`:open a.md` switches document", SETTLE, |s| {
        s.mode == "normal" && s.file.ends_with("a.md") && s.loaded
    });
    assert!(
        (opened.zoom - zoomed.zoom).abs() < 1e-9
            && (opened.text_zoom - zoomed.text_zoom).abs() < 1e-9,
        "zoom must survive `:open`: was {}/{}, got {}/{}",
        zoomed.zoom,
        zoomed.text_zoom,
        opened.zoom,
        opened.text_zoom
    );

    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn history_restores_zoom_on_a_cold_start() {
    // The other half of D5a's split: the per-file zoom in `history.toml` is the
    // *default on open*, and a window's first document is the one place with no
    // live session zoom to inherit. Reopening a note in a fresh window must
    // therefore still land at the zoom you last read it at.
    let Some(_g) = setup_guard() else { return };

    let manifest = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest).join("demo").join("demo.md");
    let dir = std::env::temp_dir().join(format!("jumanji-e2e-histzoom-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let doc = dir.join("doc.md");
    std::fs::copy(&src, &doc).expect("copy demo");
    let config_home = dir.join("cfg");
    let data_home = dir.join("data");

    let marked = {
        let mut h = Harness::launch_in(doc.clone(), config_home.clone(), data_home.clone());
        h.execute_action("zoom in", 3);
        h.execute_action("text zoom in", 4);
        let st = h.wait_for_state("both axes raised before quit", SETTLE, |s| {
            s.zoom > 1.0 && s.text_zoom > 1.0
        });
        h.clean_quit();
        drop(h);
        (st.zoom, st.text_zoom)
    };

    let h = Harness::launch_in(doc, config_home, data_home);
    let restored = h.wait_for_state("zoom restored on relaunch", SETTLE, |s| s.zoom > 1.0);
    assert!(
        (restored.zoom - marked.0).abs() < 1e-9,
        "restored geometric zoom {} should match saved {}",
        restored.zoom,
        marked.0
    );
    assert!(
        (restored.text_zoom - marked.1).abs() < 1e-9,
        "restored text zoom {} should match saved {}",
        restored.text_zoom,
        marked.1
    );

    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn history_persists_scroll_across_relaunch() {
    let Some(_g) = setup_guard() else { return };

    let manifest = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest).join("demo").join("demo.md");
    let dir = std::env::temp_dir().join(format!("jumanji-e2e-hist-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let doc = dir.join("doc.md");
    std::fs::copy(&src, &doc).expect("copy demo");
    let config_home = dir.join("cfg");
    let data_home = dir.join("data");

    // First run: scroll, then quit cleanly so history.toml is flushed.
    let marked = {
        let mut h = Harness::launch_in(doc.clone(), config_home.clone(), data_home.clone());
        h.execute_action("scroll down", 12);
        let y = h
            .wait_for_state("scrolled before quit", SETTLE, |s| s.scroll_y > 10.0)
            .scroll_y;
        h.clean_quit();
        drop(h);
        y
    };

    // A history file must now exist under the private data home.
    assert!(
        data_home.join("jumanji").join("history.toml").exists(),
        "history.toml written on clean quit"
    );

    // Relaunch on the same file + data home: the scroll offset is restored.
    {
        let h = Harness::launch_in(doc, config_home, data_home);
        let restored =
            h.wait_for_state("scroll restored on relaunch", SETTLE, |s| s.scroll_y > 1.0);
        assert!(
            (restored.scroll_y - marked).abs() < 5.0,
            "restored scroll {} should match saved {marked}",
            restored.scroll_y
        );
        // And without flashing the top on the way: opening a file you had read
        // before restores through the load, not after it (see
        // `jumping_back_paints_no_unscrolled_frame`).
        assert!(
            (restored.first_frame_scroll_y - marked).abs() < 5.0,
            "the first painted frame of a relaunch must already be at the saved \
             offset {marked}, got {}",
            restored.first_frame_scroll_y
        );
        h.wait_for_state("body revealed after the relaunch restore", SETTLE, |s| {
            !s.restoring
        });
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn external_fence_renderer_produces_output() {
    // DESIGN D6.2: a `[renderers]` entry maps a fence language to a shell
    // command whose stdout replaces the fence. Configure a `box` renderer that
    // echoes a fixed-size SVG, load a document with a `box` fence, and assert
    // the SVG actually rendered (nonzero `.rendered-fence svg` width) — the
    // pipeline ran the subprocess and embedded its output.
    let Some(_g) = setup_guard() else { return };

    let dir = std::env::temp_dir().join(format!("jumanji-e2e-fence-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let config_home = dir.join("cfg");
    let data_home = dir.join("data");
    let cfg = config_home.join("jumanji");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    std::fs::write(
        cfg.join("config.toml"),
        "[renderers]\n\
         box = \"printf '<svg xmlns=\\\"http://www.w3.org/2000/svg\\\" \
         width=\\\"123\\\" height=\\\"40\\\"></svg>'\"\n",
    )
    .expect("write config");

    let doc = dir.join("doc.md");
    std::fs::write(&doc, "# Fence renderer\n\n```box\nignored body\n```\n").expect("write doc");

    let h = Harness::launch_in(doc, config_home, data_home);
    let s = h.wait_for_state("fence renderer output rendered", SETTLE, |s| {
        s.fence_width > 0.0
    });
    assert!(
        (s.fence_width - 123.0).abs() < 5.0,
        "expected the echoed 123px SVG, got width {}",
        s.fence_width
    );

    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// stdin streaming (DESIGN M3)
// ---------------------------------------------------------------------------

/// A markdown fragment with `count` `## Section <first..>` headings, each padded
/// with filler so the rendered document is tall enough to scroll. `title` is
/// prepended (an extra `#` heading) when non-empty.
fn stdin_doc(title: &str, first: u32, count: u32) -> String {
    let mut s = String::new();
    if !title.is_empty() {
        s.push_str(&format!("# {title}\n"));
    }
    for i in first..first + count {
        s.push_str(&format!("\n## Section {i}\n\n"));
        for _ in 0..14 {
            s.push_str("Lorem ipsum dolor sit amet consectetur adipiscing elit sed do.\n");
        }
    }
    s
}

#[test]
fn stdin_dash_renders_after_content_then_close() {
    // `jumanji -` with content written to stdin then closed must render: the TOC
    // fills in and the document reports loaded.
    let Some(_g) = setup_guard() else { return };
    let (h, mut stdin) = Harness::launch_stdin();

    let doc = stdin_doc("Streamed", 1, 3); // # + 3 ## => toc_len 4
    stdin.write_all(doc.as_bytes()).expect("write stdin");
    stdin.flush().expect("flush stdin");
    drop(stdin); // EOF

    let s = h.wait_for_state("stdin content renders a TOC", SETTLE, |s| {
        s.loaded && s.toc_len >= 4
    });
    assert!(
        s.toc_len >= 4,
        "expected the streamed headings in the TOC, got {}",
        s.toc_len
    );
}

#[test]
fn stdin_streaming_grows_toc_and_preserves_scroll() {
    // Progressive rendering: write half a document, assert it renders; scroll
    // into it; write the rest, assert the TOC grows and the reading position is
    // preserved across the re-render (mirrors live_reload_grows_toc_and_preserves
    // _dark, but driven by stdin chunks instead of file edits).
    let Some(_g) = setup_guard() else { return };
    let (h, mut stdin) = Harness::launch_stdin();

    // Part one: title + 3 sections => toc_len 4.
    let part1 = stdin_doc("Streamed", 1, 3);
    stdin.write_all(part1.as_bytes()).expect("write part 1");
    stdin.flush().expect("flush part 1");
    let first = h.wait_for_state("first chunk renders", SETTLE, |s| s.toc_len >= 4);
    let toc0 = first.toc_len;

    // Scroll into the rendered content over D-Bus (no key focus needed).
    h.execute_action("scroll down", 8);
    let scrolled = h
        .wait_for_state("scrolled into streamed content", SETTLE, |s| {
            s.scroll_y > 0.0
        })
        .scroll_y;

    // Part two: 3 more sections => toc grows to 7.
    let part2 = stdin_doc("", 4, 3);
    stdin.write_all(part2.as_bytes()).expect("write part 2");
    stdin.flush().expect("flush part 2");
    drop(stdin); // EOF

    let grown = h.wait_for_state("second chunk grows the TOC", Duration::from_secs(10), |s| {
        s.toc_len > toc0
    });
    assert!(
        grown.toc_len > toc0,
        "streaming more content should grow the TOC: {toc0} -> {}",
        grown.toc_len
    );
    // The re-render preserves the reading position, exactly like live reload.
    // `toc_len` updates synchronously as the new doc is built, but the scroll
    // restore lands later in the load-finished handler, so wait for the position
    // to settle back — a broken preservation (stuck at the post-reload top) would
    // time out here rather than pass by luck.
    let settled = h.wait_for_state("reading position restored after re-render", SETTLE, |s| {
        s.toc_len > toc0 && (s.scroll_y - scrolled).abs() < 5.0
    });
    assert!(
        (settled.scroll_y - scrolled).abs() < 5.0,
        "scroll position must be preserved across a streaming re-render: \
         was {scrolled}, now {}",
        settled.scroll_y
    );
}

#[test]
fn stdin_instant_eof_renders_fine() {
    // `echo | jumanji -` — stdin closes immediately (empty). The reader hits EOF
    // at once; the app must render fine (loaded, driveable), never crash or hang.
    let Some(_g) = setup_guard() else { return };
    let (h, stdin) = Harness::launch_stdin();
    drop(stdin); // immediate EOF, no bytes written

    // Still loaded and answering D-Bus after the instant EOF.
    let s = h.wait_for_state("instant-EOF stdin stays loaded", SETTLE, |s| s.loaded);
    assert!(s.loaded, "empty stdin should still render and load");
    // And it still drives (no wedged main loop after EOF).
    h.execute_action("recolor", 1);
    h.wait_for_state("instant-EOF stdin still responds to actions", SETTLE, |s| {
        s.dark
    });
}

// ---------------------------------------------------------------------------
// Editor sync (DESIGN D7)
// ---------------------------------------------------------------------------

/// A source line well down the tall demo (the `### Mindmap` heading) — forwarding
/// to it must scroll a long way from the top.
const FORWARD_LINE: u32 = 250;

#[test]
fn goto_line_over_dbus_scrolls_to_the_source_line() {
    // Forward editor sync: the `GotoLine` D-Bus method scrolls the running
    // reader to the element nearest at-or-before the given source line.
    let Some((_g, h)) = setup() else { return };
    assert_eq!(h.get_state().scroll_y, 0.0, "starts at top");

    h.goto_line(FORWARD_LINE);
    let s = h.wait_for_state("GotoLine scrolls down", SETTLE, |s| s.scroll_y > 0.0);
    assert!(
        s.scroll_y > 100.0,
        "forwarding to a line deep in the document should scroll well down, got {}",
        s.scroll_y
    );
}

#[test]
fn forward_flag_jumps_after_load_on_fresh_launch() {
    // `jumanji --forward <line> file` with no running instance opens normally and
    // jumps to the line once the load finishes.
    let Some(_g) = setup_guard() else { return };
    let manifest = env!("CARGO_MANIFEST_DIR");
    let demo = Path::new(manifest).join("demo").join("demo.md");
    let id = std::process::id();
    let config_home = std::env::temp_dir().join(format!("jumanji-e2e-fwd-cfg-{id}"));
    let data_home = std::env::temp_dir().join(format!("jumanji-e2e-fwd-data-{id}"));

    let h = Harness::launch_in_forward(demo, config_home, data_home, Some(FORWARD_LINE), None);
    let s = h.wait_for_state("fresh --forward jumps after load", SETTLE, |s| {
        s.scroll_y > 0.0
    });
    assert!(
        s.scroll_y > 100.0,
        "a fresh --forward launch should land deep in the document, got {}",
        s.scroll_y
    );
}

#[test]
fn forward_to_a_running_instance_exits_without_a_window() {
    // With an instance already showing the file, a second `--forward` invocation
    // on the same bus must drive that instance over D-Bus and exit 0 quickly,
    // never opening a window of its own (zathura's --synctex-forward behaviour).
    let Some((_g, h)) = setup() else { return };
    assert_eq!(
        h.get_state().scroll_y,
        0.0,
        "running instance starts at top"
    );

    let start = Instant::now();
    let status = Command::new(env!("CARGO_BIN_EXE_jumanji"))
        .arg(&h.file)
        .args(["--forward", &FORWARD_LINE.to_string()])
        // Same private bus; deliberately no DISPLAY — the forward path returns
        // before any GTK/WebKit init, so it needs no X server.
        .env("DBUS_SESSION_BUS_ADDRESS", &h.dbus_addr)
        .env_remove("DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn second --forward");
    assert!(status.success(), "second --forward should exit 0");
    assert!(
        start.elapsed() < Duration::from_secs(8),
        "second --forward should exit promptly, took {:?}",
        start.elapsed()
    );

    // The already-running instance received the jump and scrolled.
    let s = h.wait_for_state("running instance scrolled via forward", SETTLE, |s| {
        s.scroll_y > 0.0
    });
    assert!(s.scroll_y > 100.0, "forwarded jump should scroll well down");
}

#[test]
fn reverse_ctrl_click_spawns_editor_command() {
    // Reverse editor sync: Ctrl+click on an element resolves its source line and
    // spawns `editor-command` with `%l`/`%f` substituted. Point editor-command at
    // a script that records its argv, click a paragraph, and assert the argv.
    let Some(_g) = setup_guard() else { return };

    let dir = std::env::temp_dir().join(format!("jumanji-e2e-rev-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let config_home = dir.join("cfg");
    let data_home = dir.join("data");
    let cfg = config_home.join("jumanji");
    std::fs::create_dir_all(&cfg).expect("create config dir");

    // A recorder: write the received argv (one per line) beside the script.
    let script = dir.join("record.sh");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/argv.txt\"\n",
    )
    .expect("write record script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    let argv_out = dir.join("argv.txt");

    std::fs::write(
        cfg.join("config.toml"),
        format!(
            "[options]\neditor-command = \"{} +%l %f\"\n",
            script.display()
        ),
    )
    .expect("write config");

    // A doc whose first paragraph is a long wrapped block, so a click anywhere in
    // its vertical band lands on text (never inter-block whitespace).
    let doc = dir.join("doc.md");
    let long = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do \
                eiusmod tempor incididunt ut labore et dolore magna aliqua ut enim \
                ad minim veniam quis nostrud exercitation ullamco laboris nisi.";
    std::fs::write(&doc, format!("# Title\n\n{long}\n\nSecond paragraph.\n")).expect("write doc");

    let h = Harness::launch_in(doc.clone(), config_home, data_home);

    // Click into the long paragraph (below the title, inside the reading column).
    h.ctrl_click(220, 160);

    // The script writes argv.txt on spawn; poll for it.
    let deadline = Instant::now() + SETTLE;
    let contents = loop {
        if let Ok(s) = std::fs::read_to_string(&argv_out)
            && !s.trim().is_empty()
        {
            break s;
        }
        if Instant::now() >= deadline {
            panic!("editor-command was not spawned (no argv.txt) within {SETTLE:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected `+<line>` and `<file>`, got {lines:?}"
    );
    assert!(
        lines[0].starts_with('+') && lines[0][1..].parse::<u32>().is_ok(),
        "first arg should be `+<line>`, got {:?}",
        lines[0]
    );
    assert_eq!(
        lines[1],
        doc.to_string_lossy(),
        "second arg should be the document path"
    );

    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Obsidian dialect (DESIGN D11)
// ---------------------------------------------------------------------------

/// Build a throwaway vault under the system temp dir and return its root.
/// A vault is just a directory of notes — there is no marker to create; what
/// makes it the vault is launching the reader *in* it (DESIGN D11).
fn temp_vault(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jumanji-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create vault");
    dir
}

/// Enough prose that a heading placed after it sits well below the fold.
fn filler() -> String {
    (1..=80)
        .map(|i| format!("Filler paragraph {i} padding the document past the fold.\n\n"))
        .collect()
}

#[test]
fn wikilink_follows_across_vault_notes() {
    // The checked-in fixture vault, entered the way a user would. It carries an
    // `.obsidian/` marker, so the index roots there — not at the surrounding
    // jumanji repo — and `[[Concepts/Callouts]]` resolves vault-wide. The
    // emitted `file://` link then routes through `open_uri` → `open_file` like
    // any other.
    // `Welcome.md`'s first link is that one, so the first hint label follows it.
    let Some(_g) = setup_guard() else { return };
    let manifest = env!("CARGO_MANIFEST_DIR");
    let vault = Path::new(manifest).join("demo").join("vault");
    let h = Harness::launch_file_in_dir(vault.join("Welcome.md"), Some(vault.clone()));
    assert!(
        h.get_state().file.ends_with("Welcome.md"),
        "starts on Welcome.md"
    );

    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");
    h.key(&["a"]);
    h.wait_for_state("wikilink opens the target note", SETTLE, |s| {
        s.mode == "normal" && s.file.ends_with("Callouts.md")
    });
}

#[test]
fn wikilink_heading_fragment_scrolls_target_document() {
    // The `pending_anchor` path: a cross-document fragment used to be dropped
    // (the file opened at the top). A one-link vault keeps the hint label
    // deterministic, so what is asserted is the anchor, not the hint ordering.
    let Some(_g) = setup_guard() else { return };
    let vault = temp_vault("frag");
    std::fs::write(
        vault.join("Source.md"),
        "# Source\n\nGo to [[Target#Folding]].\n",
    )
    .expect("write Source.md");
    std::fs::write(
        vault.join("Target.md"),
        format!("# Target\n\n{}## Folding\n\nLanded here.\n", filler()),
    )
    .expect("write Target.md");

    let h = Harness::launch_file_in_dir(vault.join("Source.md"), Some(vault.clone()));
    assert!(h.get_state().file.ends_with("Source.md"));

    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");
    h.key(&["a"]);
    h.wait_for_state(
        "fragment link opens the target *at the anchor*",
        SETTLE,
        |s| s.mode == "normal" && s.file.ends_with("Target.md") && s.scroll_y > 1.0,
    );

    drop(h);
    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn vault_root_follows_the_marker_not_the_working_directory() {
    // The whole point of marker-based rooting (DESIGN D11): a note opened from
    // deep inside a marked tree, with the process launched somewhere else
    // entirely, still resolves against the *vault*. `[[Welcome]]` lives in the
    // vault root, two levels above the document and nowhere near the CWD, so it
    // resolves only if the `.obsidian/` marker was honoured.
    let Some(_g) = setup_guard() else { return };
    let vault = temp_vault("marker-root");
    std::fs::create_dir_all(vault.join(".obsidian")).expect("marker");
    std::fs::create_dir_all(vault.join("Concepts/Nested")).expect("subfolders");
    std::fs::write(vault.join("Welcome.md"), "# Welcome\n\nArrived.\n").expect("write Welcome.md");
    std::fs::write(
        vault.join("Concepts/Nested/Deep.md"),
        "# Deep\n\nUp to [[Welcome]].\n",
    )
    .expect("write Deep.md");
    let elsewhere = temp_vault("marker-root-cwd");

    let h = Harness::launch_file_in_dir(
        vault.join("Concepts/Nested/Deep.md"),
        Some(elsewhere.clone()),
    );

    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");
    h.key(&["a"]);
    h.wait_for_state(
        "the wikilink resolved against the marked root",
        SETTLE,
        |s| s.mode == "normal" && s.file.ends_with("Welcome.md"),
    );

    drop(h);
    let _ = std::fs::remove_dir_all(&vault);
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn the_vault_index_skips_ignored_and_irrelevant_files() {
    // Two filters keep the index proportional to the vault rather than to the
    // tree it sits in (DESIGN D11): ignore files are obeyed, and only Obsidian's
    // accepted formats are indexed. Both matter most under the `.git/` fallback,
    // where the root can be a source repo.
    let Some(_g) = setup_guard() else { return };
    let vault = temp_vault("index-filters");
    std::fs::create_dir_all(vault.join("Private")).expect("subfolder");
    std::fs::create_dir_all(vault.join("build")).expect("subfolder");
    std::fs::write(vault.join(".gitignore"), "Private/\nbuild/\n").expect("write .gitignore");
    std::fs::write(
        vault.join("Doc.md"),
        "# Doc\n\nTo [[Secret]] and [[Public]].\n",
    )
    .expect("write Doc.md");
    std::fs::write(vault.join("Public.md"), "# Public\n\nArrived.\n").expect("write Public.md");
    std::fs::write(vault.join("Private/Secret.md"), "# Secret\n").expect("write Secret.md");
    // Not an accepted format, and not ignored either: filtered on its extension.
    std::fs::write(vault.join("main.rs"), "fn main() {}\n").expect("write main.rs");
    std::fs::write(vault.join("build/out.o"), "\0").expect("write out.o");

    let h = Harness::launch_file(vault.join("Doc.md"));

    // Exactly the two notes: `.gitignore` took `Private/` and `build/`, the
    // format filter took `main.rs`. `.gitignore` itself is hidden.
    h.wait_for_state("the background scan landed", SETTLE, |s| s.vault_files > 0);
    assert_eq!(h.get_state().vault_files, 2, "indexed files");

    // And the ignored note is genuinely unaddressable: an unresolved wikilink
    // gets no `href`, so it is not a hint target — the first hint is `[[Public]]`
    // even though `[[Secret]]` comes first in the source.
    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");
    h.key(&["a"]);
    h.wait_for_state("the first live link was Public, not Secret", SETTLE, |s| {
        s.mode == "normal" && s.file.ends_with("Public.md")
    });

    drop(h);
    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn a_note_created_after_launch_resolves_once_the_background_rescan_lands() {
    // The index is built off the main loop (DESIGN D11), so `r` cannot re-render
    // with a fresh index synchronously — it renders now and re-renders when the
    // scan lands. This is that whole round trip: a dead link becomes a live one
    // without the reader having done anything but ask for a reload.
    let Some(_g) = setup_guard() else { return };
    let vault = temp_vault("rescan-lands");
    std::fs::write(vault.join("Doc.md"), "# Doc\n\nOn to [[Later]].\n").expect("write Doc.md");

    let h = Harness::launch_file(vault.join("Doc.md"));
    h.wait_for_state("the initial scan landed", SETTLE, |s| s.vault_files == 1);

    // The target arrives after the window is already up.
    std::fs::write(vault.join("Later.md"), "# Later\n\nArrived.\n").expect("write Later.md");
    h.execute_action("reload", 1);
    h.wait_for_state("the rescan landed and re-rendered", SETTLE, |s| {
        s.vault_files == 2
    });

    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");
    h.key(&["a"]);
    h.wait_for_state("the once-dead wikilink resolved", SETTLE, |s| {
        s.mode == "normal" && s.file.ends_with("Later.md")
    });

    drop(h);
    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn mouse_side_buttons_walk_the_jumplist() {
    // Buttons 8/9 are the browser gesture, bound to the cross-document jumplist
    // (DESIGN D10) so a thumb click and `Ctrl-o`/`Ctrl-i` cannot disagree.
    let Some(_g) = setup_guard() else { return };
    let vault = temp_vault("side-buttons");
    std::fs::write(vault.join("Source.md"), "# Source\n\nOn to [[Target]].\n")
        .expect("write Source.md");
    std::fs::write(vault.join("Target.md"), "# Target\n\nArrived.\n").expect("write Target.md");

    let h = Harness::launch_file_in_dir(vault.join("Source.md"), Some(vault.clone()));

    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");
    h.key(&["a"]);
    h.wait_for_state("followed the link", SETTLE, |s| {
        s.mode == "normal" && s.file.ends_with("Target.md")
    });

    // Park the pointer over the window; the buttons are read at the pointer.
    h.mouse_move(200, 200);
    h.side_click(8);
    h.wait_for_state("back button returns to the source", SETTLE, |s| {
        s.file.ends_with("Source.md")
    });
    h.side_click(9);
    h.wait_for_state("forward button returns to the target", SETTLE, |s| {
        s.file.ends_with("Target.md")
    });

    drop(h);
    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn frontmatter_is_hidden_until_the_command_shows_it() {
    // Hidden is the default and the toggle is a full re-render, so this asserts
    // the whole round trip: absent → shown → absent again (DESIGN D11).
    let Some(_g) = setup_guard() else { return };
    let vault = temp_vault("frontmatter");
    let doc = vault.join("Note.md");
    std::fs::write(
        &doc,
        "---\ntitle: Properties\ntags: [alpha, beta]\n---\n\n# Note\n\nBody text.\n",
    )
    .expect("write Note.md");

    let h = Harness::launch_file_in_dir(doc, Some(vault.clone()));
    let s = h.wait_for_state("loaded", SETTLE, |s| s.loaded);
    assert_eq!(
        s.frontmatter_width, 0.0,
        "frontmatter is hidden by default, got {s:?}"
    );

    h.execute_action("toggle frontmatter", 1);
    let shown = h.wait_for_state("frontmatter panel rendered", SETTLE, |s| {
        s.frontmatter_width > 0.0
    });
    assert!(
        shown.frontmatter_width > 0.0,
        "`:frontmatter` shows the properties panel, got {shown:?}"
    );

    h.execute_action("toggle frontmatter", 1);
    h.wait_for_state("frontmatter hidden again", SETTLE, |s| {
        s.frontmatter_width == 0.0
    });

    drop(h);
    let _ = std::fs::remove_dir_all(&vault);
}

#[test]
fn unresolved_wikilink_is_not_hintable() {
    // An unresolved link carries no `href`, so the `a[href]` hint overlay never
    // sees it — dead by construction, not by a guard in the router. `[[Nowhere]]`
    // is written *first*, so an href would have claimed the `a` label.
    let Some(_g) = setup_guard() else { return };
    let vault = temp_vault("unresolved");
    std::fs::write(
        vault.join("Source.md"),
        "# Source\n\n[[Nowhere]] and then [[Target]].\n",
    )
    .expect("write Source.md");
    std::fs::write(vault.join("Target.md"), "# Target\n\nArrived.\n").expect("write Target.md");

    let h = Harness::launch_file_in_dir(vault.join("Source.md"), Some(vault.clone()));

    h.key(&["f"]);
    h.wait_for_state("hint overlay active", SETTLE, |s| s.mode == "hint");
    h.key(&["a"]);
    h.wait_for_state("the first hint is the resolvable link", SETTLE, |s| {
        s.mode == "normal" && s.file.ends_with("Target.md")
    });

    drop(h);
    let _ = std::fs::remove_dir_all(&vault);
}
