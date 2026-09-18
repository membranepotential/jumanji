---
title: Storage — sources, metadata and build output
---

# Storage

Lighthouse keeps three stores: the source tree (read-only), a metadata
database under `.lighthouse/`, and the output directory. Only the build
writes to the last two.

The metadata database holds the [build cache](cache.md) and a snapshot of the
[content model](content-model.md), which together make
[[incremental-builds]] possible. Paths are set in
[Configuration keys](../reference/config-keys.md).

Up: [Architecture](../architecture.md) · [overview](../README.md)
