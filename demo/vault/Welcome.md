---
title: Welcome
aliases: [Start here]
tags: [demo, obsidian]
---

# Demo vault

- [[Concepts/Callouts]] — by full vault-relative path.
- [[Aliased Note]] — a filename with a space in it.
- [[Start there]] — the same note, through its frontmatter alias.
- [[Concepts/Callouts#Folding]] — a heading fragment in another document.
- [[Nowhere]] — unresolved: visibly dead, and deliberately not clickable.
- [[#Embeds]] — a heading in this file.

This folder is a vault because you opened jumanji in it — there is no marker
file and nothing to install. The links above resolve vault-wide **by name**
rather than by relative path, so `[[Callouts]]` means the same note from
anywhere in the tree: the root outranks a sibling folder, matching is
case-insensitive, and frontmatter aliases participate.

Open this file from somewhere else (`cd /tmp && jumanji …/Welcome.md`) and the
links above go inert — the index is rooted where you launched, not where the
document happens to live.

## Embeds

An image embed, sized by the pipe:

![[attachments/diagram.png|320]]

A note embed degrades to a link-card, because transclusion is deferred:

![[Concepts/Callouts]]

## The rest of the dialect

Text can be ==highlighted==, carry an inline footnote^[which folds into the
usual footnote list at the bottom], and hide %%an editor-only comment%% that
never reaches the page.

- [x] a finished task
- [ ] an unfinished one
- [?] a task with a non-standard marker
- [-] and another

This paragraph carries a block identifier, so `[[Welcome#^welcome-block]]`
has somewhere to land. ^welcome-block
