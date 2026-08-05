# Research: Obsidian's markdown dialect

*Web research conducted 2026-08-05. Primary sources: the Obsidian help vault's
**raw markdown** (served by `publish-01.obsidian.md`, which is the same content
`help.obsidian.md` renders), `docs.obsidian.md` for API semantics,
`forum.obsidian.md` where the help pages are silent. Section 5 is **measured**,
not cited: comrak 0.53.0 source plus a scratch binary run against it.*

Obsidian markets itself as "CommonMark + GitHub Flavored Markdown + LaTeX"
([OFM](https://obsidian.md/help/obsidian-flavored-markdown)). That is true of
the *base*; the interesting part is the extension set on top, which is small,
sharply defined, and — for links — has resolution semantics that no
file-at-a-time reader implements by accident.

One global caveat from the same page, worth internalizing early: **"Obsidian
does not render Markdown syntax inside HTML elements."** `**bold**` inside a
`<div>` stays literal. That is stricter than comrak with `render.unsafe`.

## 1. Internal links

### 1.1 The surface forms

Obsidian accepts two link formats, and calls them equivalent
([Internal links](https://obsidian.md/help/links)):

- Wikilink: `[[Three laws of motion]]` or `[[Three laws of motion.md]]`
- Markdown: `[Three laws of motion](Three%20laws%20of%20motion)` or `…(….md)`

The `.md` extension is **optional** on markdown notes and **mandatory** on
everything else: "links to file formats other than Markdown needs to include a
file extension, such as `[[Figure 1.png]]`". Markdown-format destinations must
be percent-encoded; wikilink destinations must not be — spaces are literal
inside `[[ ]]`.

Folder paths are allowed and are **vault-root-relative with forward slashes,
even on Windows**: `[[Projects/Three laws of motion]]`.

The full grammar of the target, in the order the pieces appear:

| Form | Meaning |
|---|---|
| `[[Note]]` | link to a note |
| `[[folder/Note]]` | vault-root-relative path |
| `[[Note\|Display text]]` | custom display text |
| `[[Note#Heading]]` | anchor link to a heading |
| `[[Note#Heading#Subheading]]` | nested heading path — repeated `#`, **not** a literal `#` in the heading |
| `[[#Heading]]` | heading in the *same* file |
| `[[Note#^block-id]]` | block reference |
| `[[Note#Heading\|alias]]` | fragment **and** alias, in that order |

Two more forms exist but are **editor autocomplete affordances, not link
targets**: `[[## team]]` searches headings vault-wide and `[[^^block]]` searches
blocks. They are typed to *summon a picker*; what lands in the file is an
ordinary `[[Note#Heading]]`. A reader can and should ignore them.

Obsidian names the characters that break a link target:

> A string which contains the following characters may not work as a link:
> `# | ^ : %% [[ ]]`

`#`, `|` and `^` are structural (they *are* the grammar); `:` and `%%` are
banned for filesystem and comment reasons. Inside table cells the pipe must be
backslash-escaped — `[[Basic formatting syntax\|Markdown syntax]]`
([Advanced formatting](https://obsidian.md/help/advanced-syntax)).

### 1.2 Resolution — the part that is genuinely surprising

This is where a naive "treat the target as a relative path" implementation gets
it wrong, and where most third-party renderers diverge from Obsidian.

**Resolution is vault-wide by filename, not path-relative.** The API surface is
`getFirstLinkpathDest(linkpath: string, sourcePath: string): TFile | null`,
documented only as "Get the best match for a linkpath"
([API](https://docs.obsidian.md/Reference/TypeScript+API/MetadataCache/getFirstLinkpathDest)).
The behavior behind it, per the Obsidian team on the forum:

> If the file name is unique, then it's just the filename. If it's not unique,
> then it's the absolute path from the vault root.
> ([forum](https://forum.obsidian.md/t/settings-new-link-format-what-is-shortest-path-when-possible/6748))

**The vault root outranks the sibling folder.** Given `A.md` at the root and
`Folder/A.md`, a bare `[[A]]` written *inside* `Folder/B.md` resolves to the
**root** `A.md`. A moderator confirmed this is deliberate:

> This is not a bug. it's intentional. Otherwise, where `[[A]]` points to
> depends on which file it is contained. We don't want that. We want `[[A]]` to
> point to the same note across the vault.
> ([forum](https://forum.obsidian.md/t/absolute-link-path-has-higher-precedence-than-relative-path/69542))

So `sourcePath` is a tiebreaker, not the base of a path join. `[[A]]` means the
same note everywhere in the vault; that context-independence is the design
goal.

**"New link format" is a *generation* setting, not a resolution setting.** The
three options — *shortest path when possible* / *relative path to file* /
*absolute path in vault* — only decide what Obsidian writes into the file when
*you* insert a link. The resolver accepts all three shapes regardless. A reader
implements the resolver and can ignore the setting entirely; but it means a
real vault contains a *mixture* of bare names, partial paths and full paths,
and all of them must resolve.

**Matching is case-insensitive.** `[[note]]` finds `Note.md`
([forum](https://forum.obsidian.md/t/case-sensitivity/52331)). Obsidian is
inconsistent about files that differ only in case, which is a vault-hygiene
problem rather than something a reader can fix.

**Aliases participate in resolution.** The `aliases` frontmatter property
creates "alternative names for notes", and the help page explicitly frames it
as a link target: if you regularly write `[[Three laws of motion]]`, adding
"3 laws" as an alias lets you write `[[The 3 laws]]` instead. Resolving links
therefore requires reading every note's frontmatter, not just its filename.

**Unresolved links are rendered, not dropped.** Obsidian emits
`<a class="internal-link is-unresolved" data-href="…">` and clicking creates
the note ([forum](https://forum.obsidian.md/t/how-can-i-make-unresolved-link-display-differently/45171)).
`data-href` carries the raw linkpath, `href` the resolved one. For a
read-only reader the useful half is: keep the text, style it as dead, don't
navigate.

### 1.3 Heading anchors

The fragment after `#` is **the literal heading text**, not a slug. `[[Help and
support#Questions and advice#Report bugs and request features]]` is matched
against heading strings. Duplicate headings resolve to the first
([forum](https://forum.obsidian.md/t/with-2-headings-of-same-name-in-file-can-only-link-to-first-one/74574)).
This matters: comrak/GitHub-style anchors are slugified
(`#questions-and-advice`), so a renderer must translate heading-text →
slug at link-rewrite time rather than passing the fragment through.

### 1.4 Block identifiers

A block is "a unit of text in your note, such as a paragraph, block quote, or
list item". Three placement rules, all from the Internal links page:

- **Simple paragraphs** — blank space then `^id` at end of the line:
  `…a happier place. ^37066d`
- **Structured blocks** (lists, quotations, callouts, tables) — the identifier
  goes on **its own line, with a blank line before and after**.
- **Specific lines within a list** — the identifier can sit directly on the
  bullet.

Charset: "Block identifiers can only consist of Latin letters, numbers, and
dashes." Auto-generated ones are six lowercase alphanumerics (`^37066d`,
`^b15695`); human ones look like `^quote-of-the-day`.

Two explicit limits worth quoting:

> We do not support links to specific parts of quotations, callouts, and tables.

> Block references are specific to Obsidian and not part of the standard
> Markdown format. Links containing block references won't work outside of
> Obsidian.

## 2. Embeds / transclusion

Prefix any internal link with `!`
([Embed files](https://obsidian.md/help/embeds)). Everything in §1 applies to
the target; the pipe changes meaning from *alias* to *dimensions* for media.

| Syntax | Result |
|---|---|
| `![[Internal links]]` | whole note, inline, live |
| `![[Internal links#Heading]]` | one section |
| `![[Internal links#^b15695]]` | one block |
| `![[Engelbart.jpg]]` | image |
| `![[Engelbart.jpg\|100x145]]` | width×height |
| `![[Engelbart.jpg\|100]]` | width only, aspect preserved |
| `![[Excerpt….ogg]]` | audio player |
| `![[Document.pdf]]` | PDF viewer |
| `![[Document.pdf#page=3]]` | PDF opened at page 3 |
| `![[Document.pdf#height=400]]` | PDF viewer height in px |
| `![[My canvas.canvas]]` | canvas (shapes only, "not the text inside cards") |
| `![[My note#^my-list-id]]` | a list, via a block id |

The `|dimensions` convention leaks into the **markdown** form too, where it
rides in the *alt text*: `![Engelbart|100x145](https://…/Engelbart.jpg)`
([Basic formatting](https://obsidian.md/help/syntax)). That is Obsidian-only —
everywhere else that is just an alt string.

Accepted file formats
([help](https://help.obsidian.md/file-formats)): `.md`, `.base`, `.canvas`;
images `.avif .bmp .gif .jpeg .jpg .png .svg .webp`; audio `.flac .m4a .mp3
.ogg .wav .webm .3gp`; video `.mkv .mov .mp4 .ogv .webm`; `.pdf`.

Missing targets degrade the same way unresolved links do — a dead placeholder,
not an error.

## 3. Callouts — and how far they are from GitHub alerts

Syntax ([Callouts](https://obsidian.md/help/callouts)): a blockquote whose
first line is `> [!type]`, optionally followed by a fold marker and a title.

```md
> [!tip] Custom title
> Body text, which may contain **markdown**, [[wikilinks]] and ![[embeds]].

> [!faq]- Collapsed by default
> body

> [!note]+ Expanded by default
> body

> [!note]
> > [!tip]
> > nested, via stacked `>`
```

Type identifiers are **case-insensitive**; a title-only callout (no body) is
legal; the default title is the type in title case; **unknown types fall back
to the `note` style** rather than failing. Custom types are pure CSS —
Obsidian emits `data-callout="<type>"` and themes key off it with
`--callout-color` / `--callout-icon`.

The built-in set:

| Type | Aliases |
|---|---|
| `note` | — |
| `abstract` | `summary`, `tldr` |
| `info` | — |
| `todo` | — |
| `tip` | `hint`, `important` |
| `success` | `check`, `done` |
| `question` | `help`, `faq` |
| `warning` | `caution`, `attention` |
| `failure` | `fail`, `missing` |
| `danger` | `error` |
| `bug` | — |
| `example` | — |
| `quote` | `cite` |

**Divergence from comrak's `alerts` extension** (measured, §5): comrak
recognizes exactly five types — `note`, `tip`, `important`, `warning`,
`caution` — matching GitHub. It *does* support custom titles (`> [!note] My
title` → `<p class="markdown-alert-title">My title</p>`), which is better than
the GitHub spec suggests, and it nests correctly. But:

- **22 of Obsidian's 27 spellings are unrecognized.** `> [!question]` renders
  as a plain blockquote containing the literal text `[!question]`. That is a
  visible defect, not graceful degradation.
- **Fold markers leak into the title.** `> [!tip]- Foldable` yields the title
  string `"- Foldable"`; `> [!note]+ Expanded` yields `"+ Expanded"`.
- `important` collides: in Obsidian it is an *alias of `tip`*; in GitHub/comrak
  it is its own type with its own color. Cosmetic, but they will not match.

## 4. The rest of the dialect

All core (no plugin) unless marked. From
[Basic formatting](https://obsidian.md/help/syntax),
[Advanced formatting](https://obsidian.md/help/advanced-syntax),
[Tags](https://obsidian.md/help/tags), [Properties](https://obsidian.md/help/properties).

- **Highlight** — `==Highlighted text==`. Obsidian-only; not in CommonMark or
  GFM.
- **Comments** — `%%inline%%` and a block form where `%%` sits alone on lines
  around multi-line content. "Comments are only visible in Editing view" — a
  reader must **delete** them, not render them.
- **Footnotes** — reference `[^1]` / `[^1]: text`, named `[^note]`, and
  **inline `^[This is an inline footnote.]`** with the caret *outside* the
  brackets. The inline form is reading-view-only, so a reader must handle it.
- **Math** — `$inline$` and `$$block$$`, LaTeX via **MathJax** (not KaTeX).
- **Mermaid** — ```` ```mermaid ```` fences. Obsidian additionally lets diagram
  nodes carry the `internal-link` class (`class Biology,Chemistry
  internal-link;`) to make them clickable wikilinks; "Internal links from
  diagrams don't show up in the Graph view."
- **Task lists** — `- [ ]` / `- [x]`, plus: "You can use any character inside
  the brackets to mark it as complete", with `- [?]` and `- [-]` given as
  examples. Comrak accepts only `[ ]` and `[x]`; `- [-] c` renders as the
  literal text `[-] c`.
- **Tags** — `#tag`; letters, numbers, `_`, `-`, `/` for nesting (`#inbox/to-read`),
  plus "commonly accepted Unicode characters, including emojis". **Must contain
  at least one non-numeric character** — `#1984` is not a tag, `#y1984` is.
  Matching is case-insensitive.
- **Frontmatter properties** — YAML between `---` fences. Rendering-relevant
  keys: `aliases` (list — participates in link resolution, §1.2), `tags`
  (list), `cssclasses` (list — per-note styling hooks). `publish`, `permalink`,
  `description`, `image`, `cover` are Obsidian Publish concerns. Internal links
  inside properties "must be surrounded with quotes".
- **Tables** — GFM, with the pipe-escaping caveat from §1.1.
- **Strict line breaks** — an editor setting that switches Obsidian between
  "single newline is a `<br>`" (default) and CommonMark soft-wrap. Files
  authored under the default may rely on newline-per-line.

**Plugin territory — out of scope, but must degrade cleanly.** Dataview
(```` ```dataview ````, ```` ```dataviewjs ````), Tasks (```` ```tasks ````),
Templater (`<% … %>`). Note that ```` ```query ```` is **core**, not a plugin —
it embeds search results. None of these can be evaluated without a vault index
and a JS runtime; all four should fall through to a syntax-highlighted code
block.

## 5. What comrak actually does — measured against 0.53.0

jumanji already runs comrak with `strikethrough`, `table`, `autolink`,
`tasklist`, `footnotes`, `math_dollars`, `math_code`, `alerts`, sourcepos and
`header_id_prefix = Some("")` (`src/core/pipeline.rs:150-172`). Wikilinks are
*not* enabled. Turning on `wikilinks_title_after_pipe` gets less than it looks
like.

The parser (`src/parser/inlines.rs:883-1000`) is deliberately minimal: it reads
one component, optionally one `|`, one more component, then requires `]]`. It
produces `NodeValue::WikiLink(NodeWikiLink { url: String })` — a **single URL
string and nothing else** (`src/nodes.rs:352`) — with the label as child
inlines. HTML output is `<a href="…" data-wikilink="true">label</a>`.

Verified outputs:

| Input | comrak 0.53.0 output |
|---|---|
| `[[Note]]` | `<a href="Note" data-wikilink="true">Note</a>` |
| `[[Note\|alias]]` | `<a href="Note" …>alias</a>` |
| `[[Note#Heading]]` | `<a href="Note#Heading" …>Note#Heading</a>` |
| `[[Note#^block-id]]` | `<a href="Note#%5Eblock-id" …>Note#^block-id</a>` |
| `[[#Heading]]` | `<a href="#Heading" …>#Heading</a>` |
| `[[folder/Note]]` | `<a href="folder/Note" …>folder/Note</a>` |
| `[[Note name]]` | `href="Note%20name"` (percent-encoded) |
| `[[Note#Heading\|a\|b]]` | **literal text** — parse fails on the second pipe |
| `![[Note]]`, `![[img.png\|300]]` | **literal text** — not parsed at all |
| `[[]]` | `<a href="" data-wikilink="true"></a>` |
| `\| [[Note\\\|alias]] \|` in a table | parses correctly; the escaped pipe survives |

The divergences that matter:

1. **No embed syntax whatsoever.** `![[…]]` is not implemented. Tracing the
   `!` branch (`inlines.rs:317`) explains why: `!` consumes the following `[`
   to open an image bracket, so the wikilink branch never sees `[[`. Embeds
   must be handled before comrak sees the text, or by patching the AST.
2. **The fragment is not split out.** `#Heading` and `#^block` ride along
   inside `url`, and `clean_url` percent-encodes `^` to `%5E`. Any resolution
   logic has to re-parse the URL string itself.
3. **The default label is wrong.** With no pipe, comrak uses the raw target, so
   `[[Note#Heading]]` displays as `Note#Heading`. Obsidian displays
   `Note > Heading`.
4. **One pipe only.** A second pipe aborts the parse and the whole construct
   falls back to literal text — loud rather than silent, at least.
5. **`wikilinks_title_before_pipe` is the wrong dialect** — `[[label|url]]`.
   Obsidian is url-first, so `wikilinks_title_after_pipe` is the only
   correct switch.
6. **No resolution, by design.** comrak emits the raw target as an href. All of
   §1.2 — vault index, root-beats-sibling, case folding, alias lookup,
   heading-text → slug translation — is the consumer's job.

Also unsupported by comrak and needing separate handling: `==highlight==`,
`%%comments%%` (rendered literally today), inline footnotes `^[…]`, block
identifiers `^id` (rendered as literal text at the end of the paragraph — a
visible artifact in any vault that uses block refs), non-`x` task markers, and
tags (harmless: `#tag` stays plain text).

## 6. Implications for jumanji

**Must implement for a vault to "just work":**

1. **A vault index.** Everything hinges on it: filename → path map (case-folded),
   plus each note's `aliases`. Resolution is vault-wide, so the reader needs a
   vault root — discovered by walking up for `.obsidian/`, with the file's own
   directory as the degenerate fallback. This is the single largest new concept;
   it belongs in core as a pure `VaultIndex` with a `resolve(linkpath,
   source) -> Resolution` function, testable without a display.
2. **A real wikilink target type.** `NodeWikiLink { url: String }` is too weak.
   Parse into an ADT in core — path, optional fragment (`Heading(Vec<String>)` |
   `Block(BlockId)` | `PdfPage(u32)` | `None`), optional display/dimensions —
   and make the "unresolved" state an explicit variant rather than a dangling
   href. This is exactly the "illegal states unrepresentable" case the project
   conventions call for.
3. **Heading-text → slug translation.** `[[Note#Getting Started]]` must become
   `#getting-started` to match what `src/core/toc.rs` and comrak's anchorizer
   already produce. Without this, every anchor link in a vault silently fails.
4. **Embeds, done outside comrak.** Since `![[…]]` never reaches the wikilink
   parser, this needs a pre-pass or an AST patch. Note embeds imply recursive
   rendering with cycle detection; image embeds with `|100x145` are the common
   case and much cheaper — worth shipping images first.
5. **The full callout set.** comrak's five GitHub alerts leave 13 Obsidian
   spellings rendering as visible `[!question]` garbage, and mangle fold
   markers into titles. A callout pass in core covering all 27 spellings,
   `+`/`-`, and title-only forms is a small, well-bounded piece of work with a
   large visual payoff. Emitting `data-callout="<type>"` buys theme
   compatibility for free.
6. **Strip `%%comments%%` and paragraph-trailing `^block-ids`.** Both currently
   render as literal text. Both are one-line wins.
7. **`==highlight==` → `<mark>`.** Trivial, and common in real vaults.

**Degrade gracefully, don't chase:**

- PDF/audio/video/canvas embeds — render a labeled link-card, not a viewer.
  `#page=` / `#height=` can be parsed and ignored.
- Non-`x` task markers — render the character rather than dropping the item.
- `internal-link`-classed mermaid nodes — merman won't know about them; the
  diagram should still draw.
- Dataview / dataviewjs / tasks / query fences — highlighted code blocks.
  Cheap, honest, and per the project's "broken fence never crashes" rule.
- Unresolved links and missing embeds — visible but inert, mirroring
  `is-unresolved`. Never a blank page.

**Explicitly out of scope:** Templater `<% %>`, Bases (`.base`), Publish-only
properties, and anything requiring evaluation rather than rendering. jumanji is
a reader; a vault index is the most state it should ever hold.

**Zathura-semantics note:** a vault turns jumanji from a one-file viewer into a
navigable graph, which makes `Ctrl-O`/`Ctrl-I` jumplist and `f`-hint link
following (`src/core/jumplist.rs`, `src/core/keymap.rs`) far more valuable than
they are for a single document. Wikilink following should push the jumplist
exactly as anchor links do.

## Sources

- https://obsidian.md/help/links · https://obsidian.md/help/embeds
- https://obsidian.md/help/callouts · https://obsidian.md/help/syntax
- https://obsidian.md/help/advanced-syntax · https://obsidian.md/help/obsidian-flavored-markdown
- https://obsidian.md/help/tags · https://obsidian.md/help/properties
- https://help.obsidian.md/file-formats
- https://publish-01.obsidian.md/access/f786db9fac45774fa4f0d8112e232d67/ (raw help vault)
- https://docs.obsidian.md/Reference/TypeScript+API/MetadataCache/getFirstLinkpathDest
- https://forum.obsidian.md/t/settings-new-link-format-what-is-shortest-path-when-possible/6748
- https://forum.obsidian.md/t/absolute-link-path-has-higher-precedence-than-relative-path/69542
- https://forum.obsidian.md/t/case-sensitivity/52331
- https://forum.obsidian.md/t/with-2-headings-of-same-name-in-file-can-only-link-to-first-one/74574
- https://forum.obsidian.md/t/how-can-i-make-unresolved-link-display-differently/45171
- comrak 0.53.0 source: `src/parser/inlines.rs`, `src/nodes.rs`,
  `src/parser/options.rs`, `src/tests/fixtures/wikilinks_title_after_pipe.md`
