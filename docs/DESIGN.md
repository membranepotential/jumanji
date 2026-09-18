# jumanji — design & decision record

The decision record now lives with its feature; this page maps IDs to
their homes. The decisions are **binding**: a change that departs from
one updates it in the same commit, with reasons. The
[docs index](README.md#design-decisions) lists them all.

## Goal

Moved to [architecture/README.md](architecture/README.md#goal).

## The gap we fill

Moved to [architecture/README.md](architecture/README.md#the-gap-we-fill).

## Decisions

### D1: Language — Rust

Moved to [architecture/README.md](architecture/README.md#d1-language--rust).

### D2: UI — gtk4-rs + system WebKitGTK 6, girara-style shell reimplemented

Moved to [architecture/README.md](architecture/README.md#d2-ui--gtk4-rs--system-webkitgtk-6-girara-style-shell-reimplemented).

### D2a: Three layers — core, controller, toolkit shell (2026-09-02)

Moved to [architecture/README.md](architecture/README.md#d2a-three-layers--core-controller-toolkit-shell-2026-09-02).

### D3: Content pipeline — 100% Rust, no JavaScript

Moved to [architecture/README.md](architecture/README.md#d3-content-pipeline--100-rust-no-javascript).

### D4: Keybindings — GTK capture phase, zathura semantics

Moved to [keys/README.md](keys/README.md#d4-keybindings--gtk-capture-phase-zathura-semantics).

### D5: Config — TOML, zathura idioms

Moved to [keys/README.md](keys/README.md#d5-config--toml-zathura-idioms).

### D5a: Two-axis zoom

Moved to [reading/README.md](reading/README.md#d5a-two-axis-zoom).

### D5a.0: Anything that changes a block's height must be anchored (2026-09-13)

Moved to [reading/README.md](reading/README.md#d5a0-anything-that-changes-a-blocks-height-must-be-anchored-2026-09-13).

### D5a.1: Wide blocks — pictures get the window, prose keeps the measure (2026-09-13)

Moved to [reading/README.md](reading/README.md#d5a1-wide-blocks--pictures-get-the-window-prose-keeps-the-measure-2026-09-13).

### D5a.2: Diagram fit and per-diagram zoom (2026-09-13)

Moved to [reading/README.md](reading/README.md#d5a2-diagram-fit-and-per-diagram-zoom-2026-09-13).

### D6: Extensibility — pipeline seams, not a plugin ABI

Moved to [architecture/README.md](architecture/README.md#d6-extensibility--pipeline-seams-not-a-plugin-abi).

### D7: Editor pairing — the SyncTeX analogue (built)

Moved to [editor-sync/README.md](editor-sync/README.md#d7-editor-pairing--the-synctex-analogue-built).

### D8: Math — pulldown-latex → MathML Core, no JavaScript (M3)

Moved to [math/README.md](math/README.md#d8-math--pulldown-latex--mathml-core-no-javascript-m3).

### D9: stdin streaming (M3)

Moved to [stdin/README.md](stdin/README.md#d9-stdin-streaming-m3).

### D10: Cross-document jumplist navigation (post-1.0)

Moved to [jumplist/README.md](jumplist/README.md#d10-cross-document-jumplist-navigation-post-10).

### D11: Obsidian dialect & vault resolution (post-1.0; implemented)

Moved to [obsidian/README.md](obsidian/README.md#d11-obsidian-dialect--vault-resolution-post-10-implemented).

### D12: A document opens where it is meant to open (post-1.0; implemented)

Moved to [reading/README.md](reading/README.md#d12-a-document-opens-where-it-is-meant-to-open-post-10-implemented).

### D13: Performance regressions are judged in instructions, felt in milliseconds (2026-09-02)

Moved to [architecture/README.md](architecture/README.md#d13-performance-regressions-are-judged-in-instructions-felt-in-milliseconds-2026-09-02).

### D14: The document graph — a route spine with its links fanned out (2026-09-18)

Moved to [graph/design.md](graph/design.md#d14-the-document-graph--a-route-spine-with-its-links-fanned-out-2026-09-18).

## Non-goals

Moved to [architecture/README.md](architecture/README.md#non-goals).

## Milestones

Moved to [architecture/README.md](architecture/README.md#milestones).

## Keybinding spec (M1 + M2)

Moved to [keys/README.md](keys/README.md#keybinding-spec-m1--m2).

## Component boundaries

Moved to [architecture/README.md](architecture/README.md#component-boundaries).

## Risks & mitigations

Moved to [architecture/README.md](architecture/README.md#risks--mitigations).
