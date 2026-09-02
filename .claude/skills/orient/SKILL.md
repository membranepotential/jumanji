---
name: orient
description: Project orientation dashboard. Reconciles the repo's STATUS.md against ground truth (git log, task list, in-flight work, open issues), rewrites it, and prints a short brief. Use whenever the user asks "what's the status", "where are we", "what's next", "what have we achieved", "/orient" — or to bootstrap STATUS.md in a project that lacks one.
---

# /orient — reconcile and brief

STATUS.md is the project's single orientation artifact: direction, active workstreams, recent wins, queue, open questions. This skill makes it match reality, then answers the user's question from it. The file is the deliverable; the brief is the interface.

## The file

Lives at the repo root (`git rev-parse --show-toplevel`), git-tracked. **Hard budget: 60 lines.** It is a dashboard, not a log — history lives in git; prune rather than append.

```markdown
# Status — <project>

_reconciled: <YYYY-MM-DD> @ <short-sha>_

## Goal
<1–3 lines. The north star. Rarely changes — do not rewrite it on reconcile unless the user redirects the project.>

## Now
- **<workstream>**: <objective> — <state> — next: <concrete next step>

## Done (recent)
- <MM-DD>: <one line, outcome not activity>

## Next
1. <ordered, 3–7 items; reference issue numbers where they exist>

## Open questions
- <decisions pending on the user>
```

Section rules: **Now** = one line per active workstream/agent/background task (this is the agent registry — every spawned subagent or parallel session must have a line). **Done** = newest first, max 8, merge trivial commits into one outcome line. **Next** = ordered queue, not a wishlist.

## Procedure

### If STATUS.md is missing → bootstrap

1. Goal: distill from README / CLAUDE.md / the user's stated intent (ask only if genuinely underivable).
2. Done: `git log --oneline --since='14 days ago'` → group into ≤8 outcome lines.
3. Next: open issues if the repo has them (`gh issue list --state open --limit 15`, respect priority labels), else infer from TODOs/recent conversation, else leave a placeholder and say so.
4. Now: only what is verifiably in flight (uncommitted work, running tasks). Empty is fine.
5. Write the file, show it to the user in full (it's ≤60 lines).

### If STATUS.md exists → reconcile

1. Parse the `_reconciled: … @ <sha>_` marker. Gather ground truth since then, cheap things only:
   - `git log --oneline <sha>..HEAD` and `git status --porcelain`
   - TaskList (session tasks), running background tasks/agents you know about
   - If a `/next` pipeline exists (`.next/config.json`): in-flight markers, open issue queue
   - `gh issue list`/`gh run list` only if the repo uses them and it's fast; skip silently on failure
2. Rewrite the file against that evidence:
   - Landed work: move the Now entry to Done (dated, one line); prune Done to 8.
   - Now entries with no evidence of activity since last reconcile: mark `— stalled?` rather than deleting; the user decides.
   - Uncommitted changes with no Now entry: add one (best-effort description from the diff paths).
   - Refresh Next from the issue queue / stated intent; keep the user's own ordering unless evidence contradicts it.
   - Never touch Goal or Open questions except on user instruction or when a question is now answered (then note the answer in Done).
   - Update the reconciled marker. Enforce the 60-line budget.
3. Respect project formatting conventions (e.g. run prettier on the file if the project formats markdown). Do **not** commit — leave the change staged-able for the user's normal flow.

### The brief (what you print)

≤12 lines, always this shape:

```
<Goal, 1 line>
Now:   <one line per workstream, with state>
Next:  <top 3>
Since last reconcile: <N commits — 1-line gist>; <anything surprising: red CI, stalled workstream, drifted plan>
```

No prose padding. If something contradicts the file (e.g. CI red, a workstream silently dead), say it in the brief — surprises are the point of reconciling.

## Boundaries

- This skill never starts or redirects work; it only reports and updates the dashboard.
- Never let STATUS.md grow into a plan document — plans live in their own files/issues; STATUS.md links to them.
