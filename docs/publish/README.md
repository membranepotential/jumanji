# Publishing a vault as a website

`jumanji-publish` renders a folder of markdown notes (a vault) into a static
website: one HTML page per note, styled like the reader, with wikilinks,
embeds, diagrams and math working as they do in jumanji. The site is hosted on
Cloudflare behind a shared password, so only people the owner gives the
password to can read it, and no crawler can index it.

First site: the thesis wiki, `~/Documents/Sandeep/thesis/wiki` (about 20
notes; relative markdown links, SVG figures, heavy math, some mermaid).

Status: **implemented** (2026-10-28): `jumanji-publish`, the password
Worker and the deploy script. Not yet deployed; the owner runs the one-time
setup below. Details that the build settled are in
[As built](#as-built-2026-10-28).

Decisions on this page:

- [D15: Publishing — a static site from the core pipeline, served behind a password (2026-10-28)](#d15-publishing--a-static-site-from-the-core-pipeline-served-behind-a-password-2026-10-28)

## D15: Publishing — a static site from the core pipeline, served behind a password (2026-10-28)

### What the owner asked for

- Publish a collection of markdown documents plus their images as an
  interlinked wiki that looks like jumanji.
- The site must not be crawled. Access control is the accepted way to get
  there.
- A reader gets in with a password the owner hands them.
- URLs stay close to the file names. Their exact shape does not matter.

### Decision

**A static site generator built on `core::pipeline::render`.** Each note is
rendered by the same function the reader uses, so the site and the reader
cannot drift apart: a fix to callouts, mermaid or math reaches both. The output
is plain HTML and CSS. Reading a page needs no JavaScript (D3 holds on the
web too), so a server only ever serves files.

**Hosting: Cloudflare Workers Static Assets on the `workers.dev` hostname,
with a small Worker script in front that asks for a password (HTTP Basic
Auth).**

- `run_worker_first: true` makes the script see every request before a file
  is served. Without the right `Authorization` header it answers `401` with
  `WWW-Authenticate: Basic`, and the browser shows its own login dialog. With
  it, the script passes the request to the `ASSETS` binding.
- The password is a Worker secret (`wrangler secret put SITE_PASSWORD`),
  never in a file or in the repo. The script compares it with
  `crypto.subtle.timingSafeEqual`. The user name is ignored. Changing the
  password means putting a new secret; every reader then needs the new one.
- The script is about 20 lines of JavaScript that run on Cloudflare, not in
  the page. D3 (no JavaScript in the content pipeline) is unaffected.
- Cloudflare now points new static sites at Workers Static Assets. Pages is
  still supported.
- Static asset limits: 20,000 files per version and 25 MiB per file on the
  free plan. The thesis wiki is far below both.

**No crawling.** The password is the real guard: a crawler gets `401`, never
content. The script also adds `X-Robots-Tag: noindex, nofollow` to every
response, including the `401`, in case the password check is ever switched
off. No `robots.txt` and no `_headers` file: the script is the one place
response headers are set.

**Rejected:**

- *Cloudflare Access:* it has no shared-password login. Its closest method
  is a one-time PIN: the reader types their email address and receives a code.
  That is stronger (one login per person, revocable per person), but the owner
  asked for a password to hand over. If per-person access is wanted later,
  delete the script and switch Access on for `workers.dev` (a one-click
  toggle, Cloudflare changelog 2025-10-03). Nothing else changes.
- *Cloudflare Pages:* it runs scripts in front of static files too, but
  Cloudflare puts its new static-site work into Workers.
- *GitHub Pages:* it has no access control outside GitHub Enterprise, and
  its sites are public.
- *A server that renders on request:* that means running a process and
  trusting it with the vault. Static files behind a password give the same reading
  experience with nothing to operate.
- *An existing generator (Quartz, Obsidian Publish):* the site would no
  longer look like jumanji, and it would resolve links in its own way. D11
  already defines link resolution once.

### The vault root is the folder you name

`jumanji-publish DIR` roots the vault at `DIR` (`Vault::rooted`), not at
`vault::root_for`. The reader's rule climbs to the nearest `.obsidian/` or
`.git/`. For the thesis wiki that is `thesis/`, which would put the code,
the papers and the whole repository inside the vault. What you name is what
can be published, and nothing above it.

### Selection: what gets published

**One rule for notes and files: whatever the entry note reaches by links.**

- The entry note is `DIR/index.md`, else `DIR/README.md`. It becomes
  `index.html`. If neither exists, the publisher stops with an error.
- **Notes:** the entry note and every note reachable from it by following
  wikilinks, note embeds and relative markdown links, transitively. This is
  the walk the document graph already does (`graph::scan` per note), without
  its budget cut. A note nothing links to stays private, so files like the
  wiki's `CLAUDE.md` (agent instructions) or a draft are never published,
  and no opt-out list is needed.
- **Assets:** only files that a published note references, through `![[…]]`,
  `[[…]]` or a relative markdown link or image. An unreferenced attachment
  never leaves the machine.
- **Dead links:** a link to anything not published (outside `DIR`, missing,
  or a `file://` URL) renders like an unresolved wikilink: a dead link with
  no `href`. A markdown link's URL never appears in a page; a dead image
  becomes an inert link-card naming its alt text. (An unresolved `[[…]]`
  keeps the reader's markup, which repeats the written name in `title`.) The publisher prints every dead
  link with its source note and line, so a link that should work is noticed
  before deploying. Example: the thesis wiki's `index.md` links to
  `../papers/Matthaiou 2024 … .pdf`, which lies outside `wiki/` and will be
  reported.

*Rejected: everything the vault scan indexes, minus notes marked
`publish: false`.* That publishes by default, so forgetting the marker
on one note leaks it.

### URLs: the vault path, with `.md` replaced by `.html`

`Projects/My Note.md` → `/Projects/My%20Note.html`, and assets keep their path
(`attachments/fig 1.png` → `/attachments/fig%201.png`). Every href is
**relative** (`../Projects/My%20Note.html#heading`) and percent-encoded with
`obsidian::percent_encode`. The site therefore works at any URL prefix and
also from a local folder opened in a browser. Heading and block anchors are the
ids the pipeline already emits.

`index.html` at the root is the entry note. A topic folder's `index.md`
becomes that folder's `index.html`, so `/cdr/` works as a URL as well. A
`README.md` entry note is published as `README.html` (where links point) and
copied to `index.html`.

### Changes to core

Today, resolved links are spelled `file:///abs/path` in two places,
`wikilink::open_tag` and `textscan::embed_html`. Relative markdown links and
images are left as written, and `CSP` allows only `file:` images.

1. **`LinkStyle`**, a new enum in `core::pipeline::Options`:
   `File` (today's behaviour, the default) | `Site { root: PathBuf }`. One
   function, `LinkStyle::href(&self, target: &Path, from: &Path) -> String`,
   replaces the two direct `obsidian::file_uri` calls. `Site` returns a
   relative URL computed from `from` (`Vault::source()`, the document the
   vault is bound to). `pipeline::page_path` maps a note to its `.html`.
2. **Publish set.** A link to a file that is not published must come out
   dead. Rendering runs against a `VaultIndex` built only from the published
   notes and assets (`VaultIndex::build` already takes injected entries).
   Resolution then fails for everything else, and the existing unresolved
   path handles it, with no new branch.
3. **A markdown-link pass in site mode** (`core::sitelink`). It rewrites a
   relative `[x](a.md#h)` to `a.html#h`, and a relative image or link to an
   asset to its site URL. A target outside the vault, or one that was not
   published, becomes dead. `file://` links become dead as well. Published
   means "in the render-time index": `VaultIndex::file` looks a vault-relative
   path up, case-insensitively, and only as a file name (`a` does not find
   `a.md`). `sitelink::classify` reads a URL for both this pass and the
   walk, so the page and the dead-link report agree.
4. **The CSP depends on the link style.** In site mode, `img-src`, `media-src`
   and `font-src` allow `'self' data:` in place of `file:` (the bundled fonts
   are `'self'`). `object-src` is not needed: PDFs are linked, not embedded.
   `default-src 'none'` and the ban on scripts stay.
5. **Dark mode without a script.** The reader toggles `html.dark` from the
   shell. In site mode, the dark colours are also emitted under
   `@media (prefers-color-scheme: dark)`, so the site follows the system
   setting. Nothing is restated: the colour table is read out of
   `style.css`'s `html.dark` block, the dark syntax theme is emitted unscoped
   inside the query, and the light syntax theme sits under
   `@media not all and (prefers-color-scheme: dark)`. The configured
   `default-recolor` does not apply to a site.

Everything here is pure and unit-tested in core, as D2a requires.

### The publisher

`core::site`, new and pure:

- `plan(root, entries, read) -> Result<SitePlan, PlanError>`. It finds the
  entry note among the scanned `entries`, walks the links from it, and
  decides the published notes (with titles and backlinks), the referenced
  assets, the dead links (with source line) and the render-time
  `VaultIndex`. It reads notes only through the injected `read` function, as
  `graph::build` does, so the walk is unit-tested without files. Links come
  from `graph::references`, the AST walk `graph::scan` itself is now built
  on, so backlinks follow the document graph's link rules (D14).
- Navigation HTML around each page: a TOC sidebar from
  `RenderedDocument::toc`, a backlinks list at the end, the note's folder path
  as a breadcrumb, and a link to the index.

`src/bin/jumanji-publish.rs`, the imperative shell:

```sh
jumanji-publish ~/Documents/Sandeep/thesis/wiki -o ~/Documents/Sandeep/thesis/wiki-site
```

1. Load the reader's `config.toml` (`Config::load`) and themes
   (`config::load_themes`). The site then uses the same theme, fonts,
   `diagram-look` and fence renderers as the reader. Render options come from
   the config through `From<&config::Options>`, the same path the reader's
   session takes.
2. Scan the vault, build the `SitePlan`, render the pages in parallel (rayon,
   already a dependency) and copy the assets.
3. Print the dead links and a summary (notes, assets, bytes).
4. Write into a temporary sibling directory, then swap it with the output
   directory. The publisher refuses to replace a non-empty directory that
   lacks its marker file `.jumanji-site`, so a typo in `-o` cannot delete a
   real folder. It also refuses an output inside the vault or containing it,
   judged after resolving the output's `..` and symlinks.
   It writes an `.assetsignore` so wrangler does not upload the marker.

The publisher performs no network I/O. Deploying is a separate step, which
keeps the "no network access anywhere" rule intact.

It ships as a **second binary in the same crate**: it runs on the owner's
machine, where GTK and WebKit are installed anyway. Splitting the code into a
toolkit-free `jumanji-core` crate is deferred until publishing has to run in
CI.

### Deployment (one-time setup, then one command)

`deploy/cloudflare/` in this repo holds the password script (`worker.js`) and
a `wrangler.jsonc`: `main` points at the script, and the `assets` block names
the output directory (`target/site` in this repo), the `ASSETS` binding and
`run_worker_first: true`. The Worker is named `jumanji-site`; a second site
passes its own name to the deploy script. Wrangler runs through
`pnpm dlx wrangler@4.151.3`. The pinned version must be at least 7 days old
(the dependency seasoning rule), and at least 4.44.0 so preview URLs follow
the `workers.dev` setting. 4.151.3 was published on 2026-10-20 (checked with
`pnpm view wrangler time --json`); bump it in `scripts/publish-site.sh` and
in the steps below together.

Preview URLs serve the same Worker, so the password covers them too.

One-time steps for the owner:

1. Run `pnpm dlx wrangler@4.151.3 login`. A browser opens; log in to
   Cloudflare.
2. In `deploy/cloudflare/`, run `pnpm dlx wrangler@4.151.3 secret put
   SITE_PASSWORD`. It asks for the password without echoing it. If the Worker
   does not exist yet, wrangler offers to create it; accept.
3. Run `scripts/publish-site.sh ~/Documents/Sandeep/thesis/wiki`. It runs
   `jumanji-publish` and then `wrangler deploy`. The output prints the
   `https://<name>.<account>.workers.dev` URL.
4. Open the URL in a private window. The browser asks for a user name and
   password; any user name and the password from step 2 open the wiki.
5. Give the reader the URL and the password, ideally by different channels.

After that, publishing again is step 3 alone.

### Out of scope for the first version

- **Search across the site.** It needs JavaScript in the page and a prebuilt
  index. It comes next if the wiki grows big enough to need it.
- **Keyboard navigation** (`j`/`k`, `gg`/`G`, `f` hints). This could reuse the
  controller's scripts through a browser shim for `__jmnj_post`. It comes
  later, if at all.
- **The document graph on the web.** Its overlay is controller JavaScript.
  A static per-page "links" view is possible later.
- **Note transclusion.** `![[note]]` stays a link card, as in the reader
  (deferred there too).
- **Incremental builds.** Every publish renders the whole vault. D13's
  benchmarks will show whether that is slow.

## As built (2026-10-28)

What the build settled that the plan above left open:

- **Fonts.** A viewer's browser does not have the configured `font-body`,
  `font-mono` and `font-ui`, so the site bundles them. The publisher asks fontconfig
  (`fc-match`) for each face the stylesheet uses (`site::FACES`: regular,
  italic, bold, bold italic; `strong` is weight 680, which matches the bold
  face), accepts only an answer from the same family, slant and weight,
  copies each file once to `fonts/` and converts it to WOFF2 when
  `woff2_compress` is installed (else the original file is kept). Each page
  declares them with `@font-face`: a static face with its one weight, a
  variable font (found with `fc-scan`) with its weight range, e.g.
  `font-weight: 100 900`, so a 650 heading is drawn at 650 rather than
  matched to one face or synthesized. A family fontconfig does not know is
  skipped with a warning, and the page falls back to the stylesheet's stack.
  - `font-ui` (2026-10-28) names the interface font: headings, callout
    titles, table heads and the site's navigation, which the stylesheet sets
    in `var(--font-ui, system-ui, …)`. Unset, the reader keeps using the GTK
    font through `system-ui` and the site the viewer's system font; the
    publisher prints a one-line hint. Set it to the GTK font (Adwaita Sans
    here, a variable font: one file upright, one italic) and the site
    matches the reader.
  - Source Serif 4 and SauceCodePro Nerd Font Mono resolve to static files
    (the variable Source Serif is a separate family, "Source Serif 4
    Variable"), so they are declared one weight per file.
  - Mermaid labels keep merman's own stack (`"trebuchet ms", verdana, arial,
    sans-serif`), in the reader and on the site alike; `font-ui` does not
    touch them.
- **Navigation** (`site::page`, `site::site_css`, `core/assets/site.css`),
  spliced around the pipeline's output at `pipeline::MAIN_OPEN` /
  `MAIN_CLOSE`:
  - TOC sidebar: the page's headings, fixed in the left margin, shown only
    when the window is the column plus two sidebar widths (240 px each).
    From there on, wide blocks use the sidebar width as their gutter, so they
    never run under it. Below that width it is hidden. A page with fewer than
    two headings has none.
  - Breadcrumb: the entry note's title, then each folder above the note,
    linked by the title of the folder's own published `index.md` where there
    is one. The entry page has none.
  - Backlinks: "Linked from" at the end of the column; not on the entry page,
    which nearly every note links back to.
- **An image of a note** (`![x](a.md)`) counts as a link to the note.
- **Raw HTML and diagrams** (2026-10-28, review fix). A note's own `<a href>`
  / `<img src>`, an HTML comment, or a URL inside generated HTML (merman's
  SVG) is not a markdown node, so neither the walk nor the markdown-link pass
  sees it. `sitelink::scrub`, called by `site::page`, runs over every page's
  content after rendering and enforces the invariant for every URL attribute
  (`href`, `src`, `srcset`, `xlink:href`, `poster`, `data`, `action`,
  `formaction`, `cite`, `background`): a web URL, a fragment, or a relative
  URL to a file the site serves. Any other attribute is removed and reported
  with the nearest source line ("raw HTML or a diagram naming no published
  file"), and `<!-- … -->` comments are removed. Raw HTML does not publish
  anything: a raw `<img>` works when a markdown link or embed publishes the
  same file, and is reported dead otherwise. Not covered: CSS `url()` inside
  a raw `<style>` or `style` attribute.
- **Page weight.** Each page is self-contained, as in the reader, so a page
  with math carries the math stylesheet and its base64 fonts (about 0.7 MB).
  The thesis wiki publishes to about 23 MB: 15 MB of pages, 5.2 MB of
  fonts (the Nerd Font code faces are 1.1 MB each, fetched only by pages
  with code; Adwaita Sans is 0.7 MB for both files), and the figures.
- **The Worker** compares SHA-256 digests of the given and the stored
  password with `crypto.subtle.timingSafeEqual` (equal lengths whatever was
  typed), fails closed when `SITE_PASSWORD` is unset, and marks every served
  file `Cache-Control: private` (static assets come marked `public`, which a
  shared cache may keep). `deploy/cloudflare/worker.test.mjs` tests it under
  plain node: `node --test deploy/cloudflare/`.

## Implementation plan

Each step lands as its own commit with tests; `cargo test`, `cargo clippy -D
warnings` and `cargo fmt` stay green throughout.

| # | Step | Touches | Done when |
|---|---|---|---|
| 1 | `LinkStyle` in `Options`; `wikilink` and `textscan` call `href` | `core/pipeline.rs`, `core/wikilink.rs`, `core/textscan.rs` | Reader output is byte-identical (existing tests and benches); new tests cover relative site hrefs, including spaces, `#`, `^block` and nested folders |
| 2 | Site-mode markdown-link pass, CSP and dark media query | `core/pipeline.rs`, new pass module | Unit tests: `.md`→`.html`, assets, outside-vault and unpublished targets dead, CSP per mode |
| 3 | `core::site::plan`: link walk from the entry note, output paths, referenced assets, backlinks, dead links | new `core/site.rs` | Unit tests over injected entries: an unlinked note and an unreferenced asset stay out, a link outside the root is dead and reported, backlinks match `graph::scan` |
| 4 | Navigation HTML (TOC sidebar, backlinks, breadcrumb) | `core/site.rs`, CSS in `core/assets/` | Rendered from the thesis wiki; screenshot reviewed by the owner |
| 5 | `jumanji-publish` binary: explicit root, config, parallel render, asset copy, dead-link report, swap with marker | `src/bin/jumanji-publish.rs` | Integration test: publish `demo/` into a temp dir, then check that every `href`/`src` in every page points at a file that exists or at a fragment present in that page (a link checker, run in `cargo test`). Thesis wiki: no `CLAUDE.md` in the output, the papers PDF reported dead |
| 6 | Password script, wrangler config, deploy script, docs | `deploy/cloudflare/`, `scripts/publish-site.sh`, this page, top-level README | Owner has run the one-time setup; in a private window the URL asks for a password; `curl -I` without credentials returns `401` with `X-Robots-Tag`, with credentials `200` |

## Open questions

- **The papers PDF.** `index.md` links to a PDF in `thesis/papers/`, outside
  the wiki. As planned, that link is dead on the site. Publishing it means
  copying the PDF into `wiki/`. It is someone else's thesis, so that is the
  owner's call.

## Sources

- `run_worker_first`, a script in front of static assets:
  <https://developers.cloudflare.com/workers/static-assets/routing/worker-script/>
- Access login methods, one-time PIN (no shared password):
  <https://developers.cloudflare.com/cloudflare-one/identity/one-time-pin>
- Cloudflare changelog, one-click Access for Workers (2025-10-03):
  <https://developers.cloudflare.com/changelog/post/2025-10-03-one-click-access-for-workers/>
- Preview URL default behaviour, Wrangler 4.44.0 (2025-10-23):
  <https://developers.cloudflare.com/changelog/post/2025-10-23-preview-url-default-behavior/>
- Static asset limits: <https://developers.cloudflare.com/workers/platform/limits>
- Custom headers for static assets: <https://developers.cloudflare.com/workers/static-assets/headers/>
- Zero Trust plans (Access free for up to 50 users): <https://www.cloudflare.com/plans/zero-trust-services/>
- Pages bare `pages.dev` hostname and Access (third-party guide):
  <https://dev.classmethod.jp/articles/cloudflare-pages-access/>
- GitHub Pages limits: <https://docs.github.com/en/pages/getting-started-with-github-pages/github-pages-limits>
