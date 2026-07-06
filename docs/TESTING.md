# Testing

Two layers:

- **Unit tests** (`src/core/**`) — the pure functional core (pipeline, TOC,
  config, keymap). Run everywhere, need no display: `cargo test --lib` (or the
  full `cargo test`).
- **Headless end-to-end** (`tests/e2e.rs`) — drives the *real* application: a
  real (virtual) X server, real GTK key events, real WebKit, asserting on state
  read back over D-Bus. This document is about that layer.

## Running

```sh
cargo test --test e2e          # just the e2e suite
cargo test                     # core unit tests + e2e
```

Each test is fully isolated (its own `Xvfb` display + its own private session
bus) and cleans up after itself even on panic, so the suite never touches your
live desktop or session bus. The tests are **serialized** behind a process-wide
mutex — seven concurrent WebKit instances thrash a loaded machine and make
timing flaky — so `--test-threads` has no effect on them.

Typical wall-clock: ~9–10 s for all seven on a fast machine (each spins up and
tears down a WebKit instance).

## System requirements & the skip gate

The harness shells out to three tools. On Arch:

```sh
sudo pacman -S xorg-server-xvfb xdotool dbus
```

If any of `Xvfb`, `xdotool`, or `dbus-daemon` is missing from `PATH`, **the
suite skips**: it prints a one-line notice to stderr and every test passes as a
no-op. CI and developer machines without a display therefore never fail on e2e.
The file is also gated `#![cfg(unix)]`.

## What it covers

Each test injects keys (or calls a D-Bus method) and then polls `GetState`
until the expected state appears (or a ~5 s timeout fails with the last observed
state):

| Test | Exercises |
|---|---|
| `j_and_k_scroll` | `j` scrolls down, `k` scrolls back up |
| `count_multiplies_scroll` | `5j` scrolls ~5× a single `j` (delta comparison) |
| `g_jumps_to_bottom_and_top` | `G` → 100 %, `gg` → top |
| `ctrl_r_toggles_dark` | `Ctrl-r` toggles dark mode on/off |
| `zoom_in_and_reset` | `+` raises zoom, `=` resets to 1.0 |
| `section_next_and_previous` | `J` next section, `K` previous |
| `execute_action_scrolls_without_keys` | `ExecuteAction("scroll down", 3)` — pure D-Bus, no key injection |

## The D-Bus interface is the automation / editor-sync surface

The tests don't use a back door. They exercise the same per-instance D-Bus
service (`src/shell/dbus.rs`) that is the foundation for the M3 editor-sync
feature (DESIGN.md D7). Each running reader owns

- **name** `org.membranepotential.jumanji.PID-<pid>` on the session bus,
- **object** `/org/membranepotential/jumanji`,
- **interface** `org.membranepotential.jumanji`,

with two methods:

- `GetState() -> (s)` — a JSON snapshot: `file`, `scroll_y`, `scroll_percent`,
  `dark`, `zoom`, `mode`, `section`, `toc_len`, `loaded`. Scroll figures are
  queried live from the webview (async JS); the reply is completed from the JS
  callback, so the main loop never blocks.
- `ExecuteAction(s action, u count)` — parses an action string with the config
  action parser (`core::config::parse_action`) and runs it through the exact
  same `execute()` path the keyboard uses. An unknown action string returns a
  D-Bus error, not a crash. This is the full action vocabulary, available to
  tests today and to editor integrations tomorrow.

You can poke a running instance by hand:

```sh
PID=$(pgrep -n jumanji)
DEST=org.membranepotential.jumanji.PID-$PID
gdbus call --session --dest $DEST \
  --object-path /org/membranepotential/jumanji \
  --method org.membranepotential.jumanji.GetState
gdbus call --session --dest $DEST \
  --object-path /org/membranepotential/jumanji \
  --method org.membranepotential.jumanji.ExecuteAction "scroll down" 3
```

## How the harness works (and two gotchas)

`tests/e2e.rs` is a small self-contained module:

1. **Xvfb** — a free display number is chosen (offset by pid to avoid clashing
   with other `cargo test` runs), `Xvfb :N -screen 0 1280x1024x24` is spawned,
   and we wait for its `/tmp/.X11-unix/XN` socket.
2. **Private bus** — `dbus-daemon --session --print-address=1` is spawned with
   its address read off stdout; `DBUS_SESSION_BUS_ADDRESS` + `DISPLAY` are
   passed to the app child only.
3. **App** — launched via `env!("CARGO_BIN_EXE_jumanji")` against `demo/demo.md`.
4. **Wait for `loaded`** — poll `GetState` until `loaded: true` (this is why the
   flag exists: keys injected before the initial load are silently dropped).
5. **Key injection** — the window is matched by **WM_CLASS** (`jumanji`), never
   by title (titles collide with terminal windows on a dev machine).

Two things that will bite you if you extend the harness:

- **You must focus the window first.** Under a bare Xvfb there is no window
  manager, so nothing gives the window X input focus, and GTK4 silently drops
  synthetic key events aimed at an unfocused window. The harness runs
  `xdotool windowfocus --sync <id>` once at startup; only then do
  `xdotool key --window <id> …` injections land. (`windowactivate` does *not*
  work — it needs EWMH, which requires a WM.)
- **Shift needs the explicit `shift+` form.** `xdotool key G` does not reliably
  deliver a shifted keysym under Xvfb; use `shift+g`, `shift+j`, `shift+k`.

## Adding a new e2e case

1. Add a `#[test] fn my_case() { let Some((_g, h)) = setup() else { return }; … }`.
   `setup()` acquires the serialization lock and launches a focused harness, or
   returns `None` (skip) when tools are missing — always early-return on `None`.
2. Drive the app with `h.key(&["…"])` (xdotool key syntax) or
   `h.execute_action("…", n)` (pure D-Bus).
3. Assert by polling, not sleeping: `h.wait_for_state("what you expect", SETTLE,
   |s| …)` returns the first state matching the predicate, or panics with the
   last observed state on timeout. Prefer a predicate over a fixed delay so the
   test is robust to a slow, loaded machine.
4. If your action needs a new observable, add the field to the `GetState` JSON
   in `src/shell/app.rs` (`state_json`) and to the `State` struct + parser here.
