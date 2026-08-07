# Development log

Newest entries first. Each entry: what happened, what was decided, what's next.

## 2026-08-07 — the restore gate stops conceding while the page is still growing

**Reported:** switching documents flashes the new page at scroll 0 before it
scrolls to the linked/stored position — on link follows *and* `Ctrl-o`/`Ctrl-i`.
Confirmed by the user against `board-reader/docs/index.md`.

**What it was not.** Two structural explanations were killed by tracing an
instrumented build (artifacts in `.flash-investigation/`):

- the position is *not* applied after first paint — `do_render_and_load` hands
  `at` to `View::load_document` before the HTML is built, and switches really do
  carry `open=Some("offset:…")`;
- there is *not* a second load per switch — the vault-rescan re-render fires only
  at startup, where the index goes from empty to populated (`equal=true` on every
  later switch, so `rescan_vault`'s guard short-circuits).

**Cause.** `scroll_restore_js`'s loop conceded one frame after
`document.readyState === 'complete'`. But `complete` is not a statement that
layout is final, and `apply` places the position with `scrollTo`, which *clamps
against the height it finds*. So for a deep offset in a document still growing,
the loop gave up, `reveal` ran, and the body became visible at a near-top
offset — then `settle_initial_position` corrected it after the load finished.
That correction is the visible jump. The comment on `settle_initial_position`
assumed the content "was already revealed near the right place", which is
exactly what fails for a deep offset: clamped means ~0, not "near".

Measured directly: forcing `apply` to fail briefly flips the restore from
`why=reached, reveal_y=24000` to `why=gaveup, reveal_y=0` — the reported shape.

**Fix.** Concede only once the document has stopped growing: track
`documentElement.scrollHeight` per tick and require `STABLE_FRAMES` (3)
consecutive unchanged heights *in addition to* `readyState === 'complete'`. A
document genuinely too short still settles in ~3 frames, and the unconditional
400ms failsafe still bounds the gate, so the never-blank-page guarantee
(CLAUDE.md) is untouched.

**Verification.** 300 unit + 48 e2e green on three consecutive full runs, and
**confirmed gone by the user on `board-reader/docs`** — the machine that
reliably showed it. Worth recording that the headless suite never reproduced the
bug (every fixture placed correctly, `why=reached`): the fixtures load too fast
to catch a document mid-growth, so the green suite was only ever evidence of no
regression. The reproduction gap is still open as a testing weakness.

**Next:** the startup double load — real but separate: at launch the vault index
goes empty → populated, so `rescan_vault` fires a second full `render_and_load`
after the first already finished.

## 2026-08-07 — incremental rebuilds: 5.3s → 1.0s

**Reported:** rebuilds feel slow.

**Measured.** A one-line edit to `src/main.rs` took **5.3s**. Almost none of that
was rustc — jumanji is a single crate linking GTK4 + WebKitGTK 6, so a trivial
edit recompiles one crate and then spends the bulk of its wall clock in the
linker. The repo had no `.cargo/config.toml`, so it was linking with GNU `ld`,
the slowest linker on the box, while `mold` and `lld` sat installed and unused.

**Two changes, both config, no code:**

- `.cargo/config.toml` sets `-C link-arg=-fuse-ld=mold`. gcc has supported
  `-fuse-ld=mold` since GCC 12 (16.1.1 here), so this needs no `clang` — the
  only new build requirement is `mold` itself, added to the AUR `makedepends`.
- `[profile.dev] debug = "line-tables-only"` in `Cargo.toml`. Panics and
  backtraces keep file:line (`.debug_line` verified present in the linked
  binary); what goes away is per-variable DWARF, i.e. inspecting locals in
  gdb/lldb. Raise it back to `true` for a debugging session if you need that.

**Result: 5.3s → 1.0s** (1.02 / 1.00 / 1.09 over three runs), a 5.2x cut.
Verified mold actually linked it (`.comment` records `mold 2.41.0`). Full clean
rebuild is 2m35s. 300 unit + 48 e2e tests pass, clippy clean.

Note the config is repo-wide, not dev-only — cargo has no per-profile
`rustflags` — so release and packaging builds link with mold too. The
`-ffat-lto-objects` workaround in the PKGBUILD is still required: mold no more
consumes GCC LTO bitcode from `onig_sys` than lld does.

**Next:** `target/` is 22G and still holds the superseded `ld`-built,
full-debuginfo artifacts. A `cargo clean` reclaims it at the cost of one 2m35s
rebuild — worth doing when convenient, not urgent.

## 2026-08-07 — zoom sticks across navigation

**Reported:** reading at 130% and clicking a wikilink drops you back to 100%.

**Cause.** `load_document` treated zoom as document state: on every switch it
restored the *target* file's saved zoom, and hard-reset both axes to 1.0 when the
target had never been opened. The reset was the common case — you follow links
into notes you have not read before.

**Decided: zoom is a session-level live view setting.** It carries unchanged
across every document switch; the per-file `zoom`/`text_zoom` in `history.toml`
is the *default on open*, read only at a window's cold start — the one moment
with no live session zoom to inherit. That is zathura's own split between
`adjust-open` and the live zoom (`docs/research/02-zathura.md`). Recorded in
DESIGN D5a.

**The change is a deletion.** `Shell.zoom` / `Shell.text_zoom` already *were* the
session value; the only thing making them look per-document was `load_document`
overwriting them. Removing that block leaves `build_ui` as the sole reader of the
saved zoom, so the "cold start vs. session" distinction needs no flag and no
state to keep honest — it is just which function runs. Nothing else moved:
history still records per-file, the format is unchanged, and reopening a note in
a fresh window still lands at the zoom you last read it at.

Two things the deletion depends on, both pre-existing and now load-bearing:
geometric zoom needs no re-apply because the native `zoom_level` is a WebView
property that survives a document load, and text zoom rides into the generated
HTML as the inline `--font-size` (D12) — read from `s.text_zoom` at render time,
which is now the inherited session value, so the new document's *first painted
frame* is already at the right size.

**Tests.** Two e2e cases: `zoom_carries_across_document_switches` (both axes
survive a hint follow into a never-opened file, `Ctrl-o`, `Ctrl-i` and `:open`,
and the target measures taller than the same filler at base size — proof the zoom
was in the HTML, not re-applied after paint) and
`history_restores_zoom_on_a_cold_start` (the other half of the split). 48 e2e +
300 unit tests pass.

**Next:** the flash on document switch is still open (STATUS item 1) and is being
diagnosed separately; this change does not touch the load/paint path.

## 2026-08-07 — the unscrolled page stops flashing; `background = true`

**Reported:** walking the jumplist between documents flashes the top of the
page before it lands where you left off.

**Root cause, and it was never really about the jumplist.** Every reading
position was applied from Rust inside the `LoadEvent::Finished` handler. By the
time that fires, WebKit has parsed, laid out and composited the document at
scroll 0, and the correction takes another UI→web IPC hop — so the unscrolled
top is on screen for that whole window. The jumplist just shows it worst,
because a cross-document hop reloads and the recorded offset is usually deep in
the file. Five other paths had the same bug and nobody had named it: startup
into a file with a saved position, **every live reload** (i.e. every save of the
file you are reading), a landed vault rescan, `r`/`:frontmatter`/`:set`, the
theme watcher, and — up to *three* positions in one load — a link fragment or
`--forward` line, which were applied after the history offset that had already
been applied after the top. A stdin stream re-rendered this way at up to 8 Hz
for the life of the pipe.

**The position is now part of the load.** `InitialPosition` (`Top | Offset |
Anchor | SourceLine`) is armed by whoever starts the load — so the precedence
between the three ways to ask for a position is decided once, in one `match`,
instead of by the order of two statements in the finished handler — and rides
into the HTML as `data-jmnj-open` on `<html>`, through the same one-shot rewrite
that already pre-applied `class="dark"`. Text zoom joins it there: the inline
`--font-size` was also being re-applied after paint, so every reload at a
non-100% text zoom reflowed from base size up. A permanent document-start
user-script applies the attribute on `requestAnimationFrame` — callbacks run
before the frame they belong to is painted — and `html.jmnj-restoring body {
visibility: hidden }` covers the rest, with an unconditional timer-backed
reveal, because a permanently blank page would be the worse bug.

Inert markup rather than an inline `<script>` is not a stylistic call: the page
CSP is `default-src 'none'` with no `script-src`, so a script would be blocked,
and D3 wants HTML that can later feed an export path. Recorded as DESIGN D12.

**On proving it.** The first frame-capture harness passed the *buggy* build —
it marked the top of the document with a `file:` PNG, and an image has no
intrinsic size until it loads, so during the exact window under test the marker
had not painted and the detector saw white. Second version marks the top with
CSS from a user theme (inlined into the page, nothing async) and adds
subresources below the fold so `Finished` lags first paint the way it does in a
real note. That version discriminates: pre-fix, frames 145-146 of the capture
are a full screen of magenta; post-fix, zero magenta frames and two frames of
the page's own white `--bg` instead. Worth remembering — with an all-inline
test document the compositor may never produce a top-of-document frame at all,
and a clean capture then measures nothing.

The e2e observable is `first_frame_scroll_y`: the offset the first painted frame
was placed at, recorded in-page. The old code always *ended* in the right place,
so any assertion on the final position passed throughout — which is precisely
why one existed and the bug shipped anyway.

Two things fixed in passing: `'x` quickmark no longer issues a native zoom
change alongside an async scroll when the mark was set at the current zoom (a
frame at the new zoom and the old offset), and `open_file` now queries the
departure offset live instead of reading the cached `last_scroll`, which the
scroll listener only refreshes when the *rounded percent* changes — so a small
wheel scroll then a link click used to record a stale departure and land
`Ctrl-o` slightly off.

**Also: `background = true`.** Running a GUI reader from a terminal meant typing
a trailing `&` every time. New `[options]` key (off by default — blocking is
what a command is expected to do), with `--background`/`--foreground` overriding
it. It detaches by re-executing the binary rather than forking: this process is
about to bring up GTK, WebKit and D-Bus, and `fork` in a soon-to-be-threaded
program is a footgun that a fresh process simply does not have. The child gets
argv plus a trailing `--foreground` (last flag wins, so it cannot re-detach), its
own process group, and null stdio. It detaches last, after every startup
diagnostic has had its chance at the terminal, and never for a stdin source —
we are the consumer of a pipe the shell is still writing into.

**Next:** the two frames of background during a restore are the hide gate's
cost; if it ever becomes noticeable on a slow document, the reveal could be
gated on layout stability rather than a fixed failsafe.

## 2026-08-06 — the statusbar says how you got here; completion pages

Two chrome complaints, both about the bottom bar not telling the whole truth.

**The left field is a breadcrumb now**, not a filename: `index.md > topic.md >
note.md`. The data was already there — the jumplist spans documents — so this is
`Jumplist::trail`, which walks the entries *behind* the cursor, appends the live
document, and collapses consecutive jumps within one file (a breadcrumb answers
"which files", not "how many jumps"). Following a link extends it; `Ctrl-o`
shortens it and `Ctrl-i` re-extends it, which falls out of the existing `pos`
without any new state. The `Shell::filename` field went away in the process: it
was the last segment of something derivable, and now it is derived.

Overflow is cut on the **left**, as asked: `core::jumplist::breadcrumb` drops
whole segments oldest-first behind a leading `…` until the line fits, and never
drops the last one, so the current filename is always visible. Fitting needs a
width, which is the one genuinely shell-side part: `Bar::status_columns`
measures a run of monospace glyphs against the label's allocated width, and
`refresh_status` re-fits (idempotent, string-compare) so a resize reflows on the
next interaction. `EllipsizeMode::Start` on the label covers the gap in between
— and stops a long trail from forcing a window minimum width.

That measurement is also why `status_left` is now `halign: Fill` rather than
`Start`. A `Start`-aligned label is allocated only its natural width, so
`width()` reported *what it already showed*, not the space available — a budget
that shrank on every re-fit until the breadcrumb collapsed to `… > note.md` in a
1000 px window. Caught in the Xvfb screenshot pass, not by the tests: the e2e
suite asserts the untruncated trail (new `GetState.trail`), which was correct
the whole time. Worth remembering that a headless assertion on the *model* says
nothing about the *layout*.

**Tab completion paginates** instead of showing the first eight candidates and
silently dropping the rest. `core::command::completion_line` packs candidates
into pages that fit the bar and renders the page holding the selection with `▸`
on it; repeated `Tab` walks the pages in order, so all 37 command names are
reachable rather than the same five being echoed forever. Pages are packed from
the start of the list (stable boundaries, so tabbing back and forth doesn't
reshuffle), every candidate reserves the marker column (so the boundaries don't
shift with the selection), and the `(page/pages)` counter is omitted when there
is only one page.

Next: nothing outstanding on the chrome. If the completion echo turns out to be
cramped on narrow windows, the zathura-idiomatic answer is a multi-row list
above the inputbar — deliberately not built here, because pagination was the ask
and a popup list is a widget, not a formatting function.

## 2026-08-06 — the vault index gets small, and gets off the main thread

Follow-up to the entry below, which shipped marker-based rooting and then
admitted in the same breath that the `.git/` fallback walked 21 k files in
~150 ms per load, with the fix deferred to "measurement first — a narrower cap,
or moving the walk off-thread". This is that measurement, and all three fixes.

The measurement first, because it moved the target. On this repo the old path
walked 22 966 files in ~23 ms and then spent **~126 ms building the lookup
tables**. The ~150 ms was mostly the build, not the walk — a shrunken index was
always going to matter more than a faster traversal.

**`.gitignore` is obeyed.** Via the `ignore` crate (ripgrep's): `.gitignore`,
`.ignore`, `.git/info/exclude`, the global gitignore, and the same files in
parent directories. `require_git` is off, so an ignore file counts in a
marker-less or `.obsidian/`-rooted vault too — the file is the user saying "this
is not my content", and whether git is watching is beside the point. Writing a
gitignore matcher by hand was never on the table; negation, `**`, and precedence
are exactly the kind of thing that looks done and isn't.

**Only Obsidian's accepted formats are indexed** — notes, images, audio/video,
PDF, `.canvas`, `.base` (research §2). Nothing else can be named by a `[[…]]`,
so indexing it bought nothing and cost an alias-read each.
`AssetKind::classify` now returns `Option<AssetKind>`, which makes that list the
single place the formats are written down and `is_indexable` one line over it.
The earlier revision's rejection of a `target/`/`node_modules/` blocklist still
stands and is the reason this shape is right: a blocklist is a guess about which
directories don't count, whereas an ignore file is already in the tree and the
user wrote it.

Together: 22 966 files / ~149 ms → **16 files / ~1.4 ms** on this repo.

**And the walk moved off the main loop** anyway, because the remaining cost is
set by the filesystem, not by us — a vault on a slow mount would still stall
every `:open`. `scan` + `VaultIndex::build` run on `gio::spawn_blocking` and the
result is swapped in when it lands. Landing is deliberately quiet: the new index
is compared against the one in hand and an identical one — nearly always —
re-renders nothing, and a changed one re-renders only if the document contains a
`[[` at all. Launch starts with an empty index and the scan overlaps window
creation and WebKit startup, so it lands long before the first load finishes.
`Vault::rooted` is the only blocking constructor left and is now `#[cfg(test)]`,
so "the reader never blocks on a walk" is a compile error rather than a habit.

The e2e suite caught a real bug in this: a rescan landing *mid-load* re-queried
the live scroll position (still 0.0, the restore not yet applied) and clobbered
the pending history restore. It now keeps a queued restore instead of re-reading.
`GetState` gained `vault_files`, which is both how the tests wait for a scan to
land without sleeping and the first thing to check when a `[[…]]` won't
resolve — it tells you whether the vault jumanji found is the one you meant.

Next: nothing outstanding here. If a vault ever does hit the 50 000-file cap the
answer is still measurement, not a cache.

## 2026-08-06 — vault root by marker, a frontmatter toggle, mouse side buttons

Three user-reported gaps after living with D11 for a day.

**The vault root is the document's tree again, not the CWD.** Yesterday's entry
argued the root should be the process working directory, on the grounds that
depending on `.obsidian/` meant depending on a competitor's private directory.
Using it proved the trade backwards. A marker directory sitting in your own
notes tree is a *fact about how those notes are organised*; jumanji reading it
requires Obsidian to exist about as much as reading `.git/` requires GitHub. The
CWD, meanwhile, is state that is not in the tree at all and is invisible in the
launcher, desktop entry, or file manager that most opens actually come through —
so the same note rendered differently depending on which directory a shell
happened to be in. The original objection that *did* hold up was two resolution
modes, and there is still only one.

`vault::root_for(doc)` now walks up from the document's directory: nearest
ancestor with `.obsidian/`, else nearest with `.git/`, else the document's own
directory. Each marker is searched over the whole chain before the next is
tried, so a repo checked out inside a vault reads as part of that vault rather
than rooting at itself. `.git/` is what covers the marker-less notes tree, which
was the case that prompted this.

The root is now **pinned**: resolved once from the launch document and held for
the process, where before every load recomputed it. Without CWD as the anchor
that recomputation would silently narrow the vault as you followed links into
subfolders. Only the index is still rebuilt per load, which is what keeps it
fresh. `demo/vault/.obsidian/` came back — deleted yesterday as unused, it is
now what makes the fixture a vault instead of part of the jumanji repo.

Measured, since `.git/` can root wide: this repo is ~21 000 non-hidden files and
walks in ~150 ms per load. Left alone. A build-output blocklist (`target/`,
`node_modules/`) is the obvious next move and is exactly the wrong one — it is a
heuristic guess about which directories "don't count", which is the invisible
state this change exists to remove. The 32-deep / 50 000-file caps still bound
the tail.

**Frontmatter got a toggle, and a rendering.** Hidden stays the default — a note
should open as prose. `:frontmatter` (or `toggle frontmatter`) flips it live,
`show-frontmatter` sets the startup state. Showing it was nearly free: comrak
already parses frontmatter into a node its formatter skips, so the pass swaps
that node's value for raw HTML the way the fence passes do, keeping its source
position and so its D7 reverse-click target.

What it renders is a `<dl>` properties table, not the YAML source. The reason to
ask for frontmatter is to read the values, and a monospace dump is a worse way to
do that. New `core::frontmatter` parses it with the same deliberately shallow
rules as `parse_aliases` — one level of `key: value`, both list spellings,
verbatim text for anything structured it does not model, and the whole block
verbatim when nothing parses. No YAML crate: a dependency to render metadata
nobody asked to see is a bad trade, and the fallback is honest rather than
lossy.

**Mouse buttons 8/9 walk the jumplist.** They dispatch `JumpBackward` /
`JumpForward` rather than owning a history — after following a wikilink the
jumplist *is* "back through what I was reading", and a thumb click should not be
able to disagree with `Ctrl-o`. Capture-phase `GestureClick` with
`set_button(0)` on the toplevel: a controller on the WebView never sees these
(D5a), and WebKit would otherwise walk its own session history, which is not the
history the reader navigates.

Also fixed a pre-existing `collapsible_if` in `tests/e2e.rs` that clippy 1.95
started flagging.

Tests: 281 core unit tests, 42 e2e (three new: marker rooting from an unrelated
CWD, the frontmatter round trip, and the side buttons under XTEST), all green.

## 2026-08-05 — D11 follow-up: the vault root is the working directory

Reversed the vault-discovery half of D11, hours after landing it. The dialect
support was built around walking up from the document for an `.obsidian/`
marker, with a second `Loose` resolution mode for everything else. That was
wrong in a way worth naming: it made jumanji's rendering depend on a
*competitor's private directory*. A folder of notes got one behaviour with
Obsidian installed and a different one without; the same document meant
different things depending on invisible state; and the majority case — a plain
directory of markdown — was relegated to the fallback path.

**The vault root is now simply the process CWD, always.** We use Obsidian's
*syntax* and none of its machinery. A directory of notes is a vault because you
opened jumanji in it. That is the same agnosticism the "always on, zero config"
decision already argued for, followed to its conclusion.

Consequences, all accepted and recorded in D11: the two-state `Vault` collapsed
to one (`{ source, index }` — a variant nothing constructs is a Tar Pit
invitation), and with it went `resolve_loose`, its case-insensitive `read_dir`
retry, the `..`/absolute-path guard, and the `.md`-variant join. None are
missed: the index is case-folded already, and because resolution is a table
lookup over scanned entries and never a path join, `[[../secrets]]` and
`[[/etc/passwd]]` are simply not keys — the guard was only ever needed by the
path-joining branch. `Vault::discover(source)` became
`Vault::rooted(root, source)`, taking the root as an argument so the core holds
no ambient state; the shell's `vault_for` supplies `current_dir()`.

A document opened from *outside* the CWD tree now resolves only against the CWD
index, so a bare `[[x]]` in it may come out unresolved. That is correct rather
than a defect — the reader was not launched in that note's collection — and the
document still renders, with its same-file `[[#Heading]]` / `[[#^id]]`
references intact, since those never consult the index. stdin needed no change:
D9 already hands it a `<cwd>/stdin.md` sentinel.

**Measured, because the caps now carry real weight** — the root is whatever
directory the user launched from, not one they deliberately marked. A repo root
of 18 662 files walks in ~145 ms. A `$HOME` big enough to hit the 50 000-file
cap takes ~1.0 s, which is a visible stall on the UI thread for anyone who runs
`jumanji ~/notes/x.md` from their home directory. Left as-is per D11's
"measurement, not a speculative cache" rule, but it is now a *known* number
rather than an assumption; a narrower cap or an off-thread walk is the fix if it
becomes a complaint.

Fallout: `demo/vault/.obsidian/.gitkeep` deleted (nothing looks for it), and the
e2e harness gained a working-directory knob — `Harness::launch_file_in_dir`
setting `Command::current_dir` — because a wikilink test that launches from the
repo root now indexes the repo instead of the fixture. All three D11 e2e cases
launch inside their vault.

Tests: 257 core unit tests, 39 e2e, all green.

## 2026-08-05 — Obsidian dialect & vault resolution (D11)

Obsidian is the largest population of markdown jumanji rendered badly:
frontmatter showed as a setext-heading mess, `[[wikilinks]]` as literal text,
22 of the 27 callout spellings as visible `[!question]` garbage, and
`%%comments%%` / `^block-ids` leaked into the prose. DESIGN D11 is now
implemented, always on and with no new config option.

**Four constructs are comrak flags, not code.** `extension.highlight`
(`==x==` → `<mark>`), `extension.inline_footnotes` (`^[…]`, folded into the
existing footnote numbering), `extension.wikilinks_title_after_pipe` (Obsidian
is url-first) and `extension.front_matter_delimiter = Some("---")`.
`parse.relaxed_tasklist_matching` joins them. `extension.alerts` is turned
**off** — `core::callout` subsumes it, so the two `markdown-alert` tests were
deleted and their coverage reinstated as callout tests (with `important` now an
alias of `tip`, a deliberate Obsidian-over-GitHub divergence).

**The vault model.** `core::vault` walks up from the document for `.obsidian/`;
found ⇒ an indexed vault (case-folded maps of full relative path, bare filename
and frontmatter `aliases`, with vault-root-beats-sibling ranking), otherwise a
`Loose` vault resolving under the document's own directory with `.md` optional,
one case-insensitive `read_dir` retry, and `..`/absolute targets refused. It is
a table lookup, never a path join, so a wikilink cannot address anything
outside the vault. `aliases` is read by a ~40-line targeted parser over the
first 4 KiB of each note, not a YAML crate. **Freshness:** rebuilt in
`load_document` and by `r`, deliberately *not* by the watch-driven live reload
— editing a note cannot rename another one.

**Wikilinks emit three nodes**, not one: `HtmlInline("<a …>")`, a real `Text`
node holding the label, `HtmlInline("</a>")`. Folding the label into the raw
HTML would hide it from `comrak::html::collect_text`, which both `core::toc`
and comrak's own heading-id renderer use — a heading containing a wikilink
would silently get a truncated anchor. Unresolved links carry **no `href`**, so
they are non-clickable, non-focusable and invisible to the `f` overlay's
`a[href]` selector: dead by construction rather than by a guard in the router.
One thing measurement corrected: comrak consumes the pipe itself, so an alias
arrives as the node's *children*, not in `NodeWikiLink.url` — the label is
therefore "what the children say, when that differs from the target".

**The text-run scanner** (`core::textscan`) carries the three constructs comrak
never sees as nodes: `![[embeds]]` (the `!` consumes the `[`, so the wikilink
branch never fires), `%%comments%%` and trailing `^block-ids`. It joins each
block's consecutive `Text`/`SoftBreak` siblings, scans that string with a pure
`spans()` function, and splices only when something matched — a run with no
construct is left untouched, so no paragraph's inline `data-sourcepos` churns
for nothing. Fence and inline-code safety is structural: those are `CodeBlock` /
`Code` nodes, never `Text`. A trailing `^block-id` is anchored to the end of
the **block**, not the end of the run: a run ends wherever the next sibling is
an inline node, so `Note the marker ^abc *see below* end.` would otherwise emit
a bogus anchor and eat the whitespace around the pseudo-id. Three honest
limits, all recorded in the module docs: a `%%` region straddling a list
boundary is not handled; a block id inside a table cell is ignored (neither
does Obsidian); and callout titles are plain text — emphasis loses its markers
but keeps its text, while raw inline HTML or a wikilink in a title is dropped
entirely, text and all.
Image embeds become `<img class="internal-embed">` with the `|W`/`|WxH`
dimensions; note/PDF/media/canvas and unresolved targets degrade to an honest
link-card, since transclusion needs recursive rendering with cycle detection.

**`pending_anchor`** closes a gap D11 supersedes: `open_uri` used to drop the
fragment of a *cross-document* link, so `other.md#section` opened the file and
ignored the anchor. It is now carried across the load and applied in the
load-finished handler exactly as `pending_forward` is (which still wins — an
explicit editor request outranks a link fragment). This fixes plain markdown
fragment links too, not just wikilinks. The fragment is percent-decoded on the
way in, because WebKit hands back `%5E` for a block id and `getElementById`
needs the literal `^`. The anchor is armed inside `open_file`, *after* its
existence check, so a dead link cannot leave one armed to hijack the next
document's restored scroll. The same-document short-circuit next to it now
compares **paths**, not URI strings: gio and our own percent-encoder disagree
on `'`, `(`, `)` and `+`, so a byte comparison missed a same-document
`[[#Heading]]` in any file whose path contained one — reloading the document
instead of scrolling within it.

**Three decisions settled during implementation** and recorded as a DESIGN D11
amendment: (1) the `Vault` binds the document path, so `render(md, opts, vault)`
stays three-argument and `Loose` derives its directory from `source.parent()`
rather than storing one; (2) callout titles are plain text; (3) a non-`x` task
marker clears comrak's symbol (which would otherwise render as *checked*) and
prepends a `<span class="task-marker">` badge.

Pass order, extending D3/D6.2: strip comments → callouts → fences → mermaid →
highlight → math → embeds → wikilinks → block ids → task markers → wrap tables
→ annotate html-block lines → TOC → format. Comments go first so a
commented-out fence never runs a renderer; embeds before wikilinks so the two
never race on the same text; block ids after wikilinks so an `^id` inside a
link label is not mistaken for a trailing id.

Tests: 256 core unit tests (up from 201) and 39 e2e (up from 36). The new e2e
cases drive the checked-in `demo/vault/` fixture and two throwaway temp vaults.
`.markdown-alert*` CSS was replaced by `.callout*` (the `--alert-*` accents are
kept and reused across the 13 canonical types), plus rules for
`is-unresolved`, `img.internal-embed`, `.embed-card`, `.block-anchor`, `mark`
and `.task-marker`.

Next: none of this is transclusion. If real note embedding is ever wanted it
needs recursive rendering with cycle detection, which is why the link-card
exists. `#tag` pills, a rendered properties table, Bases and Canvas stay out of
scope.

## 2026-07-07 — Fix: independent processes per file (GApplication NON_UNIQUE)

Opening a second file while one was already open failed:
`failed to export D-Bus object: An object is already exported … at
/org/membranepotential/jumanji` and `could not own D-Bus name …PID-<pid>;
automation disabled` — and the "second" window showed the *first* file.

Root cause: the GTK `Application` ran in the default single-instance mode. A
second `jumanji <file2>` saw the primary already owned `…jumanji`, so
GApplication forwarded *activation* to the first process instead of running the
second process's own `activate`. The primary then re-ran `build_ui` for its
original file, re-exporting the D-Bus object at the same path (error 1) and
re-owning its own PID name (error 2), and popping a duplicate window on the
wrong document.

Fix: build the `Application` with `ApplicationFlags::NON_UNIQUE`, so each
`jumanji <file>` is a fully independent process (zathura semantics) — which is
what the per-PID D-Bus surface (`…jumanji.PID-<pid>`) always assumed.
Verified under Xvfb + a private session bus: two instances now own distinct PID
names and each reports its own file over `GetState`; the neovim `--forward`
routing still finds the running instance and exits without a second window.
DESIGN D7 updated to record the invariant. Next: nothing outstanding.

## 2026-07-07 — Release v1.2.0

Minor bump collecting the two features since v1.1.1: the in-repo Neovim plugin
for two-way editor sync (`lua/jumanji/`) and the wider 960 px default content
column. No behavior change beyond those; released as tag + GitHub release, AUR
PKGBUILD repointed.

## 2026-07-07 — Wider default content column (720 → 960 px)

The default `page-width` was 720 px, which on a 1920-logical-px display (a 4K
panel at 2× scale) reads as roughly a third of the screen. Bumped the default
to 960 px so it lands at ~half the width out of the box. Purely a default
change — the config knob and the intrinsic-width diagram model are untouched.
Updated everywhere the default lives: `Options`/`Defaults` in `pipeline.rs` and
`config.rs`, the `--content-width` CSS fallback, the DESIGN option table, and
the README/demo sample configs.

## 2026-07-07 — Neovim plugin: two-way sync without `--listen` (D7 editor half)

The README's copy-paste Neovim snippets grew into a real plugin, shipped
in-repo at `lua/jumanji/` (fzf-style repo-as-plugin; DESIGN D7 updated). Both
directions reuse the existing D7 surfaces — nothing changed on the Rust side.

- **Forward.** `require("jumanji").open()` (suggested `<leader>cj`,
  ft=markdown) pairs the buffer with a reader: discovers a running instance
  with the file open via session-bus scan + `GetState` (pid from the bus name
  is the liveness handle) or spawns `jumanji --forward <line> <file>` detached.
  `CursorHold`/`BufWritePost` autocmds push the cursor line while that pid is
  alive; a dead pid disarms quietly, so a closed reader is never resurrected
  by a stray autocmd.
- **Reverse.** `editor-command = "nvim -l …/lua/jumanji/reverse.lua %l %f"` —
  vimtex's inverse-search pattern instead of the README's old `nvr` +
  `--listen` recipe: the scripting-mode nvim globs default server sockets
  (`stdpath("run")/nvim.*`, socket-type filtered, own pid skipped — self-RPC
  deadlocks), and the instance with the file loaded claims the jump + raises
  its terminal (`$WINDOWID`/xdotool); otherwise the first reachable instance
  opens the file. Two bugs found by headless tests: `ok and res or nil`
  swallowed the meaningful `false` ("reachable, didn't claim"), and socket
  paths >107 chars get silently truncated by `sun_path` (only bit the test
  harness — real sockets live in `$XDG_RUNTIME_DIR`).
- **Verified headless** (Xvfb + real instance for forward; `--listen`
  listeners for reverse): claim, fallback, no-instance, route-to-existing
  (no second window), spawn-at-line, live push, detach survival.

Next: nothing planned for the plugin; candidates are a `%c` column placeholder
and TOC-aware reverse jumps.

## 2026-07-07 — Statusbar/search UX polish (1.1.1)

Three zathura-fidelity fixes, all in the shell:

- **Live scroll percent.** Wheel / touchpad / scrollbar scrolls are handled by
  WebKit itself, so Rust never learned about them and the statusbar percent went
  stale until the next keypress. Added an injected `scroll` listener (rAF-
  coalesced, fires only when the rounded percent changes) that pings a new
  `scroll` script-message handler → `refresh_status`. Keyboard scrolls already
  refreshed directly, so they're untouched.
- **`Esc` clears more.** `Action::Abort` now unconditionally clears the active
  search (highlight + `n`/`N` state via `find_clear`) and restores the statusbar
  left to the filename, wiping any transient notice — the universal-reset
  semantics zathura's `abort` implies. Previously it only cleared search when the
  input bar happened to be open.
- **Search no longer clobbers the clipboard.** WebKitGTK mirrors the DOM
  selection into X11 PRIMARY, and `FindController` selects each match — so every
  `/`, `n`, `N` was silently overwriting the clipboard with the hit. Two moves:
  copy-on-select now triggers on `mouseup` (a real pointer gesture) instead of
  `selectionchange` (which also fires for the programmatic find selection); and a
  `FindController::found-text` handler restores PRIMARY to the user's last real
  selection right after WebKit writes the match. `found-text` reliably fires
  *after* WebKit's PRIMARY write (verified under Xvfb), so it wins the race —
  clearing the DOM selection does not, since the write is already done. The match
  highlight stays; the clipboard is left alone.

Verified end-to-end under Xvfb (search doesn't copy, a genuine selection survives
a search, `Esc` clears highlight+notice, wheel moves the percent). All 36 tests
green.

**Process note:** every tagged release must also get a GitHub release with notes
(`gh release create`) — see CLAUDE.md. v1.0.0's was created; v1.1.0's was missed
and is being backfilled.

## 2026-07-06 — Cross-document jumplist: go back after following a link (D10)

Following a `.md` link (or `:open`) used to reset the jumplist, so there was no
way back to the previous file. Now the jumplist spans documents:

- `core::jumplist` entries are a `Location { doc: Option<PathBuf>, scroll_y }`
  instead of a bare scroll offset — which document, and where in it (`None` =
  the non-returnable stdin stream). The push/back/forward algorithm is
  unchanged; only the payload widened. Core stays pure (`PathBuf` is std).
- `open_file` now records the departure `Location` on the jumplist before
  loading the new file; the load mechanics split into `load_document`, which
  jump-navigation reuses (no jumplist bookkeeping of its own) and which no
  longer resets the jumplist. `Ctrl-o` scrolls in place for a same-document
  target, else reopens the file at the recorded offset. Marks stay
  per-document.
- `Backspace` is a second default binding for `jump backward` (remappable) — the
  natural "go back" key. Rejected a separate document back-stack: one
  document-aware jumplist serves both keys (vim's jumplist already carries a
  buffer per entry).

Verification: new jumplist unit test (`crosses_documents_carrying_the_source_file`),
keymap default asserts Backspace → JumpBackward, and a headless e2e
(`back_and_forward_cross_documents_after_following_a_link`) drives follow-link →
`Backspace` → `Ctrl-i` across two files (run 8× to confirm it isn't flaky — the
e2e asserts *which document* each step lands on; a location's scroll offset is
covered by the unit test, since the live back-capture is inherently async).
Full suite + clippy `-D warnings` + fmt green. **Next:** nothing pending.

## 2026-07-06 — v1.0.0: Milestone 3 complete

All M3 items are built and the backlog of user-reported issues is fixed, so
this is 1.0:

- **LaTeX math** (D8): comrak `$`/`$$` → pulldown-latex → native MathML Core,
  no JS; fonts embedded as `data:` URIs under unshadowable family names after
  the mathjax2 system-font collision broke layout constants (see the
  font-shadowing entry below).
- **External fence renderers** (D6.2): `[renderers]` maps a fence language to
  a command (`dot = "dot -Tsvg"`), body on stdin, stdout replaces the fence;
  5 s timeout, 4 MiB cap, failures degrade to highlighted code + note.
- **Editor sync** (D7): `--forward <line>` + D-Bus `GotoLine` (running-instance
  handoff included) forward; Ctrl+click → `editor-command` (`$EDITOR +%l %f`)
  reverse, via comrak `data-sourcepos`.
- **stdin streaming** (D9): `tool | jumanji` / `jumanji -`, progressive
  re-render through the live-reload path, scroll preserved.
- **Fixes:** dark-mode syntax CSS scoping (`html:not(.dark)`), WebKit DMABUF
  renderer off by default (scroll layer dropouts; override by presetting the
  env var), press-inside-selection starts a new selection (no text drag), TOC
  selection single-sourced (mouse click + Enter jumps to the clicked row).

Verification: 166 unit + 35 e2e (headless Xvfb), clippy `-D warnings`, fmt —
all green. Release ritual: bump, tag `v1.0.0`, push, point PKGBUILD at the
tag tarball.

Awaiting user feel-test on the live GPU (headless can't show them): scroll
layer dropouts actually gone with the DMABUF default, drag-select feel.

Next: user feedback on 1.0; AUR publishing remains explicitly deferred.

## 2026-07-06 — stdin streaming (M3, DESIGN D9 — the last M3 item)

**What:** `some-tool | jumanji` (and `jumanji -`) reads markdown from stdin and
progressively re-renders as more arrives, until EOF.

- **CLI (`main.rs`).** `file` is now `Option<PathBuf>`. New pure core type
  `core::source::Source::resolve(file, stdin_is_terminal)`: `-` → stdin, a path →
  file, no arg + piped stdin → stdin, no arg + a terminal → a clap usage error
  (nothing to read). `--forward` + stdin is rejected up front with a clap
  `ArgumentConflict` (it targets a saved source line / running-instance handoff —
  meaningless for a pipe). File sources keep the absolute-path + existence check +
  D7 forward-to-running shortcut; stdin skips all of it.
- **Reader (`shell::stdin::StdinReader`).** A background thread reads stdin into a
  growing `Vec<u8>` and posts ticks over an `mpsc` channel; a `glib` 120 ms poll
  (matching `watch.rs`'s cadence) drains a burst into **one** `render_and_load(
  preserve_scroll = true)` — the exact live-reload path, so scroll position is
  preserved across streaming re-renders for free. EOF sends a final tick and the
  thread exits (no error state); `echo x | jumanji -` renders once and settles.
  Content is `from_utf8_lossy`-decoded per render, so a chunk boundary splitting a
  multibyte char self-corrects on the next chunk. Thread/IO is shell-only; core
  stays pure.
- **Stream degradations.** Watcher skipped (nothing to watch); per-file history
  skipped (no identity, zathura parity); statusbar + `GetState.file` report
  `stdin` (the latter keeps the D7 forward-search from mistaking a stream for a
  file); reverse editor sync (`%f`) suppressed with a statusbar notice;
  relative links/images resolve against the CWD via a never-touched
  `<cwd>/stdin.md` base sentinel. `:open`/a link click ends the stream and
  switches to a normal file document (watcher/history/editor sync resume). TOC,
  math, mermaid, external fence renderers, search, marks work unchanged (they run
  on pipeline output).

**Tests:** **160 → 166 unit** (`core::source`: 6 — the resolve matrix,
`is_stdin`, `display_name`). **32 → 35 e2e**: a new `Harness::launch_stdin()`
spawns `jumanji -` with `Stdio::piped()` and returns the child stdin;
`stdin_dash_renders_after_content_then_close` (write + close → TOC fills),
`stdin_streaming_grows_toc_and_preserves_scroll` (write half → render + scroll,
write rest → TOC grows and reading position restored — mirrors the live-reload
test), and `stdin_instant_eof_renders_fine` (`echo |`-style empty EOF still
loads and drives). `cargo test && clippy -D warnings && fmt` all clean. No new
dependencies.

**Note:** the streaming scroll assertion waits for the position to *settle* after
the TOC grows — `toc_len` updates synchronously as the new doc is built, but the
scroll restore lands later in the load-finished handler, so reading `scroll_y`
the instant the TOC grows catches a mid-reload top (flaked under load until the
settle-wait was added).

Next: AUR package is the only remaining M3 item.

## 2026-07-06 — fix: TOC click+Enter jumped to the wrong (stale) entry

**What:** in TOC/index mode, clicking a heading row and pressing `Enter` jumped
to the *first* heading instead of the clicked one. Reproduced via an e2e mouse
probe; present on HEAD and v0.2.2.

**Root cause (two selection states):** `src/shell/toc.rs` kept a shell-internal
`Model::selected` node index that `j`/`k`/`h`/`l` mutated and `Enter`
(`TocSelect`) read — but a mouse click only moved the `ListBox`'s own visual
selection, never `Model::selected`. So click + `Enter` used the stale internal
index (initially the top entry, scroll ≈ 56px).

**Fix (Out-of-the-Tar-Pit — one source of truth):** deleted `Model::selected`;
the `ListBox`'s selected row is now the single selection state. A visible-row
index maps to a tree node via `Model::visible()`, so `selected_node()` reads the
current selection back on demand — no sync glue between two states.
- `move_selection` clamps/moves the ListBox row directly; expand/collapse read
  the selected node, mutate the tree, and `refresh(select_node)` repaints and
  reselects the same node by its new visible position; `selected()` (used by the
  jump) reads whatever row is selected — mouse or keyboard. Counts still work
  (generic in the dispatcher, unchanged).
- Also connected the `ListBox` `row-activated` signal (new
  `TocView::set_activate_handler`, wired in `app.rs::connect_toc_activate`) to
  the same `toc_select` jump path, so **double-clicking** a row jumps directly —
  GTK convention and zathura's index spirit (single click = select only).

**Tests:** 160 unit unchanged; **30 → 32 e2e**: `toc_click_then_return_jumps_to_
clicked_row` (clicks a row below the fold, asserts the jump lands well past the
first-heading offset — fails on the buggy code where click is ignored) and
`toc_double_click_activates_and_jumps`. Both deterministic across repeated runs
(plain `click`/`double_click` harness helpers deliver the button at the pointer,
the proven XTEST pattern). `cargo test && clippy -D warnings && fmt` all clean.

## 2026-07-06 — editor pairing / sync (M3, DESIGN D7 — the SyncTeX analogue)

**What:** forward and reverse editor sync, zathura's `--synctex-forward` /
`synctex-editor-command` mapped onto markdown.

- **Forward (editor → reader).** New `--forward <LINE>` CLI flag + `GotoLine(line:
  u32)` D-Bus method on the per-instance interface. Scrolls to the element whose
  source line is the greatest at-or-before `LINE` (jumplist-pushed, like any
  jump). `jumanji --forward N file.md` first tries to forward to an instance that
  already has `file.md` open — enumerate `…jumanji.PID-` names, read each's
  `GetState` `file` (reused, no bespoke `GetFile`), and on a canonical-path match
  call `GotoLine(N)` and **exit 0 without a window**; otherwise open and jump
  after load (`pending_forward`). The discovery/forward runs before any GTK init
  (`shell::dbus::forward_to_running_instance`), so it needs no display.
- **Reverse (reader → editor).** A capture-phase Ctrl+primary-click user-script
  walks up to the nearest `[data-sourcepos]` ancestor and posts its line over a
  new `editorsync` script-message handler; the shell substitutes it into
  `editor-command` and spawns the editor detached via `gio::Subprocess` (reaped
  by the main loop, never blocking). Only Ctrl+click is intercepted, so plain
  clicks / link routing / selection are untouched; failures are statusbar notices.
- **`editor-command` (config, typed).** Default `$EDITOR +%l %f` (zathura
  placeholder style). Parsed once into `core::editor::EditorCommand` — a typed
  argv template (tokens of literal / `%l` / `%f` segments), so substitution is a
  pure fold and a path with spaces stays one argument (argv-based spawn, not a
  shell). The shell expands a leading `$VAR` per token at spawn time. Config-only,
  like `[renderers]` (not a `:set` target).
- **Source-line map.** Enabled comrak `render.sourcepos` → `data-sourcepos` on
  every native block/inline element: attribute-only, so **zero structural/CSS
  change** (chosen over wrapping blocks in marker divs, which would break the
  stylesheet's child/sibling selectors). The code-fence passes replace their node
  with a raw `HtmlBlock` (which comrak emits without sourcepos) but keep its
  `.sourcepos`, so one core pass (`pipeline::annotate_html_block_lines`) injects a
  matching `data-sourcepos` into each wrapper's opening tag (synthetic table-wrap
  divs marked line 0 → skipped). One uniform attribute; forward and reverse JS
  read the same thing; document order keeps start lines non-decreasing.

**Deviation from the D7 sketch:** the attribute is comrak's `data-sourcepos`
(explicitly the design's first-choice option), not a custom `data-line` — its
native, no-layout-change emission is why it wins. Consequence: inline elements
are annotated too, which only sharpens reverse-click precision.

**Tests:** **146 → 160 unit** (`core::editor`: 8 — placeholder parse/substitution,
spaces-in-path, `%%`, unknown/trailing `%`, empty-template error; `core::config`:
2 — `editor-command` default/parse + empty error; `core::pipeline`: 4 —
`inject_sourcepos`, block+wrapper line emission, monotonic non-decreasing lines,
table-wrap carries no line). **26 → 30 e2e** (`GotoLine` scrolls; `--forward`
fresh-launch jumps after load; second-instance `--forward` drives the running
reader and exits 0 without a window; reverse Ctrl+click spawns `editor-command`
into a recorder script and the argv is asserted — deterministic, not flaky). A
handful of pipeline unit tests loosened exact-tag asserts (`<table>` → `<table`,
etc.) now that `data-sourcepos` is present. `cargo test && clippy -D warnings &&
fmt` all clean. No new dependencies.

Next: AUR package and stdin streaming remain for M3.

## 2026-07-06 — fix: MathML layout broken by mathjax2 font shadowing (D8)

**What:** on this machine MathML rendered with wildly wrong vertical layout —
superscripts flung ~6 line-heights above the base, fractions split across lines.
**Root cause:** the vendored pulldown-latex `@font-face` for the math font listed
`src: local('Latin Modern Math'), local('LatinModernMath-Regular'), url(…woff2)`.
The Arch `mathjax2` package installs MathJax v2's split webfonts system-wide and
registers the family name "Latin Modern Math" (`fc-match "Latin Modern Math"` →
`LatinModernMathJax_Alphabets-Regular.woff`) — subsets with **no OpenType MATH
table** and deliberately huge ascent metrics. WebKit resolves the `local()`
source first, gets that subset, and derives its math layout constants from the
garbage metrics.

**Diagnosis (system MiniBrowser, three-way experiment in the same engine):**
default font list → broken; forcing `DejaVu Math TeX Gyre` → correct; our
vendored woff2 under a UNIQUE family name (`JumanjiMath`) → pixel-perfect. So the
font *file* was fine; the family-name *resolution* was poisoned.

**Fix (minimal, `src/core/assets/math/styles.css`):** removed every `local()`
source and renamed the embedded families to unshadowable names — `Latin Modern
Math` → `Jumanji Math`, `LMRoman12` → `Jumanji Roman` (and dropped the shadowable
`Latin Modern Roman` fallback from `m|mtext`). The page is self-contained by
design (D8), so consulting system fonts was only ever a nondeterminism hazard.
`core::math`'s url()→data: rewrite matches url tokens, not family names, so it was
unaffected (verified). A comment block at the top of the vendored sheet documents
the patch and why.

**Regression tests:** unit — `math_css()` must contain no `local(` and must name
the unique families. e2e (headless Xvfb) — a **geometry** assertion that would
have caught this: for the demo's `$E = mc^2$`, a new `msup_shift_ratio` probe
(`(base.top − sup.top) / base.height` of the first `<msup>`) must be a sane
fraction (0 < r < 1); the broken build measured ~6. Healthy value on this machine:
**0.73**. Wired the probe through `ViewportState`/`GetState`/`state_json` and the
e2e mini-parser, same narrow-probe pattern as `math_width`/`fence_width`.

Tests: **145 → 146 unit**, **25 → 26 e2e**, all green; clippy/fmt clean. DESIGN
D8 gains the no-`local()`/unique-family constraint.

## 2026-07-06 — external fence renderers (M3, DESIGN D6 seam 2)

**What:** any fenced code block whose language has a configured renderer is now
replaced by that command's output. Config gains a `[renderers]` table
(`d2 = "d2 - -"`, `dot = "dot -Tsvg"`, …); jumanji runs the command via `sh -c`
with the fence body on stdin and inlines its stdout (SVG/HTML). Covers graphviz,
d2, typst, … with no plugin API — the same pipeline seam merman occupies
internally.

- **Placement (D6.2): core, injectable.** New pure module `core::fence` sits
  beside `diagram.rs`/`math.rs` and runs as the pipeline's first transform pass.
  It is the first core code to spawn a subprocess, but the functional-core
  boundary holds: the exec is local, `Result`-shaped I/O (no display), and the
  transform is injectable — `transform_fences(root, &renderers, &run)` takes the
  runner as a closure, so unit tests drive it with a fake and `pipeline::render`
  passes the real `fence::run_command`. No-network rule untouched (subprocesses
  are local; the page CSP still blocks all egress).
- **Contract:** `language = "command"`, run via `sh -c`, fence body on **stdin**,
  **stdout** replaces the fence. Minimal by design — no temp files, no `%f`
  substitution. Language keys lowercased and matched case-insensitively. Typed
  as `BTreeMap<String,String>` on both `config::Options` and `pipeline::Options`;
  parsed from a free `[renderers]` table (no `deny_unknown_fields`). Not a `:set`
  target (a table, wired once at render-construction; live reload re-runs the
  pipeline, so renderers re-execute for free).
- **Safety:** hard **5 s** timeout (child killed on expiry, via a reader thread +
  `recv_timeout`), **4 MiB** stdout cap, stderr discarded. Every failure — spawn
  error, non-zero exit, timeout, over-cap, empty/non-UTF-8 output — degrades to
  the fence as a highlighted code block + a `.diagram-error` note (mirrors
  `diagram.rs`). No `catch_unwind` needed here (outcomes are `Result`-shaped, no
  panic to contain).
- **Output container:** a `.rendered-fence` block that is *only* an
  `overflow-x: auto` scroll box, so wide output scrolls in its own box, never the
  page (same invariant as `.mermaid`/`.table-wrap`/`.math-scroll`). Deliberately
  **no `--dw` intrinsic-width parsing** — the output is arbitrary SVG *or* HTML,
  so a plain scroller is the honest primitive.
- **Mermaid override:** a configured `mermaid` renderer wins over the built-in
  merman path — `transform_fences` runs before `transform_mermaid`, so a consumed
  fence is no longer a `CodeBlock` by the time the built-in pass runs. Documented
  and tested both ways.
- **Observability:** `ViewportState`/`GetState` gain `fence_width` (first
  `.rendered-fence svg` width), the same probe pattern as `diagram_width`/
  `math_width`.

Tests: **123 → 145 unit** (`core::fence`: transform with a fake runner +
`run_command` with deterministic commands — `cat`/`printf`/`false`/`true`/`sleep
10`/over-cap/non-UTF-8; `core::pipeline`: replace, untouched, degrade, garbage,
mermaid override; `core::config`: `[renderers]` parse + key lowercasing) + **24
→ 25 e2e** (`external_fence_renderer_produces_output` writes a private config
with an echo-SVG renderer and asserts `fence_width` via `GetState`). `cargo test
&& clippy -D warnings && fmt` all clean. No new dependencies (the runner is std
`Command` + threads + `mpsc::recv_timeout`). DESIGN D6.2 filled in; M3 line
updated; README + config.example.toml document the `[renderers]` table.

Next: editor sync (D7) and stdin streaming remain for M3.

## 2026-07-06 — three user-reported bug fixes: dark-mode syntax colour, GPU layer dropouts, drag-select

Three independent user reports, three small fixes.

- **Dark mode: python function name unreadable (fixed).** The classed syntect
  CSS emitted the light theme (`InspiredGithub`) *unscoped* and the dark theme
  (`Base16OceanDark`) under `html.dark`. `InspiredGithub` ships a deeply-scoped
  `.source.python .entity.name.function { color:#323232 }` (specificity 0,5,0)
  that outranked the dark block's `html.dark`-nested `.entity.name.function`
  (0,4,1), leaking near-black text onto the dark background. Fix: scope the light
  block under `html:not(.dark)` symmetrically with `dark_css`'s `html.dark`, so
  neither theme can apply in the other mode regardless of specificity — tokens
  the dark theme doesn't colour fall back to the `.code` foreground
  (`core::highlight::light_css`). Unit test extended (`light_css` starts with
  `html:not(.dark) {`); new e2e `dark_mode_python_function_name_is_readable`
  drives Ctrl-r and asserts the demo's `def fib` function-name colour is
  `rgb(50, 50, 50)` in light mode (probe-target check) and *not* that near-black
  in dark. Needed a narrow observability probe: `ViewportState`/`GetState` gain
  `fn_color` (computed `color` of `.entity.name.function.python`), the same
  pattern as `math_width` — `ViewportState` drops `Copy` (now holds a `String`).
- **j/k scrolling makes elements vanish on the real GPU (mitigated, not
  headless-reproducible).** Composited `overflow-x: auto` layers (tables, code,
  diagrams) intermittently drop out mid-scroll on the user's Intel UHD 630 / Mesa
  / X11 — the WebKitGTK DMABUF-renderer artifact class (WebKit bug 262607
  family). Fix: `main` sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` *only if unset*
  (an explicit value — even "0"/empty — wins, keeping a no-config escape hatch),
  at the very top of `main` before any GTK/WebKit init (documented `unsafe`
  `set_var`: single-threaded, pre-spawn, so edition-2024-sound). Xvfb has no GPU
  path so this can't be e2e-asserted; verified on evidence + upstream precedent
  (installed webkitgtk-6.0 2.52.4 still honours the var), user feel-tests before
  release. Recorded in DESIGN Risks as a binding default.
- **Drag-of-selected-text disabled; press-in-selection restarts selection.** A
  document-start user-script (via the existing `UserContentManager`, alongside
  copy-on-select and hints) adds two capture-phase listeners: `mousedown` inside
  the current non-collapsed selection (hit-tested against the range's client
  rects) calls `removeAllRanges()` so a fresh drag-selection starts from the
  press; `dragstart` `preventDefault()`s as belt-and-braces. Zathura/plain-text-
  area semantics. Shell viewport glue, not content-pipeline JS (D3 governs the
  *rendering* pipeline). No e2e: a real selection drag under Xvfb is the same
  flaky press-move-release the copy-on-select tests already decline to exercise
  (see the note above `j_and_k_scroll`) — the pieces (script installed, listeners
  capture-phase) are deterministic; the drag interaction is manually verified on
  the real display.

Tests: **123 unit** (count unchanged; the highlight scoping test extended to pin
`html:not(.dark) {`) + 23 → **24 e2e** (new
`dark_mode_python_function_name_is_readable`; the suite's tiny JSON parser
taught to scan quoted string values — `fn_color` contains commas).
`cargo test && clippy -D warnings && fmt` all clean. No new dependencies.

Next: user feel-test on the real GPU — scrolling (DMABUF workaround) and
selection (drag-select) are the two that can only be confirmed there.

## 2026-07-06 — LaTeX math → MathML Core (M3), no JavaScript

Math is the first M3 feature to land. **What:** `$inline$` / `$$display$$` /
`` $`code`$ `` math now renders as native MathML.

- **Pipeline (D8).** comrak's math extension (`math_dollars` + `math_code`)
  yields inline `NodeValue::Math` nodes; a new pure `core::math` module walks the
  AST and replaces each with an inline `<math>` fragment rendered by
  **pulldown-latex 0.7.1** (pure-Rust LaTeX → MathML Core, ~95% KaTeX coverage,
  MIT). Same parse → mutate → format shape as `diagram.rs`. `$…$` renders inline
  style, `$$…$$` block. **Why pulldown-latex, not typst** (a whole document
  compiler for a fragment — huge dep, wrong output model) **or KaTeX/MathJax**
  (JavaScript, which D3 forbids).
- **Fonts served as base64 `data:` URIs, not `app://`.** The brief assumed an
  `app://` scheme existed; it does not — the code inlines everything
  (`style-src 'unsafe-inline'`). So math stays consistent: `core::math` rewrites
  pulldown-latex's `styles.css` font `url()`s to base64 `data:` URIs at runtime
  (cached once; hand-rolled base64, no new dep). CSP gains exactly `font-src
  data:`. The Latin Modern fonts (4 × WOFF2, ~0.5 MB, GUST Font License) and the
  stylesheet are vendored under `src/core/assets/math/`; the math stylesheet is
  emitted **only when the document contains math**, so math-free pages are
  unaffected. D3's `app://` serving note updated to match reality; new D8 added.
- **Recolor + no-page-h-scroll, by construction.** MathML inherits `color`, so
  equations recolor with Ctrl-r for free; the one hardcoded black in the vendored
  sheet (the negation-slash gradient) is patched to `currentColor` (marked
  `jumanji:`). Display math is wrapped in a `.math-scroll` block (`<span>` set to
  `display:block` — valid inside the enclosing `<p>`, unlike a `<div>`) so a wide
  matrix/alignment scrolls in its own box, never the page — same mechanism as
  `.table-wrap`/`.mermaid`. A narrow-viewport e2e caught this: raw block math
  overflowed the page by ~20 px until wrapped.
- **Graceful degradation (binding).** A parser error (pulldown-latex emits inline
  `<merror>`) or an unbalanced environment (which *panics* inside pulldown-latex's
  writer — contained by `catch_unwind`) degrades to the raw source as a code span
  (inline) or a small error box with a note (display). Never a crash, never blank.
- **State + demo.** `GetState`/`ViewportState` gain `math_width` (first `<math>`
  rendered width, mirroring `diagram_width`) so e2e can assert MathML laid out.
  `demo/demo.md` gains a Math section (inline, display sum, matrix + determinant,
  Maxwell `aligned`, and a deliberately-broken fence).

Tests: 108 → **123 unit** (10 new in `core::math`, 5 in `core::pipeline`
including the comrak dollar-rule documentation test) + **23 e2e** (was 21; new
`math_renders_as_mathml_with_width`). `cargo test && clippy -D warnings && fmt`
all clean. Crate added: `pulldown-latex 0.7.1`. Vendored assets: 540 KB.

Next M3: external fence renderers (D6.2 — the same seam merman occupies), D-Bus
editor sync (D7), stdin streaming. Note for those: the pipeline contract is still
"pure core emits one self-contained HTML string"; `has_math`-style conditional
asset inclusion is the pattern to copy for any new heavyweight asset.

## 2026-07-06 — intrinsic diagram size, cursor-anchor fix, leading-edge zoom, one demo

Four related fixes, driven by user feedback that big diagrams were unreadably
small at zoom 1 and that Ctrl+wheel didn't zoom towards the cursor near the
bottom of the page:

- **Diagrams render at intrinsic (natural) size.** The old model shrank every
  diagram to fit the reading column, so a large flowchart was tiny at zoom 1 and
  zooming to read it blew the prose up. New model: `core::diagram` parses the
  intrinsic pixel width from merman's SVG root (`max-width:<N>px`) and emits
  `<div class="mermaid" style="--dw:<N>px">`; `.mermaid svg` is now
  `width: var(--dw, auto)`. A diagram wider than the column renders full-size and
  overflows into its own `.mermaid` scroll box, never the page. Because WebKit's
  native geometric zoom multiplies CSS px → device px, device size = intrinsic ×
  zoom *for free* — so the `--zoom` custom property and the old
  `min(100vw × --zoom − chrome, …)` formula are **gone** (`--diagram-chrome`
  deleted with them). Parse-failure degrades to a plain wrapper (svg `auto`).
  Text-zoom invariance is now by construction (diagrams don't read `--font-size`).
  Sample intrinsic widths: flowchart-LR 1200 px, gantt 1184 px, timeline 1390 px,
  sequence 650 px.
- **Cursor-anchored zoom bug fixed.** `flush_wheel_zoom` set `Shell.zoom` to the
  new level *before* calling `cursor_anchor`, but the capture JS runs while the
  page is still laid out at the OLD zoom; `cursor_anchor` divides the pointer by
  the zoom to get CSS px, so it used the wrong divisor and the error grew with
  distance from the origin — hence "doesn't zoom towards the cursor" near the
  bottom. Now the anchor is captured from the pre-change zoom. New e2e drives a
  real Ctrl+wheel with the pointer ~80 % down and asserts the reflow direction +
  no page h-scroll.
- **Leading-edge wheel-zoom coalescing.** Every Ctrl+wheel tick used to wait the
  full 40 ms before anything happened. Now the first tick of a burst applies
  immediately and the 40 ms timer only batches ticks that arrive during the
  trailing window (no-op if none do); no tick is ever lost. Combined with the
  `--zoom` removal — which drops a root-level custom-property write (a full
  document style invalidation) from *every* zoom step — this is the zoom-latency
  win. (WebKit reflow remains the measured floor; no other speculative perf.)
- **Native zoom survives reload**, so the load-finished handler no longer
  re-applies geometric zoom (only text zoom's `--font-size`, which *is* lost on
  reload). Verified: `zoom_level` is a WebView property, and the live-reload e2e
  now zooms in a narrow window and asserts the CSS viewport stays collapsed
  across the reload.
- **One demo.** `docs/demo.md` (the richer 11-diagram rendering showcase) is now
  the single `demo/demo.md`, with the run command + image path fixed, the
  zathura driving hint expanded (Tab/TOC, f/hints, q), and the old demo's
  unknown-language-fence coverage folded in. The duplicate `Notes` headings
  (anchor disambiguation) are preserved. Old `docs/demo.md` deleted.

e2e: `narrow_viewport_zoom_reflows_without_page_overflow` gains an assertion that
at zoom 1 the diagram width exceeds the narrow viewport (overflow inside its
box); device-growth assertion unchanged. New `ctrl_wheel_zoom_anchors_with_cursor_near_bottom`.
108 unit + e2e.

Next: user feel-test on real GPU (intrinsic diagrams, cursor anchor, leading-edge
snappiness).

## 2026-07-06 — v0.2.1: zoom mechanics reworked — reflow, cursor anchor, coalescing

User feedback on v0.2.0: scrolling fixed, but zoom felt janky, should zoom
towards the cursor, and should *reflow* the text — no horizontal page scroll
after zooming. That reverses the reflow-free design from earlier today, so
D5a was rewritten rather than patched (the original requirements it served —
diagrams must really zoom, position must not drift — still hold and are now
met by construction instead of by rigidity):

- **Text reflows**: the column re-fits the viewport again; the page never
  scrolls horizontally (e2e-asserted: `doc_scroll_width ≤ viewport_width` at
  every zoom × width combination). A pre-existing overflow leak surfaced by
  the new assertion — an unbreakable long-URL link — fixed with
  `overflow-wrap: break-word` on the column.
- **Diagrams keep zooming**: `.mermaid svg` pins its CSS width to the zoom-1
  value (`min(100vw × --zoom − chrome, --content-width − chrome)`; chrome in
  `rem`, invariant under text zoom since `rem` tracks the fixed root font,
  not `--font-size`). Device width scales ∝ zoom — verified empirically over
  zoom {0.7, 1, 1.5, 2} × window {500, 800, 1200}; overflow scrolls inside
  the `.mermaid` box. `.mermaid` padding normalised 1.1em → 1.25rem (0.4 px).
- **Anchored zoom, one mechanism**: capture the element under a probe point +
  its viewport offset, apply the zoom, scroll it back. `Ctrl`+wheel probes
  the cursor (motion-controller-tracked; GTK→CSS coords divide by zoom);
  keys/D-Bus/text zoom probe the viewport top. Race-free by sequencing the
  native `set_zoom_level` inside the capture eval's completion callback.
  `Shell.zoom` is now the source of truth (native level lands async).
- **De-janked**: wheel ticks coalesce into one anchored application per
  ~40 ms (10-tick burst: 10 reflows → 1, settle ~5.5× faster under Xvfb;
  absolute feel needs the user's GPU).

e2e: `narrow_viewport_zoom_does_not_reflow` rewritten as
`narrow_viewport_zoom_reflows_without_page_overflow` (the old invariant is
intentionally dead); new tests for position anchoring (±8 % band), real
synthetic Ctrl+wheel at the pointer (XTEST button 4/5 works under Xvfb), and
burst coalescing. 105 unit + 21 e2e.

Next: user feel-test on real GPU (cursor anchor, jank, 40 ms window).

## 2026-07-06 — v0.2.0: performance investigation — measure, don't guess

User: "startup could be faster, scrolling feels janky." A dedicated
measurement pass (release build, headless Xvfb, medians over runs, ffmpeg
frame capture for what the user actually sees) instead of speculative fixes:

- **Startup ≈ 950–1050 ms is a WebKitGTK floor, not app code.** The Rust
  pipeline renders the full demo (11 mermaid diagrams) in **20 ms**; WebKit
  web-process spawn (253 ms) + one-time engine warmup/layout (436 ms)
  dominate, with a blank window from ~290 ms until content pops. Every
  surgical fix was tested and disproven (pre-warm, load-before-present —
  250 ms *worse*, hwaccel toggles, a11y off, font trims: all ±0). This is
  DESIGN.md D2's documented risk materializing. Real levers are
  architectural and deferred: a daemon/window-reuse mode over the existing
  D-Bus seam (a warm process loads in 35 ms), or D2's egui escape hatch.
- **Scroll jank = smooth scrolling × SVG compositing.** Headless scrolling
  is mechanically flawless (zero lost distance over 60-key storms; the
  per-key status JS round-trip is 0.28 ms — debouncing would be pointless).
  But `enable_smooth_scrolling(true)` makes WebKit animate every wheel tick
  (~100 ms, 4× the composited frames for the same distance). **Applied:**
  smooth scrolling off + explicit `behavior:'instant'` in the scroll JS —
  zathura semantics, and a repeated `j` can never restart an in-flight
  animation. Feel needs confirming on the real GPU (Xvfb has none), along
  with two user-side experiments: `WEBKIT_DISABLE_DMABUF_RENDERER=1` and
  touchpad inertia through the capture-phase scroll controller.
- **Rendering needs no work in release.** Syntax set/theme already
  OnceLock-cached; live reload 57–78 ms end-to-end. Debug builds were
  10–20× slower in the pipeline: **applied** `[profile.dev.package."*"]
  opt-level = 2` (deps optimized once, the crate itself stays fast to
  rebuild). Disproven-and-not-built: status-bar debounce, mermaid
  content-hash cache, syntax-set pre-warm thread.

## 2026-07-06 — v0.2.0: milestone M2 — TOC, hints, command line, marks, persistence

M2 complete, split core-first then shell, both halves by Opus subagents
against a pinned contract; 105 unit + 18 e2e tests, clippy/fmt clean.

Core (pure, unit-tested): `command.rs` (`:` parse + completion; path
completion delegated to the shell via `Completions::Path`), `jumplist.rs`
(vim semantics, cap 100), `marks.rs` (char registers, position+zoom),
`history.rs` (per-file state, TOML array-of-tables for lossless LRU
round-trips, cap 500), keymap char-argument bindings (`m`/`'` capture the
next key as a typed argument — no stringly dispatch), `Mode::Toc` default
table, runtime `Options::set` returning a typed `SetEffect`
(Rerender/Recolor/None), comrak GFM alerts (verified against vendored
source: `extension.alerts`, `.markdown-alert-<kind>` classes) styled via
`--alert-*` accents + `color-mix` tints, and `extra_css` emission for user
themes.

Shell: TOC mode as a `gtk::Stack` page (collapsible outline tree, dispatcher
driven, counts work); `f`/`F` link hints (overlay JS labels visible links,
posts `label\thref` back; shell-local hint input state, *not* a keymap
mode); navigation policy that denies everything except our programmatic
loads — fragment clicks scroll in-place (jumplist push), local `.md` links
open in-window and re-point the watcher, anything else goes to the system
handler (the reader itself stays offline); `:` command line with typed
prompts, Tab completion incl. real filesystem completion; quickmarks +
jumplist wired through async scroll queries; per-file scroll/zoom/text-zoom
persistence at `$XDG_DATA_HOME/jumanji/history.toml` (restored on open,
flushed synchronously on close — two RefCell-reentrancy bugs found by the
new e2e tests and fixed); user CSS themes hot-swapped by a second directory
watcher; `GetState.mode` now truthful (normal/toc/hint/command/search).
The e2e harness gained private `XDG_DATA_HOME` isolation and a
clean-quit + relaunch flow to prove persistence.

Deliberately skipped: window-geometry persistence (core history stores
per-file entries only; a second format wasn't worth it), mouse-click e2e for
fragments (hint-follow exercises the identical routing deterministically).

Next: M3 — editor sync (D-Bus forward/reverse), external fence renderers,
math, stdin streaming; decide on the AUR publish.

## 2026-07-06 — Arch/AUR packaging

Added real Arch Linux packaging so jumanji installs and upgrades like any
other system package instead of living in `~/.local/bin`.

- `resources/jumanji.desktop`: standard desktop entry (`Exec=jumanji %f`,
  resolved via `$PATH` — not a hardcoded install path — since the system
  package puts the binary on `$PATH`), registers as a `text/markdown` /
  `text/x-markdown` handler.
- `resources/config.example.toml`: every `[options]` key documented at its
  built-in default, cross-checked against `RawOptions`' serde renames in
  `src/core/config.rs` rather than guessed from the README, plus a couple of
  commented `[keys.normal]` remap examples.
- `packaging/aur/PKGBUILD` + generated `.SRCINFO`: sources the GitHub release
  tarball for the tagged version, `cargo fetch --locked` in `prepare()`,
  `cargo build --frozen --release` in `build()`, installs the binary, desktop
  file, example config, LICENSE, and README.
- **Real build hazard found and fixed**: a clean `makepkg -f` reliably failed
  to link — `undefined symbol: onig_new` and friends from `onig_sys` (pulled
  in by syntect). Root cause: Arch's default `CFLAGS` carries `-flto=auto`,
  and `onig_sys` compiles bundled oniguruma C sources via the `cc` crate
  using that `$CFLAGS`; the resulting objects are GCC-only LTO bitcode that
  rustc's default `lld` linker can't resolve. Fixed with
  `CFLAGS+=' -ffat-lto-objects -w'` in `build()` — same fix the official
  `bat` package uses for the identical syntect+onig combination. A first
  build succeeding only after a second incremental rebuild would have been
  an easy trap (looks like a flaky race; it's actually deterministic and
  CFLAGS-dependent) — worth remembering for any future dependency that
  vendors C sources.
- Verified end-to-end: `makepkg -f` in a scratch copy produces
  `jumanji-0.1.1-1-x86_64.pkg.tar.zst` containing `/usr/bin/jumanji`
  (dynamically linked, stripped, executable), the desktop file, example
  config, and LICENSE. Not installed (no `pacman -U`) and not pushed to the
  AUR — the PKGBUILD currently points at the already-tagged `v0.1.1` GitHub
  release tarball, which predates `resources/` and `packaging/`, so
  packaging from the public tarball won't have the desktop file/config until
  a new tag (e.g. `v0.1.2`) is cut that includes them.

Next: cut a release that includes `resources/` and `packaging/`, then
publish to the AUR.

## 2026-07-06 — reflow-free geometric zoom (narrow-viewport diagram bug)

User-reported: mermaid diagrams didn't zoom when the window was narrower than
`page-width` (they did on wide windows), and zooming on a narrow viewport
drifted the reading position. One root cause: the column was sized
`max-width: var(--content-width)`, so on a narrow window its width tracked the
viewport — and webkit zoom *shrinks* the CSS viewport, so the column reflowed
to re-fit and `max-width: 100%` re-fit the SVGs. Net visual change: zero.

Fix (one rule): the shell mirrors `zoom_level` into a `--zoom` custom property
(re-applied after each load, like `--font-size`), and the stylesheet sizes the
column `width: min(var(--content-width), calc(100% * var(--zoom, 1)))`. The
layout width in CSS px is now invariant under zoom → no reflow → position
stays put, and content overflows the window edge instead of re-fitting
(pan with `h`/`l`) — zathura semantics. DESIGN.md D5a updated.

Testability: `GetState` now reports `content_width` (the column's layout width,
CSS px); a new e2e test narrows the window to 500 px, zooms ×1.5, and asserts
width invariance + real device-px growth. Asserts poll (the `--zoom` set is an
async JS eval) and the baseline waits for two stable readings after resize.

Next: M2 features, startup/scroll performance.

## 2026-07-06 — v0.1.1: config format bug — `[options]` was silently ignored

Found while verifying the installed release: `default-recolor = true` had no
effect. The parser expected option keys at the TOML **top level**, while the
README/DESIGN (and the user's config) put them in an `[options]` table — and
serde silently ignored the unknown `options` field, so the whole table was
swallowed with no error. The "fonts worked" evidence was a red herring: the
stylesheet defaults happen to match the configured fonts.

- Implementation now matches the documented format: `[options]` +
  `[keys.<mode>]`.
- `deny_unknown_fields` on every raw config struct: a misplaced or misspelled
  key now errors loudly (still non-fatal — stderr + defaults). Regression test
  asserts the old top-level format errors.
- e2e isolation hole closed: the harness inherited `$HOME`, so tests read the
  developer's real `~/.config/jumanji/config.toml`; the app child now gets a
  private empty `XDG_CONFIG_HOME`.

Released as **v0.1.1** (v0.1.0 shipped with the bug).

## 2026-07-06 — v0.1.0: `+`/`-` back to geometric zoom, anchored text zoom

User feedback after trying the two-axis zoom: `+`/`-` should do geometric
zoom (zathura muscle memory), and text zoom made the viewport jump — reflow
moves content, so a fixed pixel offset lands somewhere else in the document.

- Defaults swapped: `+`/`-` → geometric (`zoom in`/`zoom out`); text zoom is
  `Ctrl+Shift+wheel` / config-only.
- Text zoom now anchors the reading position: capture the element at the top
  of the viewport (probing a few points down the content column's center,
  skipping body/main containers), apply the font-size change, then
  `scrollIntoView` the captured element. No-op at the very top.
- Tagged **v0.1.0**; release build installed to `~/.local/bin/jumanji`,
  registered as the `xdg-open` handler for `text/markdown`.

## 2026-07-06 — Feature batch: two-axis zoom, copy-on-select, fonts, dark hardening

Five-feature batch. **54 core + 10 e2e = 64 tests, clippy `-D warnings` clean,
fmt clean.** Manually smoke-tested under Xvfb (copy-on-select, Ctrl+wheel and
Ctrl+Shift+wheel zoom, dark-first-paint).

- **Diagram-zoom diagnosis (done first, empirically).** Question: does webkit
  `zoom_level` scale mermaid SVGs? merman emits `<svg width="100%"
  style="max-width:<intrinsic>px" viewBox=…>`. Measured a diagram's
  `getBoundingClientRect` in a real WebView at zoom 1.0 vs 2.0 against the actual
  stylesheet: the svg's **layout** px stays constant (111.9) across zoom while
  the CSS viewport halves — i.e. it occupies 35 % → 70 % of the viewport from
  1.0× → 2.0×. **Conclusion: geometric zoom already scales diagrams correctly**,
  because `zoom-text-only` defaults off (full-page zoom scales the px unit
  itself, inline `max-width:<px>` included). No CSS fix needed — and a
  `max-width:100% !important` override was tested and *rejected* (it stretches
  small diagrams to fill and breaks proportional scaling).
- **Two-axis zoom.** `Action` split: `ZoomIn`/`ZoomOut` = geometric (webkit
  `zoom_level`), new `TextZoomIn`/`TextZoomOut` = the `--font-size` CSS var on
  `<html>` (reflow, no geometry change, clamped 8 px…3× base), `ZoomReset` resets
  both. Defaults: `+`/`-` → text zoom, `=` → reset both; geometric has no default
  key (config `zoom in`/`zoom out` — zathura spelling — and `Ctrl`+wheel).
  `Ctrl`+wheel = geometric, `Ctrl`+`Shift`+wheel = text; plain wheel scrolls
  untouched. Statusbar shows `150%/120%T` when either axis ≠ 100 %. `GetState`
  gains `text_zoom`.
- **Copy-on-select (zathura parity).** `WebView` now built with a
  `UserContentManager`; an injected user-script posts the debounced (~200 ms),
  non-empty selection to a `selection` message handler; Rust writes it to the
  primary or clipboard selection per new `selection-clipboard` option (default
  `primary`). Empty selections post nothing.
- **Font config.** New `font-body`/`font-mono`/`font-size` options thread into
  the pipeline and emit CSS-escaped, quoted `--font-*` overrides in the generated
  `:root{…}` block (the stylesheet already consumed these vars). `font-size` is
  the text-zoom base.
- **Reload/dark hardening (flicker follow-up).** `View` tracks desired dark state
  internally; `load_document` pre-applies `class="dark"` on `<html>` so reloads
  and `default-recolor = true` starts paint dark from the first frame, and the
  WebView's native background color switches with the theme (white / `#1a1a1a`)
  so unpainted regions never flash.

**Deviation from brief:** the Ctrl+wheel controller is attached to the
**toplevel window** (capture phase), not the WebView. A capture-phase controller
on the WebView never receives the scroll events (verified: 0 events on the
webview, fires on the window) — the window+capture placement is the same
architectural guarantee the key controller already relies on (DESIGN D4).

Copy-on-select is not e2e'd (a real selection drag under Xvfb is flaky); the
clipboard-target parsing is unit-tested and the write path is a thin
`Clipboard::set_text`. Wheel zoom is likewise verified manually, not in the
suite (`xdotool click 4/5` only delivers scroll pointer-based, never via
`--window`).

**Next:** M2 — TOC mode, `:` commands, link hints.

## 2026-07-06 — Flicker root-caused: self-sustaining reload loop

User-reported "flicker after scrolling" reproduced headlessly (Xvfb + 30 fps
ffmpeg frame capture + per-frame brightness analysis): a sustained ~4 Hz
light/dark oscillation. Root cause: `watch.rs` filtered file events by *path*
only, so `Access` (read) events counted as changes — and the reload handler
itself reads the file, so **one external read of the document started an
infinite reload loop** (read → Access event → debounce 150 ms → poll 120 ms →
reload → read → …). Every cycle reset scroll and repainted; in dark mode each
fresh load painted light before recolor re-applied, hence the strobe.

Fix: filter to content-mutating event kinds (`Create`/`Modify`/`Remove`).
Verified with the same frame harness under deliberate external reads:
150 frames, brightness stddev 0.003, zero flashes (buggy build: 0.374
oscillation amplitude). Remaining hardening queued: pre-apply the dark class
at load (no light flash on *legitimate* reloads) and set the WebView native
background to the theme color.

Testing method note: GUI verification now runs on Xvfb — never on the live
X session (the earlier live-session testing caused visible window flicker on
the desktop).

## 2026-07-06 — D-Bus state interface + headless e2e harness

Added a per-instance D-Bus service and a real-app e2e test suite. **53/53 tests
(46 core + 7 e2e), clippy `-D warnings` clean, e2e green across repeated runs
(~9.5 s/run under Xvfb).**

- `shell/dbus.rs`: zathura-style per-instance service — owns
  `org.membranepotential.jumanji.PID-<pid>` on the **session** bus, object
  `/org/membranepotential/jumanji`, interface `org.membranepotential.jumanji`.
  Built on `gtk::gio` (`bus_own_name` + `register_object` with introspection
  XML) — **no new deps, no zbus**. Two methods:
  - `GetState() -> (s)` — JSON snapshot (`file`, `scroll_y`, `scroll_percent`,
    `dark`, `zoom`, `mode`, `section`, `toc_len`, `loaded`). Scroll figures are
    queried live from the webview; the reply is completed from the async JS
    callback (`DBusMethodInvocation` finished later) so the main loop never
    blocks.
  - `ExecuteAction(s, u)` — parses via `core::config::parse_action` and runs the
    same `execute()` path the keyboard uses; unknown action → D-Bus error.
  The module is pure transport: the app injects two closures, so `dbus.rs` never
  sees `Shell` and `app.rs` never sees a `Variant`. This is deliberately the M3
  editor-sync (D7) foundation, not test-only. Name-acquisition failure (no
  session bus) logs to stderr and the reader still runs.
- `app.rs`: added a `loaded` flag (set on the first `LoadEvent::Finished`) so
  clients can wait for a driveable window — keys/actions before load are
  no-ops. Wired `serve_dbus`; `_dbus` owner id kept for process lifetime.
- `tests/e2e.rs`: spins up a private `Xvfb` + private `dbus-daemon` per test,
  launches the real binary on `demo/demo.md`, waits for `loaded`, injects real
  GTK keys via `xdotool`, asserts on `GetState`. RAII teardown even on panic;
  serialized behind a mutex (concurrent WebKit instances are flaky); skips
  cleanly if `Xvfb`/`xdotool`/`dbus-daemon` are absent. Covers j/k, counts,
  gg/G, Ctrl-r, +/=, J/K, and the pure-D-Bus ExecuteAction path.
- **Two Xvfb gotchas found and documented** (`docs/TESTING.md`): with no window
  manager, `xdotool key --window` is dropped unless the window is first given X
  input focus (`windowfocus --sync`; `windowactivate` needs EWMH); and Shift
  must be the explicit `shift+g` form, not the bare `G` keysym. A prior debug
  session's *stale overlapping app processes* — not a code bug — were what made
  section state look like it reset; a clean run is deterministic.

**Next:** M2 — TOC mode, `:` commands, link hints; and the reverse D7 direction
(modifier-click → `$EDITOR +line`).

## 2026-07-06 — M1 MVP: it renders, it scrolls, it recolors

Core pipeline and GTK shell implemented (in parallel, by two agents with
disjoint file ownership) and integrated on the first build: **46/46 tests,
clippy `-D warnings` clean.**

- `core/`: comrak (GFM + footnotes + header IDs) → AST passes (mermaid via
  merman → inline SVG, syntect classed highlighting, table wrapping) → complete
  HTML document with embedded CSS. TOC anchors are byte-identical to emitted
  ids (comrak's own `Anchorizer`). Light theme InspiredGitHub, dark Base16
  Ocean Dark under `html.dark`.
- `shell/`: capture-phase `EventControllerKey` → pure `Matcher`
  (counts, `gg`, `<N>G`), actions executed via webkit6 (`scrollBy` JS, zoom
  level, FindController search, `classList.toggle('dark')` recolor), girara
  bar (filename · pending keys · percent), notify-based live reload with
  scroll restore.
- Gotcha found at integration: merman's `render` cargo feature is required
  for `merman::render` to exist (default features are empty).
- Verified live on X11 (screenshots): typography, tables, syntect fences,
  both mermaid diagrams, footnotes, `j`/`G`/`Ctrl-r` all behave; statusbar
  percent honest (queries actual `scrollY`).
- Known gaps: keys are silent no-ops until the initial load finishes
  (sub-second); section tracking (`J`/`K`) steps the TOC list rather than
  following the viewport; ToC mode and `:` commands are M2.

**Next:** headless e2e testing (Xvfb + D-Bus state interface — doubles as the
M3 editor-sync foundation).

## 2026-07-06 — Project inception

**Research.** Three parallel research passes (full write-ups in
[research/](research/)):

1. [Landscape survey](research/01-landscape.md) — no "zathura for markdown"
   exists; inlyne is closest but keyboard-less and mermaid-less; the "missing
   5%" (mermaid, math, callouts) is why users switch tools.
2. [Zathura architecture & UX](research/02-zathura.md) — girara's inputbar/
   statusbar/dispatch design, the zathurarc idioms, the full keybinding table,
   and the SyncTeX editor-pairing pattern.
3. [Rust stack evaluation](research/03-rust-stack.md) — webview vs native,
   the 2026 pure-Rust mermaid breakthrough (merman), parser/highlighter
   comparison.

**Girara-as-framework considered and rejected** (user suggestion, investigated
seriously): girara's GTK parts were stripped upstream in Feb 2026 and absorbed
into zathura as an internal static lib — no installable headers, no Rust
bindings, no introspection. We reimplement the small girara subset in Rust
instead, with zathura's `girara-gtk/` as design reference.

**Architecture decided** (full record in [DESIGN.md](DESIGN.md)):

- Rust · gtk4-rs + system WebKitGTK 6 (webview for typesetting only)
- 100%-Rust content pipeline: comrak (GFM) + syntect/two-face (highlighting)
  + merman (pure-Rust mermaid → inline SVG). **No JavaScript pipeline.**
- Capture-phase GTK key controller → girara-style mode/count dispatch
- TOML config with zathura idioms; notify-based live reload
- License: MIT. Repo: github.com/membranepotential/jumanji

**Next:** cargo scaffold, then parallel implementation of core pipeline and
GTK shell (Opus subagents), integration, first running MVP.
