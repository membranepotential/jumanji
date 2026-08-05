# Callouts

All 13 canonical types, each reachable through any of its 27 spellings. The
class carries the canonical type; `data-callout` keeps the literal one, so an
Obsidian theme keying off it still works.

> [!note]
> The default title is the type in title case.

> [!abstract] Also spelled `summary` or `tldr`
> One accent, three spellings.

> [!info]
> Plain information.

> [!todo]
> Something still to do.

> [!tip] Also `hint` and `important`
> `important` is an alias of `tip` in Obsidian, unlike GitHub — a deliberate
> divergence.

> [!success] Also `check` and `done`
> It worked.

> [!question] Also `help` and `faq`
> Twenty-two of the twenty-seven spellings render as literal `[!question]`
> garbage under comrak's `alerts` extension. That is why this pass exists.

> [!warning] Also `caution` and `attention`
> Careful.

> [!failure] Also `fail` and `missing`
> It did not work.

> [!danger] Also `error`
> It went badly wrong.

> [!bug]
> A defect.

> [!example]
> Worked through.

> [!quote] Also `cite`
> Someone else said it first.

## Nesting

> [!note] Outer
> Blockquote nesting falls out of the recursive pass.
>
> > [!tip] Inner
> > No extra machinery needed.

## Unknown types

> [!totally-made-up] Custom
> An unknown type keeps its literal `data-callout` and falls back to the
> `note` style, exactly as Obsidian does — so a theme can style it and nothing
> breaks if the theme is absent.

## Filler

Everything below exists so the "Folding" heading sits well past the fold, which
is what makes the cross-document fragment link from `Welcome` observable as a
scroll rather than as a no-op.

Callouts are a blockquote whose first line is `[!type]`, optionally followed by
a fold marker and a title. The fold marker is the only part comrak's alert
extension gets actively wrong rather than merely missing: `> [!tip]- Foldable`
yields the title string `"- Foldable"`.

Because the wrapper is emitted as raw-HTML sibling blocks carrying the
blockquote's own source line, Ctrl+click on a callout still lands on the right
line in the editor — the D7 reverse-sync invariant survives the rewrite.

The title is plain text: dropping the `[!type]` line drops its inline markup
with it. That is an accepted limitation, recorded in the design.

A callout's body is ordinary markdown, so it may contain lists, fences,
wikilinks and embeds:

> [!example] A callout with a body
> 1. A list item.
> 2. Another.
>
> ```rust
> fn inside_a_callout() {}
> ```
>
> And a link back to [[Welcome]].

More filler, so that the next heading is comfortably below the first screen.

Obsidian shows no disclosure affordance for a callout without a fold marker,
so jumanji emits a plain `<div>` there and reserves `<details>` for the two
marked forms. Folding therefore costs no JavaScript at all.

The alias table lives in typed Rust rather than in a CSS selector list, which
is what keeps the 27 spellings from leaking into the stylesheet.

## Folding

A `+` marker means foldable and open:

> [!note]+ Expanded by default
> Click the title to collapse this.

A `-` marker means foldable and collapsed:

> [!faq]- Collapsed by default
> This body is hidden until the summary is clicked. `faq` is an alias of
> `question`.

No marker at all means no disclosure control:

> [!info]
> A plain box — no triangle, nothing to click.

Back to [[Welcome]].
