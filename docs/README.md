# jumanji documentation

jumanji is a zathura-inspired markdown reader: Rust, GTK4 and the system
WebKitGTK, with a content pipeline written entirely in Rust. The user manual is
the [top-level README](../README.md). This folder holds everything behind it:
why jumanji is built the way it is, how to test it, and what changed when.

## Start here

| Document | Read it for |
|---|---|
| [Design decisions](#design-decisions) | The decision records, D1–D14, each kept with its feature. **Binding:** a change that departs from a decision updates it in the same commit, with reasons. |
| [architecture/](architecture/README.md) | The cross-cutting decisions: stack, layers, pipeline, extension seams, performance; plus goal, non-goals, milestones, component map and risks. |
| [DEVLOG.md](DEVLOG.md) | The running log, newest first: what changed, why, what's next. |
| [TESTING.md](TESTING.md) | The three test layers (unit, controller, end to end under Xvfb), the benchmarks, and how to read CI. |
| [research/](#research) | The research the design rests on. Cite it; don't re-argue it without new evidence. |

Outside this folder: [STATUS.md](../STATUS.md) is the live dashboard of what
is in flight, and [CLAUDE.md](../CLAUDE.md) holds the conventions for working
in the repo.

## Design decisions

Each decision lives with the feature it shapes. The IDs are stable: code
comments cite them as "DESIGN D11", and [DESIGN.md](DESIGN.md) maps each old
anchor to the new home.

| # | Decision | Home |
|---|---|---|
| [D1](architecture/README.md#d1-language--rust) | Language — Rust | [architecture/](architecture/README.md) |
| [D2](architecture/README.md#d2-ui--gtk4-rs--system-webkitgtk-6-girara-style-shell-reimplemented) | UI — gtk4-rs + system WebKitGTK 6, girara-style shell reimplemented | [architecture/](architecture/README.md) |
| [D2a](architecture/README.md#d2a-three-layers--core-controller-toolkit-shell-2026-09-02) | Three layers — core, controller, toolkit shell | [architecture/](architecture/README.md) |
| [D3](architecture/README.md#d3-content-pipeline--100-rust-no-javascript) | Content pipeline — 100% Rust, no JavaScript | [architecture/](architecture/README.md) |
| [D4](keys/README.md#d4-keybindings--gtk-capture-phase-zathura-semantics) | Keybindings — GTK capture phase, zathura semantics | [keys/](keys/README.md) |
| [D5](keys/README.md#d5-config--toml-zathura-idioms) | Config — TOML, zathura idioms | [keys/](keys/README.md) |
| [D5a](reading/README.md#d5a-two-axis-zoom) | Two-axis zoom | [reading/](reading/README.md) |
| [D5a.0](reading/README.md#d5a0-anything-that-changes-a-blocks-height-must-be-anchored-2026-09-13) | Anything that changes a block's height must be anchored | [reading/](reading/README.md) |
| [D5a.1](reading/README.md#d5a1-wide-blocks--pictures-get-the-window-prose-keeps-the-measure-2026-09-13) | Wide blocks — pictures get the window, prose keeps the measure | [reading/](reading/README.md) |
| [D5a.2](reading/README.md#d5a2-diagram-fit-and-per-diagram-zoom-2026-09-13) | Diagram fit and per-diagram zoom | [reading/](reading/README.md) |
| [D6](architecture/README.md#d6-extensibility--pipeline-seams-not-a-plugin-abi) | Extensibility — pipeline seams, not a plugin ABI | [architecture/](architecture/README.md) |
| [D7](editor-sync/README.md#d7-editor-pairing--the-synctex-analogue-built) | Editor pairing — the SyncTeX analogue | [editor-sync/](editor-sync/README.md) |
| [D8](math/README.md#d8-math--pulldown-latex--mathml-core-no-javascript-m3) | Math — pulldown-latex → MathML Core, no JavaScript | [math/](math/README.md) |
| [D9](stdin/README.md#d9-stdin-streaming-m3) | stdin streaming | [stdin/](stdin/README.md) |
| [D10](jumplist/README.md#d10-cross-document-jumplist-navigation-post-10) | Cross-document jumplist navigation | [jumplist/](jumplist/README.md) |
| [D11](obsidian/README.md#d11-obsidian-dialect--vault-resolution-post-10-implemented) | Obsidian dialect & vault resolution | [obsidian/](obsidian/README.md) |
| [D12](reading/README.md#d12-a-document-opens-where-it-is-meant-to-open-post-10-implemented) | A document opens where it is meant to open | [reading/](reading/README.md) |
| [D13](architecture/README.md#d13-performance-regressions-are-judged-in-instructions-felt-in-milliseconds-2026-09-02) | Performance regressions are judged in instructions, felt in milliseconds | [architecture/](architecture/README.md) |
| [D14](graph/design.md#d14-the-document-graph--a-route-spine-with-its-links-fanned-out-2026-09-18) | The document graph — a route spine with its links fanned out | [graph/design.md](graph/design.md) |

## Features with their own docs

| Folder | Feature | Decisions |
|---|---|---|
| [keys/](keys/README.md) | Modal keys, the config file and the default key map | [D4](keys/README.md#d4-keybindings--gtk-capture-phase-zathura-semantics), [D5](keys/README.md#d5-config--toml-zathura-idioms) |
| [reading/](reading/README.md) | Zoom, anchored reading position, wide blocks, diagram fit, opening position | [D5a](reading/README.md#d5a-two-axis-zoom), [D5a.0](reading/README.md#d5a0-anything-that-changes-a-blocks-height-must-be-anchored-2026-09-13)–[D5a.2](reading/README.md#d5a2-diagram-fit-and-per-diagram-zoom-2026-09-13), [D12](reading/README.md#d12-a-document-opens-where-it-is-meant-to-open-post-10-implemented) |
| [editor-sync/](editor-sync/README.md) | Editor pairing, forward and reverse, and the Neovim plugin | [D7](editor-sync/README.md#d7-editor-pairing--the-synctex-analogue-built) |
| [math/](math/README.md) | LaTeX math rendered to MathML | [D8](math/README.md#d8-math--pulldown-latex--mathml-core-no-javascript-m3) |
| [stdin/](stdin/README.md) | Reading markdown from a pipe | [D9](stdin/README.md#d9-stdin-streaming-m3) |
| [jumplist/](jumplist/README.md) | The cross-document jumplist and back navigation | [D10](jumplist/README.md#d10-cross-document-jumplist-navigation-post-10) |
| [obsidian/](obsidian/README.md) | The Obsidian dialect and vault resolution | [D11](obsidian/README.md#d11-obsidian-dialect--vault-resolution-post-10-implemented) |
| [graph/](graph/README.md) | The document graph (`t`): [README](graph/README.md) (the feature), [interaction model](graph/interaction.md), [design](graph/design.md), [visual reference](graph/mockup.html) | [D14](graph/design.md#d14-the-document-graph--a-route-spine-with-its-links-fanned-out-2026-09-18) |

## Research

| Note | Covers |
|---|---|
| [01 — landscape](research/01-landscape.md) | Desktop markdown readers and viewers: what exists and what users ask for. |
| [02 — zathura](research/02-zathura.md) | zathura's architecture and UX, the spec for jumanji's reading model. |
| [03 — Rust stack](research/03-rust-stack.md) | Webview vs native, mermaid, parsing: the building blocks and their versions. |
| [04 — Obsidian](research/04-obsidian.md) | Obsidian's markdown dialect and link resolution. |
| [05 — macOS port](research/05-macos-port.md) | The macOS port proposal ([issue #1](https://github.com/membranepotential/jumanji/issues/1)). |
