# The build pipeline

A build has four stages: discover, parse, render, write. Each stage takes the
previous stage's output and never reaches back.

Parsing produces the [content model](content-model.md); rendering is described
in [Rendering](rendering.md); [plugins](plugins.md) may run between stages.
Up: [Architecture](../architecture.md).
