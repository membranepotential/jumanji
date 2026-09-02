# Testing

Two layers:

- **Unit tests** (`src/core/**`) — the pure functional core (pipeline, TOC,
  config, keymap, Obsidian dialect). Run everywhere, need no display:
  `cargo test --bin jumanji` (jumanji is a binary crate — there is no `--lib`
  target), or just the full `cargo test`.
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
mutex — concurrent WebKit instances thrash a loaded machine and make
timing flaky — so `--test-threads` has no effect on them.

Typical wall-clock: ~50 s for all 39 on a fast machine (each spins up and
tears down a WebKit instance).

## Fixtures

- `demo/demo.md` — the general fixture the bare `setup()` harness launches.
- `demo/links.md` — one internal link, for deterministic hint-follow testing.
- **`demo/vault/`** — the D11 dialect fixture: `Welcome.md`,
  `Concepts/Callouts.md`, an `Aliased Note.md` whose filename has a space, and
  `attachments/diagram.png`. There is no marker file — a vault is just a
  directory of notes.

**Wikilink tests must set the child's working directory.** The vault index is
rooted at the process CWD (DESIGN D11), so a test that launches the reader from
the repo root indexes the repo, not the fixture, and `[[Concepts/Callouts]]`
comes out unresolved. Use `Harness::launch_file_in_dir(file, Some(dir))`, which
sets `Command::current_dir`. Tests that also need to control link *ordering*
(which link gets the `a` hint label) build a throwaway vault under the temp dir
— see `temp_vault`.

Note that `Welcome.md` puts its link list immediately under the title **on
purpose**: the hint overlay only labels links inside the viewport, so a link
pushed below the fold takes no label and the test would see "no links in view".

### The growing fixture (`growing_fixture`)

Generated into a temp dir rather than checked in, because it is a *timing*
fixture, not content: a spacer `<div>` whose height a CSS `@keyframes`
animation drives from 0 to 8000 px over `GROW_MS` (200 ms), so the document
keeps growing for ~200 ms **after** `readyState === 'complete'`.

That is the one condition under which the restore gate does any work. Every
other fixture reaches its final height within a frame or two of `complete`, so
a restore always lands on the first try — which is why the suite could not see
the v1.7.0 document-switch flash at all, and a green run proved only "no
regression". While the document is short, `window.scrollTo(0, deep)` clamps to
near the top; a gate that conceded there would reveal the flash.

Mechanics worth knowing before editing it:

- **No JS is involved.** The page CSP allows `style-src 'unsafe-inline'` and
  comrak renders raw HTML blocks verbatim, so an inline `<style>` is the one
  lever a fixture has over layout timing. `visibility: hidden` (the gate) does
  not stop animations, and animating `height` is layout-affecting, so
  `scrollHeight` genuinely climbs frame by frame.
- **`GROW_MS` is bounded on both sides.** It must outlast `complete` by several
  frames (or the gate is right to concede and the test proves nothing), and the
  whole restore must still finish inside the restore script's unconditional
  400 ms failsafe (or a *correct* gate reveals by timer). Measured on a quiet
  machine and with every core saturated: reveal at the exact target, first
  painted frame at ~34% of it — comfortable margins at both ends.
- **The tests wait the growth out before measuring the bottom** — `G` pressed
  mid-growth lands on a bottom that then moves. `wait_out_growth()` plus a
  `scroll_percent == 100` predicate is the guard.

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
| `geometric_zoom_in_and_reset` | `zoom in` (D-Bus) raises geometric `zoom`, `zoom reset` clears it |
| `text_zoom_changes_and_reset_clears_both_axes` | `+` raises `text_zoom`; `=` resets *both* `zoom` and `text_zoom` to 100 % |
| `section_next_and_previous` | `J` next section, `K` previous |
| `execute_action_scrolls_without_keys` | `ExecuteAction("scroll down", 3)` — pure D-Bus, no key injection |
| `external_reads_do_not_storm_reload` | scroll, `fs::read` the file ×5, wait 1.5 s → scroll unchanged (reload-loop regression) |
| `live_reload_grows_toc_and_preserves_dark` | append a heading to a temp-dir copy → TOC grows and dark mode survives the reload |
| `wikilink_follows_across_vault_notes` | launched *inside* `demo/vault`, `f`+`a` on `Welcome.md` opens `Concepts/Callouts.md` — CWD rooting, indexing and `[[…]]` routing end to end |
| `wikilink_heading_fragment_scrolls_target_document` | `[[Target#Folding]]` opens the target **and** scrolls to the anchor (the `pending_anchor` path) |
| `unresolved_wikilink_is_not_hintable` | an unresolved `[[…]]` carries no `href`, so the hint overlay skips it and `a` lands on the resolvable link |
| `reload_of_a_growing_document_reveals_only_at_the_restored_offset` | live reload of the growing fixture: the body is unhidden only once the preserved offset is reached, never by the failsafe |
| `jumplist_return_to_a_growing_document_reveals_only_at_the_stored_offset` | the same, on the reported flash path — `Ctrl-o` back into a document last read at the bottom, loaded from scratch |

## Performance: the third layer

The suite proves behaviour; three measurements prove the reader has not got
slower. All live in the repo and all run in CI (`.github/workflows/bench.yml`).

- **`scripts/bench-instructions.sh`** — instructions retired per render, per
  fixture, counted by valgrind's cachegrind through the bench binary's
  `--once NAME --repeat K` mode (two runs differing by exactly K renders, so
  start-up and one-time init cancel). **Deterministic**: runner type and CPU
  frequency do not move it, so it is the number that *gates* — a 3 % change
  is a change in the code. Needs `valgrind` (Arch: `pacman -S valgrind`).
- **`cargo bench --bench pipeline`** — criterion wall clock over
  `core::pipeline::render` for the same shapes (`benches/pipeline.rs`). The
  felt number, but it moves with the machine: ±30 % between shared-runner
  instances, up to 2× on a throttling laptop — measured on byte-identical
  binaries. Informational.
- **`scripts/bench-startup.sh`** — wall clock from process spawn until the
  D-Bus `GetState` reports `loaded: true`, headless (private Xvfb + bus per
  run), median over N runs, for `demo/demo.md` and a generated wikilink-heavy
  vault. This is the number the reader is judged on: it spans WebKit's process
  spawn, the pipeline, and the shell's own startup path. `-b BIN` measures a
  given binary; `-j FILE` writes the medians as JSON.

### Checking a change against a baseline

```sh
scripts/bench-compare.sh            # latest tag vs. the working tree
scripts/bench-compare.sh v1.8.0     # or any ref
```

It builds the ref in a throwaway worktree (`.bench-baseline/`, its own
`target-baseline/`), then prints two tables: startup time, both binaries
**interleaved** run by run so a machine that warms or cools mid-bench skews
both sides equally; and pipeline instruction counts for both bench binaries,
which need no interleaving because they do not drift.

Why not criterion for the A/B: on the development laptop (governor
`powersave`, cores anywhere between 0.8 and 2.8 GHz) byte-identical binaries
read anywhere from −10 % to +140 % against each other, in every ordering
tried — whole suites back to back, per-bench alternation, three alternations
with medians. Wall clock is not a fair instrument for a 5 % question on that
machine; instruction counts are. Run the startup half on a quiet machine — no
builds, no test suites, nothing in the background. A refactor of the shell
should show startup inside the run-to-run noise and pipeline instructions at
0.00 %; a consistent delta on one fixture is a finding, not a rounding error.

### In CI

`bench.yml` runs both on every push to `main` and every pull request:

- the trail is a JSON history per bench carried forward as a workflow
  artifact (`bench-trail`): a run on `main` restores it from the previous
  successful `main` run, appends its result, and uploads it again. No branch
  holds it and nothing is committed by a bot — a data-only branch shares no
  history with `main` and is not what branches are for. Artifacts expire
  after 90 days, so the chain lives as long as `main` is pushed within that
  window; to keep a point forever, commit a snapshot at release time;
- every run uploads its raw output (the criterion report, the bencher-format
  lines, the startup JSON) as its own artifact (`bench-raw-<sha>`);
- a PR gets an alert comment when a measurement regresses past its
  threshold: the pipeline **instruction counts fail the job** at 105 %; the
  two wall-clock measurements only comment (criterion at 130 %, startup at
  150 %), because shared runners move them ±30 % on their own. The job
  summary shows all the numbers either way.

## The D-Bus interface is the automation / editor-sync surface

The tests don't use a back door. They exercise the same per-instance D-Bus
service (`src/shell/dbus.rs`) that is the foundation for the M3 editor-sync
feature (DESIGN.md D7). Each running reader owns

- **name** `org.membranepotential.jumanji.PID-<pid>` on the session bus,
- **object** `/org/membranepotential/jumanji`,
- **interface** `org.membranepotential.jumanji`,

with two methods:

- `GetState() -> (s)` — a JSON snapshot: `file`, `scroll_y`, `scroll_percent`,
  `dark`, `zoom` (geometric), `text_zoom`, `mode`, `section`, `toc_len`,
  `loaded`, plus the restore-gate trio — `first_frame_scroll_y` (the offset the
  first *painted* frame was placed at, hidden or not), `reveal_scroll_y` (the
  offset the body was *unhidden* at: the first frame the reader can see) and
  `reveal_failsafe` (whether the 400 ms timer, not the position landing, is
  what unhid it). For a document that finishes laying out immediately the first
  two coincide; for one still growing they do not, and only the reveal figure
  can tell a flash from a hidden frame nobody saw. Scroll figures are
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
