# Architecture

A build runs in stages. Lighthouse reads the source tree into a content model,
renders each page through its layout, and writes the output — skipping every
page whose inputs have not changed since the last build.

- [The build pipeline](architecture/pipeline.md) — the stages and their order.
- [The content model](architecture/content-model.md) — pages, collections, assets.
- [Storage](architecture/storage.md) — where sources, metadata and output live.
- [The build cache](architecture/cache.md) — how unchanged work is skipped.
- [Plugins](architecture/plugins.md) — where extensions hook in.
- [[incremental-builds|Incremental builds]] — what a change rebuilds.
- [Rendering](architecture/rendering.md) — markdown, templates and minification.

Back to the [overview](README.md).
