# Math

jumanji renders LaTeX math (`$…$`, `$$…$$`) to MathML in Rust, and WebKit
draws it natively, so no JavaScript runs. The fonts ship inside the binary and
the page stays self-contained.

Decisions on this page:

- [D8: Math — pulldown-latex → MathML Core, no JavaScript (M3)](#d8-math--pulldown-latex--mathml-core-no-javascript-m3)

Code: [`src/core/math.rs`](../../src/core/math.rs),
[`src/core/assets/math/`](../../src/core/assets/math/).
Research: [03 — Rust stack](../research/03-rust-stack.md).

## D8: Math — pulldown-latex → MathML Core, no JavaScript (M3)

LaTeX math is "the missing 5%" for a large slice of readers (notes, papers,
lecture material). The M3 target was "KaTeX-equivalent, no JS", and the pipeline
is 100% Rust ([D3](../architecture/README.md#d3-content-pipeline--100-rust-no-javascript)), so a JS math engine (KaTeX/MathJax) is out by construction.

- **Parse:** comrak's own math extension (`math_dollars` + `math_code`). `$x$`,
  `$$x$$`, and `` $`x`$ `` become inline `NodeValue::Math` nodes carrying the raw
  LaTeX — a first-class parse → mutate → format seam, identical in shape to the
  mermaid fence interception ([D3](../architecture/README.md#d3-content-pipeline--100-rust-no-javascript)). GitHub's dollar rules apply, so prose dollars
  ("costs $5 and $10") stay text (encoded in `core::math` tests as documentation).
- **Render:** **pulldown-latex 0.8.0** (crates.io, MIT) — a pure-Rust LaTeX →
  MathML Core renderer (~95% KaTeX coverage). `core::math` walks the AST and
  replaces each `Math` node with an inline raw-HTML `<math>` fragment (inline
  display style for `$…$`, block for `$$…$$`), mirroring [`diagram.rs`](../../src/core/diagram.rs).
  - **Rejected — typst:** pulling in a whole document compiler to typeset a
    fragment is a poor fit (huge dependency, its own markup/layout model, SVG or
    raster output rather than semantic MathML that recolors and reflows for free).
  - **Rejected — KaTeX/MathJax:** JavaScript in the content pipeline, which [D3](../architecture/README.md#d3-content-pipeline--100-rust-no-javascript)
    rules out (no bundled JS engine, no async render races, export-path hostile).
- **Display:** **WebKitGTK renders MathML Core natively** — no JS. Visual quality
  needs pulldown-latex's stylesheet plus the Latin Modern math fonts; both are
  vendored under [`src/core/assets/math/`](../../src/core/assets/math/) (`styles.css` + four WOFF2 files, ~0.5 MB,
  GUST Font License — see [`font/LICENSE.fonts`](../../src/core/assets/math/font/LICENSE.fonts)).
- **Serving — base64 `data:` URIs, not `app://`.** There is no `app://` scheme
  in the code: [D3](../architecture/README.md#d3-content-pipeline--100-rust-no-javascript)'s original plan gave way to a self-contained page (inlined CSS,
  `style-src 'unsafe-inline'`), and math stays consistent with that. `core::math`
  rewrites the stylesheet's `url('font/…woff2')` refs to base64 `data:` URIs at
  runtime (cached once), so the page fetches nothing. **CSP** gains exactly one
  token, `font-src data:` (harmless when a document has no math — nothing
  references a font). The math stylesheet is emitted only when the document
  actually contains math, so math-free pages carry none of its ~0.7 MB weight.
- **Recolor (Ctrl-r):** MathML inherits `color`, so equations recolor with the
  page for free. The one hardcoded colour in the vendored sheet — the negation
  slash's opaque-black gradient stop — is patched to `currentColor` so it stays
  visible in dark mode (marked `jumanji:` in [`assets/math/styles.css`](../../src/core/assets/math/styles.css)).
- **Deterministic fonts — no `local()`, unique family names (binding).** The
  vendored sheet must never consult system fonts: every `local()` source is
  removed and the embedded families are renamed to unshadowable names (`Latin
  Modern Math` → `Jumanji Math`, `LMRoman12` → `Jumanji Roman`). Why: CSS family
  names are shadowable, and Arch's `mathjax2` package registers "Latin Modern
  Math" for MathJax v2's split webfonts — MATH-table-less, huge-ascent subsets —
  which WebKit prefers over our woff2 via `local()`, then derives math layout
  constants from garbage metrics (superscripts flung line-heights above the base,
  fractions split across lines). Unique names + no `local()` keep the
  self-contained page's rendering identical across machines. Marked `jumanji:` in
  [`assets/math/styles.css`](../../src/core/assets/math/styles.css); pinned by a `core::math` unit test (no `local(`,
  unique names present) and an e2e geometry probe (`msup_shift_ratio`).
- **No-page-h-scroll invariant ([D5a](../reading/README.md#d5a-two-axis-zoom)):** display math is wrapped in a
  `.math-scroll` block (a `<span>` set to `display:block`, valid inside the
  enclosing `<p>`) so a wide matrix/alignment scrolls inside its own box, never
  the page — the same mechanism `.table-wrap` and `.mermaid` use.
