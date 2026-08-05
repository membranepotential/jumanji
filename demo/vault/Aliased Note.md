---
aliases:
  - Start there
  - The Alias
---

# Aliased Note

The filename has a space in it, which is deliberate: a wikilink target is not
percent-encoded in the source, but the emitted `file://` URI must be.

Both `[[Aliased Note]]` and `[[Start there]]` reach this file — aliases
participate in resolution, so a vault index has to read every note's
frontmatter, not just its filename.

Back to [[Welcome]].
