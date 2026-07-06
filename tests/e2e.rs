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
use std::io::{BufRead, BufReader};
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
    scroll_y: f64,
    scroll_percent: u32,
    /// Layout width of the content column in CSS px — invariant under
    /// geometric zoom (the D5a no-reflow guarantee).
    content_width: f64,
    dark: bool,
    zoom: f64,
    text_zoom: f64,
    #[allow(dead_code)]
    mode: String,
    section: usize,
    toc_len: usize,
    loaded: bool,
}

impl State {
    /// Parse the flat JSON object `GetState` returns. Deliberately tiny: the
    /// object is flat, so a real JSON dependency would be over-engineering.
    fn parse(json: &str) -> Option<Self> {
        Some(State {
            file: field_str(json, "file")?,
            scroll_y: field(json, "scroll_y")?.parse().ok()?,
            scroll_percent: field(json, "scroll_percent")?.parse().ok()?,
            content_width: field(json, "content_width")?.parse().ok()?,
            dark: field(json, "dark")? == "true",
            zoom: field(json, "zoom")?.parse().ok()?,
            text_zoom: field(json, "text_zoom")?.parse().ok()?,
            mode: field_str(json, "mode")?,
            section: field(json, "section")?.parse().ok()?,
            toc_len: field(json, "toc_len")?.parse().ok()?,
            loaded: field(json, "loaded")? == "true",
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

/// Same, for a string value: strips the surrounding quotes.
fn field_str(json: &str, key: &str) -> Option<String> {
    let raw = field(json, key)?;
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.to_string())
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
    /// the live-reload test, which mutates a throwaway copy.
    fn launch_file(file: PathBuf) -> Self {
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

        // Isolate the app from the developer's real ~/.config/jumanji: an empty
        // private XDG_CONFIG_HOME means every test runs on default options.
        let config_home = std::env::temp_dir().join(format!("jumanji-e2e-xdg-{display}"));
        let _ = fs::create_dir_all(&config_home);

        let app = Command::new(env!("CARGO_BIN_EXE_jumanji"))
            .arg(&file)
            .env("DISPLAY", &display_arg)
            .env("DBUS_SESSION_BUS_ADDRESS", &dbus_addr)
            .env("XDG_CONFIG_HOME", &config_home)
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
fn narrow_viewport_zoom_does_not_reflow() {
    // Regression: on a viewport narrower than `page-width`, geometric zoom used
    // to *reflow* — the column re-fit the shrunken CSS viewport, so diagrams
    // never got visually bigger and the reading position drifted. The fix pins
    // the layout width (CSS px) via `--zoom`, so it must not change when
    // zooming; the content instead overflows the viewport (device px), which is
    // exactly what makes a full-width mermaid diagram scale.
    let Some((_g, h)) = setup() else { return };

    // Shrink the window well below page-width (720) so the column is
    // viewport-constrained. No WM under Xvfb, so resize the X window directly.
    h.xdotool(["windowsize", "--sync", &h.window_id, "500", "800"]);
    h.wait_for_state("window narrowed", SETTLE, |s| {
        s.content_width > 0.0 && s.content_width < 600.0
    });
    // Let the resize settle: the baseline must be the final width, not a
    // mid-resize snapshot, or the invariance assert below compares junk.
    let narrow = {
        let prev = std::cell::Cell::new(-1.0_f64);
        h.wait_for_state("width stable after resize", SETTLE, move |s| {
            let stable = (s.content_width - prev.get()).abs() < 1.0;
            prev.set(s.content_width);
            stable
        })
    };

    // Scroll into the document, then zoom in hard.
    h.execute_action("scroll down", 10);
    h.wait_for_state("scrolled", SETTLE, |s| s.scroll_y > 0.0);
    h.execute_action("zoom in", 5);
    // The layout width (CSS px) must come back to its zoom-1 value: reflow-free
    // zoom. Polling (not first-snapshot) because the `--zoom` property is set
    // by an async JS eval.
    let zoomed = h.wait_for_state("layout width zoom-invariant (no reflow)", SETTLE, |s| {
        s.zoom > 1.4 && (s.content_width - narrow.content_width).abs() < 2.0
    });
    // Device-pixel width = layout width × zoom now exceeds its zoom-1 value:
    // the column (diagrams included) really is rendered larger, not re-fit.
    assert!(
        zoomed.content_width * zoomed.zoom > narrow.content_width * 1.3,
        "zoomed content should be rendered wider than the viewport: \
         {} css px × {} zoom vs {} at zoom 1",
        zoomed.content_width,
        zoomed.zoom,
        narrow.content_width
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

    // Turn on dark mode, then append a new heading to trigger a real reload.
    h.execute_action("recolor", 1);
    h.wait_for_state("dark enabled", SETTLE, |s| s.dark);

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

    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}
