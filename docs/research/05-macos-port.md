# Research: the macOS port proposal (issue #1)

Date: 2026-09-02. Evaluates
[issue #1](https://github.com/membranepotential/jumanji/issues/1), "macOS
support: analysis and a proposed spike", against the code as of v1.8.0 and
against current upstream facts. The issue's claims were re-verified rather than
taken on trust; see §3.

**Verdict in one line:** feasible and worth doing — *provided* the toolkit-agnostic
controller is extracted first as a Linux-only refactor judged on its own merits,
the macOS shell stays cfg-gated and tier-2, and the parity gaps in §4 are
written down rather than discovered.

## 1. What the issue proposes

Author: an external contributor who wants to run jumanji on macOS. Owner reply
(2026-09-02): happy to collaborate, will extract the shared parts and scaffold
the macOS shell, keep e2e + benchmarks green, work via PRs, add CI workflows.

**Blocker.** `src/core/` is portable; the shell is not, and WebKitGTK 6.0 (the
GTK4 API line `webkit6` wraps) has no macOS build. GTK4 itself runs on macOS
but has no webview there.

**Options considered by the issue, and its verdicts:**

| Option | Verdict | Reason |
|---|---|---|
| Linux VM + X11 forwarding | discard | works, isn't "running on Mac" |
| Wait for WebKitGTK on macOS | discard | not coming |
| Tauri | discard | wraps wry/tao; adds an IPC/permission/bundler layer for JS-driven apps |
| Unify both platforms on wry | **reject** | wry's Linux backend is webkit2gtk-4.1 (GTK3); hides FindController and capture-phase keys; Linux would regress |
| Second, macOS-only shell on tao + wry (WKWebView) | **proposed** | the only path that holds up |

**Shape of the port:**

1. Extract the toolkit-agnostic controller out of `shell/app.rs` (mode machine,
   action dispatch, jumplist/history/quickmarks, session) behind a small
   `Viewport` trait; the GTK shell becomes a thin adapter. Stands on its own.
2. A macOS shell on tao (window/event loop) + wry (WKWebView), compiled only for
   `target_os = "macos"`. GTK4 stays canonical on Linux, zero behaviour change.

**Stays / changes (the issue's table, condensed):**

- Unchanged and shared: `src/core/`, the rendered HTML/CSS, CSP, the five shell
  user scripts (via a `messageHandlers` → IPC polyfill), the mode machine and
  dispatch, jumplist/history/quickmarks.
- Linux-only, untouched: GTK4 widgets, native FindController, PRIMARY
  selection, D-Bus, the Xvfb/xdotool e2e harness, the Mesa/X11 workaround.
- macOS-only, new: wry/tao window + custom protocol; JS find-in-page (CSS
  Custom Highlight API); keyboard via a capture-phase JS listener feeding the
  core keymap; statusbar/inputbar/TOC as in-page overlays; single-instance and
  automation either a unix socket or dropped.

**Risk mitigation offered:** hard `cfg` isolation (Linux builds never compile
the mac shell); everything shared stays under the Linux suite; deletion is one
directory plus one cfg gate; a GitHub Actions `macos` job for compile
visibility; README tier note ("community-contributed, may lag, may be
dropped"); DESIGN.md amendments argued in the PR.

**The ask:** run a half-day spike on four assumptions and post results:
(a) `evaluate_script_with_callback` returns structured results on macOS,
(b) wry `zoom()` reaches WKWebView's page zoom, (c) CSS Custom Highlight API
in the system WKWebView, (d) init-script + IPC round trip. Then, if green, a
first PR — the controller extraction being the natural candidate.

## 2. How the shell actually decomposes today

The issue assumes the shell is "thin" in the DESIGN.md sense. It is not, and
that is the crux of the work:

| File | Lines | GTK/glib/webkit6 coupling |
|---|---|---|
| `shell/app.rs` | ~2,350 | 27 lines of direct GTK use; but every function takes `Rc<RefCell<Shell>>`, and `Shell` owns GTK handles (`ApplicationWindow`, `Stack`, `Bar`, `TocView`, `View`) |
| `shell/view.rs` | ~1,250 | webkit6 `WebView`, `UserContentManager`, `FindController`; ~500 lines are JS user-script strings |
| `shell/bar.rs`, `shell/toc.rs` | ~450 | pure GTK widgets |
| `shell/dbus.rs` | ~250 | gio D-Bus; already sees no `Shell` type (semantics injected as closures) |
| `shell/watch.rs`, `shell/stdin.rs` | ~200 | `notify` (portable) + a `glib` poll source to marshal onto the main loop |

**What is already toolkit-agnostic in shape.** `execute()` (~240 lines) is a
`match` over `core::Action` whose every effect goes through a method on
`View`, `Bar`, or `TocView` with a plain-Rust signature (`scroll_by(i64, i64)`,
`scroll_to_anchor(&str)`, `zoom_to(f64, ZoomAnchor)`, `find(&str)`,
`open_input(Prompt)`, `move_selection(i32)` …). The key handler in
`connect_keys` is four steps of pure dispatch logic (hint interception →
universal abort → input-bar passthrough/Tab completion → matcher) wrapped in a
20-line GTK adapter (`to_keypress`, `Propagation`). `View`'s public surface is
in effect the `Viewport` trait already; the issue's "small trait" is a
transcription.

**`Shell`'s fields split cleanly.** Of ~45 fields, ~35 are session state
(file/stdin buffer, options, vault index + scan flags, deferred-render flag,
pending position, zoom axes + coalescing, matcher + mode, toc + section, dark,
loaded, hint input, completion cycle, jumplist, marks, history). The remaining
handles are the toolkit objects (`view`, `toc_view`, `stack`, `bar`, `window`,
`pointer`) and the owned background sources (`_watch`, `_theme_watch`,
`_stdin`, `_dbus`).

**What genuinely needs an abstraction beyond the viewport:**

- **Main-loop scheduling.** `glib::timeout_add_local_once` (initial-render
  failsafe, wheel-zoom coalescing), `glib::spawn_future_local` (background
  vault scan), and the `glib` poll sources in `watch.rs`/`stdin.rs`. One small
  trait: defer-once, spawn-blocking-then-continue, and a channel drain.
- **Chrome.** `Bar` (status left/right, transient message, breadcrumb trail,
  input entry with prompt/text/submit/cancel) and `TocView` (a selection over
  a collapsible tree). On GTK these are widgets; on macOS they are in-page
  overlays. The controller must see them as an interface, not as widgets.
- **The user scripts' IPC seam.** All five scripts post through
  `window.webkit.messageHandlers.<name>.postMessage(msg)` (`view.rs:979`,
  `1034`, `1130`, `1164`, plus the restore script's globals). Replace with a
  shell-provided `window.__jmnj_post(name, msg)` that each shell defines in
  its own init script; then the scripts move to a shared module byte-for-byte.
- **Platform services.** `gio::AppInfo::launch_default_for_uri` (external
  links), the PRIMARY/CLIPBOARD selection copy, XDG dirs, and D-Bus. Each is a
  one-method adapter hook or Linux-only.

**Conclusion of §2:** the extraction is real work — roughly 1,600 lines of
`app.rs` and ~500 lines of JS move, ~700 lines of GTK wiring stay — but it is
mechanical along boundaries the code already respects. No new state, no new
concepts; the `Rc<RefCell<…>>` + async-callback pattern carries over unchanged
because tao's event loop is equally single-threaded and main-thread-bound.

## 3. The issue's technical claims, re-verified

Checked 2026-09-02 against crates.io, docs.rs, caniuse, Homebrew/MacPorts, the
tao/wry trackers and the GitHub runner-image changelog. Everything the issue
asserts holds; two items sharpen it (marked ▲).

| Claim | Verdict | Evidence |
|---|---|---|
| WebKitGTK 6.0 has no macOS build | confirmed | Homebrew `webkitgtk` 2.52.6 is Linux-only bottles, "Requires: Linux"; MacPorts carries only the 2.4/2.28 GTK3-era ports ([brew](https://formulae.brew.sh/formula/webkitgtk), [MacPorts](https://ports.macports.org/port/webkit2-gtk/)) |
| wry's Linux backend is webkit2gtk-4.1 / GTK3 | confirmed | wry 0.56.1 (2026-08-13) pins `webkit2gtk 2.0`, `gtk 0.18`; GTK4 is open issue [wry#1474](https://github.com/tauri-apps/wry/issues/1474), status Todo. tao is 0.37.0 (2026-08-21) |
| `evaluate_script_with_callback` returns structured results | confirmed | result arrives as a JSON-serialized string, all platforms ([docs.rs](https://docs.rs/wry/latest/wry/struct.WebView.html)) |
| wry `zoom()` reaches WKWebView page zoom | confirmed | documented macOS 11+; maps to `pageZoom` (same semantics as webkit6 `zoom_level` with text-only off), not window magnification |
| Custom protocol on macOS | confirmed | `with_custom_protocol` / `with_asynchronous_custom_protocol` via `WKURLSchemeHandler` ([docs.rs](https://docs.rs/wry/latest/wry/struct.WebViewBuilder.html)) |
| Init script + IPC round trip | confirmed | `with_initialization_script`, `with_ipc_handler`; on WKWebView `window.ipc.postMessage` is a shim over `webkit.messageHandlers.ipc` — which is why the shared scripts should post through a shell-defined `__jmnj_post`, not through `messageHandlers` directly |
| No native find-in-page via wry | confirmed | wry's `WebView` exposes no find API at all; WKWebView's public `find(_:configuration:)` (macOS 11+) is unwrapped, would need objc FFI. JS find is the realistic route |
| CSS Custom Highlight API in system WKWebView | confirmed ▲ | shipped in **Safari 17.2 = macOS 14.2** (Dec 2023) ([caniuse](https://caniuse.com/mdn-api_highlight)). The mac shell's floor is therefore macOS 14.2, not "any WKWebView" |
| WKWebView swallows key events before the tao window sees them | confirmed ▲ | a recurring issue cluster, not one bug: [tao#208](https://github.com/tauri-apps/tao/issues/208) (no keys until clicked), [tao#940](https://github.com/tauri-apps/tao/issues/940), [wry#184](https://github.com/tauri-apps/wry/issues/184), [tauri#5662](https://github.com/tauri-apps/tauri/issues/5662). The in-page listener sidesteps the event-routing half; the focus-handoff half (window up, webview not first responder) still needs care in the shell |
| `loadHTMLString:baseURL:` grants no local file read | confirmed (secondary sources) | Apple forum threads + write-ups; the sanctioned route is `loadFileURL:allowingReadAccessToURL:` or a scheme handler — the issue's custom protocol is the right call |
| `macos` GitHub runners free for public repos | confirmed | repo is public; `macos-latest` is macOS 26 as of mid-2026, `macos-15`/`macos-26` labels available ([changelog](https://github.blog/changelog/2026-05-14-github-actions-upcoming-image-migrations/)) |
| Ubuntu runners can build the Linux shell | confirmed | `ubuntu-latest` = 24.04; noble ships `libgtk-4-dev` 4.14.2 and `libwebkitgtk-6.0-dev`; crate minimums are gtk 4.14 / webkitgtk 2.40 |

**Not in the issue, checked anyway:** there is no GTK4-native webview for macOS
and none coming. The one new entrant is Servo's embedding crate (`servo` 0.1.0,
April 2026; API explicitly unstable, LTS cadence promised) — a plausible
*future* single-shell path, not a 2026 one. For criterion in CI,
`benchmark-action/github-action-benchmark` v1.22.1 (2026-05) is current and
supports criterion output, if a trend chart is ever wanted beyond the artifact
upload in §5.1.

## 4. Feasibility and sensibility

### Feasible — yes

- The architecture the port needs is the one DESIGN D2 already reserves as the
  "escape hatch": the pipeline is UI-independent, so a different front end can
  replace the shell. D2 assumed the shell would stay thin; it didn't, which is
  why step 1 (extraction) exists. Nothing in core, the HTML contract
  (`data-jmnj-open`, `html.dark`, `jmnj-restoring`, `--font-size`), or the CSP
  changes.
- The dependency split is clean at the manifest: `gtk4` + `webkit6` become
  `[target.'cfg(not(target_os = "macos"))'.dependencies]`, `tao` + `wry`
  the mirror. `main.rs` needs a cfg split (it uses `glib::ExitCode` and the
  D-Bus forward shortcut). `tests/e2e.rs` must gate on `target_os = "linux"`
  rather than `unix` (it links gio). Benches are core-only and unaffected.
- The Linux safety net is strong: 50 e2e cases over the real app plus the core
  unit suite. The extraction can proceed in stages that each stay green.

### Sensible — yes, under four conditions

1. **Extraction first, as a Linux refactor judged on Linux merits.** It pays
   for itself regardless of macOS: `app.rs` is the least-tested code in the
   repo (only via the ~50 s Xvfb suite). A controller driven through traits
   gets a fake viewport and fast unit tests for hint mode, jumplist,
   completion, and the mode machine — flows that today need a real WebKit.
2. **Tier 2, cfg-gated, never on the Linux release path.** Linux `cargo
   build/test/clippy` compile none of it. A macOS CI job reports compile
   breakage; it must not block Linux merges. README states the tier.
3. **Parity gaps written down up front.** These are not bugs to fix later; they
   are the shape of the mac shell:
   - **No editor pairing on macOS.** D7 (`--forward`, `GotoLine`, the Neovim
     plugin) rides on D-Bus. A unix-socket equivalent is a separate feature
     the contributor would have to build and the plugin would have to learn.
   - **The vim layer is no longer "absolute" (D4).** Keys arrive via a
     capture-phase JS listener inside WKWebView, not before it. Acceptable for
     a static document with no focusable inputs, but it is a weaker guarantee —
     and the tao/wry focus-handoff bugs (§3) mean "window open, no keys until
     the user clicks" is a failure mode the mac shell must actively defend.
   - **macOS 14.2 or newer.** The JS find needs the CSS Custom Highlight API,
     Safari 17.2+ (§3).
   - **Find is JS**, with match highlighting via the CSS Custom Highlight API,
     not the engine's FindController. Behaviourally close; not identical.
   - **Bars and TOC are in-page overlays.** Different look, and they share the
     document's zoom/scroll context — the JS category DESIGN D12 sanctions as
     "shell viewport glue", but a real UX divergence.
   - **No e2e on macOS**, at least initially: the harness is Xvfb + xdotool +
     D-Bus. The macOS job builds and unit-tests only.
4. **The spike comes before any mac code.** The four assumptions in §1 are the
   contributor's to verify on real hardware; the owner cannot. The extraction
   (step 1) does not wait for the spike; the scaffold (step 3) should.

### What the issue gets right

Everything structural: the option triage, the rejection of unifying on wry
(DESIGN D2 and `03-rust-stack.md` already record the GTK3 reason), the
isolation strategy, and the choice of the controller extraction as the first
PR. The stays/changes table matches the code.

### What the issue underestimates

- **The size of "thin".** See §2. The extraction is the port's main cost, and
  it lands on the owner's side.
- **The chrome.** The issue lists bars/TOC as a mac-only concern. They are also
  a *controller* concern: the controller must talk to the bar and TOC through
  an interface, which is new code on Linux too.
- **The scheduler.** Not mentioned; needed (§2).
- **Editor pairing.** Listed under "single-instance / automation: dropped".
  For a reader whose README leads with SyncTeX-style pairing, dropping it is a
  headline gap, not a footnote.

## 5. Plan — the three items on the owner's side

Order: **CI → extraction → scaffold.** CI first because it protects the
extraction PR and gives the contributor's PRs checks; the scaffold last because
its trait implementations don't exist until the extraction defines them.

### 5.1 GitHub workflows

One workflow, `.github/workflows/ci.yml`, three Linux jobs plus a macOS job
that is added with the scaffold:

| Job | Runs | Notes |
|---|---|---|
| `check` | `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --lib --bins` | fast path; every push/PR |
| `e2e` | `cargo test --test e2e` | needs Xvfb + xdotool + dbus; serialized already; ~50 s on a fast box, expect 2–4 min on a 2-core runner |
| `bench` | `cargo bench --bench pipeline -- --noplot`, `scripts/bench-startup.sh -n 5` | report to the job summary + upload `target/criterion` as an artifact; **no hard gate** — shared runners are too noisy for a threshold, and a regression is a number a human reads on the PR |
| `macos` | `cargo build`, `cargo test --lib` on `macos-latest` | added by the scaffold PR; `continue-on-error` so it never blocks Linux |

**Environment for the Linux jobs — recommendation: an Arch container.**
`archlinux:base-devel` with `pacman -Syu rust gtk4 webkitgtk-6.0 mold
xorg-server-xvfb xdotool dbus` matches the dev machine (webkitgtk 2.52, gtk
4.22) and the AUR build exactly, so CI never argues with a version the owner
can't reproduce. Ubuntu 24.04 is the alternative (gtk 4.14, webkitgtk-6.0
2.44+; the crate minimums are 4.14 / 2.40, so it builds) but tests behaviour
against a two-year-older WebKit. One known knob for either: WebKit's bubblewrap
sandbox cannot create user namespaces inside a Docker container, so the e2e job
sets `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` (CI only). Cache with
`Swatinem/rust-cache`; the gtk4/webkit6 bindings dominate the cold build.

### 5.2 Extract the shared parts

Target layout — a third layer between core and the shells:

```
src/core/         pure, display-free, unit-tested (unchanged)
src/controller/   toolkit-agnostic imperative logic; depends on core only;
                  unit-tested against a fake viewport
  session.rs      the ex-`Shell` state + execute() + hint/jump/command flows
  viewport.rs     trait Viewport (ex-View pub surface), trait Chrome (bar + toc)
  scheduler.rs    trait Scheduler (defer-once, spawn-blocking, channel drain)
  scripts/        the five user scripts, byte-identical on both shells,
                  posting through window.__jmnj_post(name, msg)
src/shell/gtk/    View/Bar/TocView/Watch/Stdin/DBus implementing the traits
src/shell/mac/    (scaffold, §5.3)
```

Staged so each step is green on `cargo test` (unit + 50 e2e):

1. **Scripts.** Move the JS to `controller/scripts`, replace
   `window.webkit.messageHandlers.X.postMessage` with `window.__jmnj_post('X', …)`,
   define `__jmnj_post` in a GTK init script. Pure motion; e2e proves it.
2. **Traits.** Define `Viewport`, `Chrome`, `Scheduler`; implement them on the
   existing `View`, `Bar`+`TocView`, and a glib scheduler. No callers change.
3. **Session.** Move `Shell`'s state fields, `execute`, the key-dispatch body,
   hints, jumplist/quickmarks, completion, `state_json`, vault rescan and
   render orchestration into `controller::Session<V, C, S>`. `app.rs` becomes
   wiring: build widgets, adapt GTK events to `KeyPress`/wheel/motion, hand
   them to the session.
4. **Platform hooks.** External-URI launch, selection clipboard, data dir as
   trait methods or Linux-only modules.
5. **Docs.** DESIGN.md gains the three-layer boundary (a D2 amendment, not a
   new decision); CLAUDE.md's architecture section and TESTING.md follow.

Fast unit tests for the session land in step 3 and are the deliverable that
makes this worth doing even if macOS never ships.

Enforce the boundary the same way core's is enforced: no `gtk`, `glib`,
`webkit6` import anywhere under `src/controller/`.

### 5.3 The macOS shell (the contributor's, on `mac-support`)

Decided 2026-09-03: the owner does not pre-build a scaffold. Once the spike
is green the contributor opens the `mac-support` branch and builds the shell
against the landed contract (`src/controller/toolkit.rs`); the owner reviews
PRs and keeps CI green. What that shell consists of, compiled only on macOS:

- `Cargo.toml`: `tao` + `wry` under `[target.'cfg(target_os = "macos")'.dependencies]`;
  `gtk4` + `webkit6` (and `notify` stays shared) under the mirror cfg.
- `src/shell/mac/`: a tao window + wry WKWebView with a custom protocol
  serving the rendered document and document-relative images; an init script
  defining `__jmnj_post` over `window.ipc.postMessage`; the capture-phase key
  listener posting `KeyPress`-shaped messages; `Viewport`/`Chrome`/`Scheduler`
  impls that compile, with the parts the spike must confirm (zoom, find,
  overlays) as explicit `todo!()`s named after the spike items.
- `main.rs` cfg split; `tests/e2e.rs` gated to Linux.
- The `macos` CI job (§5.1) building the branch.
- Draft DESIGN.md amendments (dual-shell ADR, in-page chrome, JS find,
  automation story) and the README tier note, for the contributor to finish.

The owner cannot run any of it. The contract and the fake toolkit
(`src/controller/fake.rs`) are what make this tractable from the Linux side:
the traits the mac shell implements are the ones the Linux suite already
exercises, and the fake shows what each method is expected to do.

## 6. Open decisions

- **Trait + generic vs. per-platform type alias.** A `Session<V: Viewport>`
  costs nothing at runtime and buys a `FakeViewport` for unit tests; a cfg'd
  `type Viewport = gtk::View` is simpler but tests nothing. Recommendation:
  traits.
- **Where the mac shell lives.** `src/shell/mac/` under a `cfg(target_os)`
  (the issue's option) vs. an off-by-default cargo feature. `cfg(target_os)`
  keeps `cargo build` on a Mac just working with no flag; a feature would let
  Linux `cargo check --features mac` at least type-check the mac tree, but the
  wry/tao crates won't build their macOS backends on Linux anyway, so the
  feature buys nothing. Recommendation: `cfg(target_os)`.
- **CI base image.** Arch container (recommended) vs. Ubuntu 24.04 — see §5.1.
- **Editor pairing on macOS.** Out of scope for the scaffold; recorded as a
  gap. If the contributor wants it, the D-Bus interface is small enough
  (`GetState`, `ExecuteAction`, `GotoLine`) to mirror over a unix socket, and
  the Neovim plugin would need a second transport.
