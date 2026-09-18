# Obsidian dialect and vault resolution

jumanji reads Obsidian-flavoured notes as Obsidian shows them: wikilinks that
resolve across the vault, embeds, callouts, highlights, comments, block ids and
frontmatter. The dialect is always on, and the vault root comes from markers in
the tree.

Decisions on this page:

- [D11: Obsidian dialect & vault resolution (post-1.0; implemented)](#d11-obsidian-dialect--vault-resolution-post-10-implemented)

Code: [`src/core/obsidian.rs`](../../src/core/obsidian.rs),
[`src/core/vault.rs`](../../src/core/vault.rs),
[`src/core/wikilink.rs`](../../src/core/wikilink.rs),
[`src/core/callout.rs`](../../src/core/callout.rs),
[`src/core/frontmatter.rs`](../../src/core/frontmatter.rs),
[`src/core/textscan.rs`](../../src/core/textscan.rs).
Research: [04 — Obsidian](../research/04-obsidian.md).

## D11: Obsidian dialect & vault resolution (post-1.0; implemented)

Obsidian is the largest population of markdown jumanji renders badly today:
frontmatter shows as garbage, `[[wikilinks]]` as literal text, 22 of the 27
callout spellings as `[!question]`, and `%%comments%%`/`^block-ids` leak into
the prose. [`docs/research/04-obsidian.md`](../research/04-obsidian.md) is the ground truth for the dialect
and for comrak 0.53's behaviour; where these decisions differ from its §6
proposal, these win.

- **Always on, and zero new config options.** The dialect is not gated on
  detecting anything: one document must render the same wherever it sits, and a
  plain folder of notes (Zettelkasten, Foam, Dendron, a docs tree) is exactly
  the case a gate would break. Markers pick the vault *root* (the `core::vault`
  bullet below); they never decide whether the dialect is on, and a tree with no
  marker at all still gets every construct. The cost is
  that a literal `[[x]]` outside a notes collection renders as inert
  unresolved-link text; inline code
  and fences are untouched (comrak parses those first), so the realistic blast
  radius is unfenced shell `[[ -z "$x" ]]` in prose. Accepted. *Rejected:* a
  `obsidian = true/false` option — it is the "config option with one caller"
  the conventions forbid, and it makes rendering depend on invisible state.
- **Four constructs are comrak flags, not code.** Turn on
  `extension.highlight` (`==x==` → `<mark>`, exactly Obsidian's semantics),
  `extension.inline_footnotes` (`^[…]`, folded into the existing footnote
  numbering — better than the parenthetical degradation the research assumed),
  `parse.relaxed_tasklist_matching` (`- [-]`/`- [?]` stop rendering as literal
  text) and `extension.front_matter_delimiter = Some("---")` (YAML properties
  stop rendering as a setext heading + thematic-break mess). `extension.alerts`
  is turned **off** — the callout pass below subsumes it. Enabling front matter
  changes one non-Obsidian case too: a document opening with a thematic break
  and containing a later `---` line loses that span. Obsidian and every static
  site generator have the same behaviour; accepted.
- **Frontmatter is hidden by default, and showing it is a rendering, not a
  dump.** Hidden is the default because a note should open as prose rather than
  as a block of machine metadata — and it is also the *free* path: comrak parses
  frontmatter into a node its formatter already skips, so hiding costs nothing
  and showing is one AST pass swapping that node for raw HTML (keeping its
  source position, so [D7](../editor-sync/README.md#d7-editor-pairing--the-synctex-analogue-built) reverse click still lands on it). `:frontmatter`
  toggles it live and `show-frontmatter` sets the startup state. When shown it
  is a `<dl>` properties table, not the YAML source: the reason to ask for
  frontmatter is to read the values, and Obsidian shows properties as a table
  for the same reason. `core::frontmatter` parses it with the same deliberately
  shallow rules as `parse_aliases` below — one level of `key: value`, both list
  spellings, and *verbatim* text for anything with structure it does not model
  (nested maps, `|` scalars), degrading to the whole block verbatim when nothing
  parses. It never reshapes what it did not understand. *Rejected:* a YAML crate
  (a dependency to render metadata nobody asked to see), and a `<pre>` of the
  source (which is what the toggle exists to improve on).
- **`core::vault` — one `Vault`, rooted by marker walk-up from the opened
  document, pinned at launch, index rescanned off-thread per document load.**
  *(Supersedes this section's two earlier rationales: `.obsidian/` discovery
  with a second `Loose` mode, then the process CWD.)* `vault::root_for(doc)`
  walks up from the document's directory and takes **the nearest ancestor
  holding `.obsidian/`, else the nearest holding `.git/`, else the document's
  own directory.** Each marker is searched over the whole ancestor chain before
  the next is tried, so an explicit vault marker outranks an incidental repo
  marker no matter which sits closer.
  - **Why markers, having once rejected them.** The objection to `.obsidian/`
    was that it made rendering depend on a competitor's *private directory* and
    gave one document two meanings depending on invisible state. The first half
    does not survive scrutiny: a marker directory in the user's own tree is a
    fact about how those notes are organised, not a handshake with a program —
    jumanji reads it, it does not require Obsidian to exist. The second half was
    real but was aimed at the wrong thing: two *resolution modes* were the
    problem, and there is still only one. What replaced it — the CWD — turned
    out to be worse in practice. It made resolution depend on state that is not
    in the tree at all and is invisible in the launcher, desktop entry, or file
    manager that most opens actually come through: the same file rendered
    differently depending on which directory the user's shell happened to be in.
    `.git/` as the second marker covers the marker-less notes tree, which is the
    common case for anyone keeping notes in a repo.
  - **Pinned, not recomputed.** The root is resolved once, from the document
    jumanji was launched with, and held for the process. Recomputing it per load
    would let following a wikilink into a subfolder silently narrow the vault
    under the reader — you opened a collection, not a directory. Only the
    *index* is rebuilt per load, and because the root is pinned the index
    outlives any one document: a switch only rebinds which note "this one" is.
    `scan` walks `root` into a case-folded map of *filename* → path (plus full
    relative paths) and a map of frontmatter `aliases` → path. Resolution
    follows Obsidian: vault-wide by name, **root outranks a sibling folder**,
    the source path is only a tiebreaker, matching is case-insensitive, aliases
    participate. It is a table lookup, never a path join — so `[[../secrets]]`
    and `[[/etc/passwd]]` are simply not keys, and a wikilink cannot address
    anything outside the root.
  - **The index covers the vault, not the tree it sits in.** Two filters, both
    aimed at the `.git/` fallback rooting at a source repo. **Ignore files are
    obeyed** — `.gitignore`, `.ignore`, `.git/info/exclude`, the global
    gitignore, and the same files in parent directories, via the `ignore` crate
    (ripgrep's; gitignore's negation, `**`, and precedence rules are not worth
    reimplementing). `require_git` is off, so a `.gitignore` counts in a
    marker-less or `.obsidian/`-rooted vault too: the file is the user saying
    "this is not my content", and whether git happens to be watching is beside
    the point. **Only Obsidian's accepted formats are indexed** (research §2 —
    notes, images, audio/video, PDF, `.canvas`, `.base`); nothing else can be
    named by a `[[…]]`, so indexing it buys nothing and costs an alias read
    each. `AssetKind::classify` returns `Option`, which makes that list the one
    place the formats are written down. *Rejected:* a hardcoded
    `target/`/`node_modules/` blocklist — the heuristic guess about which
    directories "don't count" that the previous revision of this bullet rightly
    refused. An ignore file is not a guess; it is already in the tree, and the
    user wrote it.
  - **Accepted consequences.** A document opened from outside the pinned root
    resolves only against that root, so a bare `[[x]]` in it may come out
    `Unresolved` — correct, not a bug: it is not part of the collection you
    opened. It still renders, and its same-file `[[#Heading]]` / `[[#^id]]`
    references still work, because those never consult the index. stdin keeps
    [D9](../stdin/README.md#d9-stdin-streaming-m3)'s `<cwd>/stdin.md` sentinel, so a pipe roots at the CWD as before. The
    `.git/` fallback can still root at a large repo, but the filters above mean
    the *index* is the size of the notes in it, not of the checkout.
  - **Off the main loop.** The walk is the one piece of per-load work whose cost
    is set by the tree rather than by the document, so it is the one piece that
    must not run on the UI thread: a vault behind a slow mount would otherwise
    stall every `:open` for as long as the filesystem took to answer. The shell
    runs `scan` + `VaultIndex::build` on `gio::spawn_blocking` and swaps the
    result in when it lands; that split is exactly what `VaultIndex::build`
    taking scanned entries was already for. Landing is quiet — the index is
    compared against the one in hand (`PartialEq`) and an identical one, which
    is the overwhelmingly common case, re-renders nothing; a changed one
    re-renders only if the document contains a `[[` at all. Renders never wait:
    launch starts with an empty index and the scan overlaps window creation and
    WebKit startup, so it lands well before the first load finishes. Because the
    only blocking constructor left is test-only, `Vault::rooted` is `#[cfg(test)]`
    — the type system now enforces that the reader cannot block on a walk.
    `GetState` reports `vault_files`, which is how the e2e suite waits for a
    scan to land instead of sleeping, and the first thing to look at when a
    `[[…]]` will not resolve: it says whether the vault jumanji found is the one
    you meant.
  - **Freshness:** rescanned on every document load and on `r`, reused by every
    live-reload re-render (editing a note cannot rename another one). No
    vault-wide watcher, no cache layer. Measured on this repo (dev build, warm
    cache): the previous revision walked 22 966 files in ~23 ms and spent ~126 ms
    building the tables — the ~150 ms this bullet used to quote, and mostly the
    *build*, not the walk. With ignore files and the format allowlist it is 16
    files, ~1.4 ms total, and off-thread besides. The depth (32) and file
    (50 000) caps stay: they now bound a pathological tree rather than an
    ordinary repo.
  - `aliases` is read by a ~40-line targeted parser, not a YAML crate: one key
    in three documented shapes (`aliases: x`, `[a, b]`, a `- ` block), and a
    malformed value degrades to "no aliases". The walk is local filesystem I/O
    inside the core on the `core::fence` ([D6.2](../architecture/README.md#d6-extensibility--pipeline-seams-not-a-plugin-abi)) precedent — `Result`-shaped, no
    display, injectable, so `VaultIndex::build` takes scanned entries and
    resolution is unit-tested without a fixture tree.
- **Wikilinks: comrak's parser, our resolution.** `wikilinks_title_after_pipe`
  is the only correct switch (Obsidian is url-first). An AST pass over
  `NodeValue::WikiLink` **percent-decodes** `NodeWikiLink.url` (comrak stores it
  `clean_url`-encoded, so `#^id` arrives as `#%5Eid`), parses it into `WikiRef`,
  resolves, and rewrites the node. Heading fragments are translated to slugs
  with the *same* `comrak::Anchorizer` `core::toc` uses — no target document is
  read, because the naive slug is also the right answer for Obsidian's
  "duplicate headings resolve to the first" rule (the first occurrence is the
  one that gets the unsuffixed slug). It is wrong only when two *differently
  spelled* headings slugify identically; noted, not chased.
  - **Emitted as three nodes, not one:** `HtmlInline("<a …>")`, a `Text` node
    holding the label, `HtmlInline("</a>")`. Folding the label into the raw HTML
    would hide it from `comrak::html::collect_text`, which both `toc::extract`
    and comrak's own heading-id renderer use — a heading containing a wikilink
    would silently get a truncated anchor. The `Text` node keeps TOC, emitted
    id, and Obsidian's own slug in agreement.
  - **Label:** the alias when given; otherwise the note name with fragment
    components joined by ` > ` (`Note > Heading`), Obsidian's display, not
    comrak's raw-target default. Aliases are rendered as plain text — Obsidian
    does not parse markdown inside link text.
  - **Unresolved links carry no `href`:** `<a class="internal-link
    is-unresolved" data-href="<raw>" title="unresolved: <raw>">label</a>`. No
    href means not clickable, not focusable, and invisible to the `f` hint
    overlay (which selects `a[href]`) — dead by construction rather than by a
    guard in the router, and never note creation (jumanji is a reader). The
    native tooltip is the feedback channel. *Rejected:* a `jumanji-unresolved:`
    pseudo-scheme routed to a statusbar notice — it invents a URI scheme and
    fills the hint list with dead entries.
- **Embeds `![[…]]` are an inline-`Text` pass, because comrak never sees them.**
  `!` consumes the following `[` as an image bracket, so the wikilink parser
  never fires and the construct survives as literal text nodes. The pass joins
  each block's consecutive `Text` siblings, scans that run, and splices the
  match — fence- and inline-code-safe by construction (those are `CodeBlock` /
  `Code` nodes, never `Text`). *Rejected:* a pre-pass over the source, which
  would have to re-implement fence and code-span lexing to know where not to
  look. The same scanner also carries `%%comments%%` and `^block-ids` (below):
  one scanner, a small `enum Construct`, separate handlers — the text-run
  splicing is the tricky part and is written once.
  - Image targets → `<img class="internal-embed" src="file://…">` with the
    `|W`/`|WxH` dimensions as attributes, capped by `max-width:100%` so a huge
    declared width scrolls nothing ([D5a](../reading/README.md#d5a-two-axis-zoom)). The CSP already permits `img-src
    file:`.
  - **Note/heading/block transclusion is deferred** — it needs recursive
    rendering with cycle detection — and degrades to an honest **link-card**:
    `<a class="internal-embed embed-card" data-embed="note" …>` showing the
    target's display name. A real link, so click, `f` hints and the jumplist all
    work, and it visibly says "a door, not the content". PDF/audio/video/canvas
    embeds get the same card (`data-embed="pdf"`, …) and open via the system
    handler; `#page=`/`#height=` are parsed and ignored.
- **Callouts: one pass, all 27 spellings, `<details>` for folding.** A pass
  over `BlockQuote` nodes matches `[!type]` + optional `+`/`-` + optional title
  on the first line, drops that line, and wraps the quote the way `wrap_tables`
  does (`HtmlBlock` siblings, but carrying the blockquote's source line so [D7](../editor-sync/README.md#d7-editor-pairing--the-synctex-analogue-built)
  reverse click still lands). A fold marker emits `<details class="callout"
  …><summary class="callout-title">`, open for `+`, closed for `-`; **no marker
  emits a plain `<div>`** — Obsidian shows no disclosure affordance there, and
  `<details open>` would invent one. Folding therefore costs no JavaScript ([D3](../architecture/README.md#d3-content-pipeline--100-rust-no-javascript)).
  Emission is `data-callout="<literal lowercased type>"` (Obsidian's own, so
  themes keying off it work) *plus* `class="callout callout-<canonical>"`,
  keeping the 27 → 13 alias table in typed Rust rather than a CSS selector list.
  An unknown type keeps its literal `data-callout` and falls back to
  `callout-note`, matching Obsidian. Nesting falls out of the recursive pass.
  Per-type icons are deferred; the existing `.markdown-alert` colour language is
  reused.
- **Comments and block ids.** Inline `%%…%%` is deleted by the text-run scanner;
  the block form (`%%` alone on a line) is matched between *sibling* blocks and
  the span removed — a comment region straddling a list boundary is not handled,
  which is the honest limit of an AST-level treatment. A trailing `^block-id` is
  stripped from display and replaced by `<span class="block-anchor"
  id="^37066d"></span>` inserted **before** the owning block (a standalone `^id`
  paragraph attaches to its preceding sibling), so `[[Note#^37066d]]` has
  somewhere to land. The `^` is kept in the id: heading slugs never contain one,
  so the two anchor namespaces cannot collide. The shell must **percent-decode
  the fragment** before `getElementById` — WebKit hands back `%5E`.
- **Pass order** (extends [D3](../architecture/README.md#d3-content-pipeline--100-rust-no-javascript)/D6.2): strip comments → callouts →
  `fence` → `mermaid` → `highlight` → `math` → embeds → wikilinks → block ids →
  task markers → `wrap_tables` → `annotate_html_block_lines` → `toc::extract` →
  format. Comments go first so a commented-out fence never runs a renderer.
  Every new pass either preserves the node's `.sourcepos` (block passes, which
  `annotate_html_block_lines` then picks up) or works inside a block whose own
  `data-sourcepos` is untouched (inline passes) — [D7](../editor-sync/README.md#d7-editor-pairing--the-synctex-analogue-built) holds. Inline sourcepos is
  lost on the rewritten inlines themselves; reverse click walks up to the
  enclosing block, so this is invisible.
- **Routing ([D10](../jumplist/README.md#d10-cross-document-jumplist-navigation-post-10), extended).** `pipeline::render` takes the `Vault` as a third
  argument (`render(md, opts, vault)`) — it is per-document state, not
  config-derived render options. A resolved wikilink is an ordinary `file://`
  link, so `open_uri` → `open_file` handles it with the jumplist push [D10](../jumplist/README.md#d10-cross-document-jumplist-navigation-post-10)
  already gives `.md` links, and `f` hints pick it up with no change. One real
  gap this **supersedes**: `open_uri` today drops the fragment of a
  *cross-document* link (`other.md#section` opens the file and ignores the
  anchor) — it only honours a fragment when the base is the current document.
  D11 adds a `pending_anchor: Option<String>` applied in the load-finished
  handler exactly as `pending_forward` is, overriding the restored history
  scroll. This fixes plain markdown fragment links too.

Types, sketched (illegal states unrepresentable; "unresolved" is a variant, not
a dangling href):

```rust
// core::obsidian — a parsed, percent-decoded `[[…]]` / `![[…]]`.
pub struct WikiRef { note: Option<String>,   // None => same file, `[[#H]]`
                     fragment: Option<Fragment>,
                     pipe: Option<String>,   // alias | embed dimensions
                     kind: RefKind }         // Link | Embed
pub enum Fragment { Heading(Vec<String>), Block(BlockId) }  // `#A#B` | `#^id`

// core::vault — resolution.
pub enum Vault { Loose { dir: PathBuf }, Indexed(VaultIndex) }
pub enum Target { Note { path: PathBuf, anchor: Option<String> },  // slugified
                  Asset { path: PathBuf, kind: AssetKind },  // Image|Pdf|Av|…
                  Unresolved }
impl Vault { pub fn resolve(&self, r: &WikiRef, source: &Path) -> Target }
```

**Amendment (settled during implementation).** Three points the sketch above
left underspecified:

- **`Vault` binds the document path**, so `render(md, opts, vault)` stays
  three-argument. The sketch's `Vault::resolve(&self, r, source)` and a
  three-argument `render` cannot both hold — `render` would need a fourth
  argument for the source. Since the `Vault` is already per-document state,
  it carries the source: `struct Vault { source: PathBuf, index: VaultIndex }`.
  With the root fixed at the CWD there is exactly one state, so there is no
  `VaultKind` — a variant nothing constructs is a Tar Pit invitation.
  `VaultIndex::resolve(&self, r, source)` keeps the sketch's signature at the
  layer that must be unit-testable without a fixture tree; `Vault::resolve(&self,
  r)` delegates to it, including the `note: None` same-file case, so that rule
  lives in one place. `Vault::rooted` takes the root as an argument rather than
  reading `current_dir()` itself — the core stays free of ambient state, and a
  test can root a vault anywhere.
- **Callout titles are plain text**, and only the parts of them comrak
  reports as text survive. `> [!tip] **Bold** title` titles as `Bold title`
  (emphasis markers dropped, text kept), but a construct comrak turns into a
  node with no text of its own — raw inline HTML, and a wikilink, which this
  pass runs before — is dropped **entirely, text and all**: `> [!note] a <c> b`
  titles as `a  b`. Obsidian does parse markdown in a title; matching it would
  mean re-parsing the title fragment as inlines and re-hosting them inside a
  raw-HTML wrapper, which the `HtmlBlock` sandwich shape cannot express.
  Accepted limitation — a callout title is a label, not prose.
- **Non-`x` task markers.** `relaxed_tasklist_matching` makes comrak *parse*
  `- [-]`/`- [?]`, but `render_task_item` emits `checked` for any non-empty
  symbol — so they render as done, which is wrong (Obsidian shows the
  character). A `mark_task_symbols` pass sets `symbol = None` (honest: not
  done) and prepends `<span class="task-marker">?</span>` to the item's first
  paragraph. `x`/`X` are left alone.

**Deliberately out of scope (D11):** transclusion (link-card instead), `#tag` pills
(no search index behind them, so a pill is decoration), Templater, Bases/`.base`, Canvas
rendering, Publish-only properties, and the `internal-link` mermaid node class.
Dataview/dataviewjs/tasks/query fences need no work — `highlight_code_blocks`
already falls back to plain-text syntect for unknown languages, so they render
as code blocks today.
