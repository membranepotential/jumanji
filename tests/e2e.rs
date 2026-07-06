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
    dark: bool,
    zoom: f64,
    text_zoom: f64,
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
            viewport_width: field(json, "viewport_width")?.parse().ok()?,
            doc_scroll_width: field(json, "doc_scroll_width")?.parse().ok()?,
            diagram_width: field(json, "diagram_width")?.parse().ok()?,
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
    /// the live-reload test, which mutates a throwaway copy. Fresh private XDG
    /// dirs are allocated per launch.
    fn launch_file(file: PathBuf) -> Self {
        let id = next_display();
        let config_home = std::env::temp_dir().join(format!("jumanji-e2e-xdg-{id}"));
        let data_home = std::env::temp_dir().join(format!("jumanji-e2e-data-{id}"));
        Self::launch_in(file, config_home, data_home)
    }

    /// Launch on `file` with explicit private `config_home`/`data_home` dirs. The
    /// data home holds `history.toml`, so a relaunch on the same data home
    /// exercises window-state persistence.
    fn launch_in(file: PathBuf, config_home: PathBuf, data_home: PathBuf) -> Self {
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

        let app = Command::new(env!("CARGO_BIN_EXE_jumanji"))
            .arg(&file)
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

    h.execute_action("scroll down", 14);
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
    }

    let _ = std::fs::remove_dir_all(&dir);
}
