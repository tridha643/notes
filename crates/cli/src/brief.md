## Briefing the user on a PR (`notes brief`)

As briefly as possible, explain the PR (or the current branch against its base)
in stage, not in chat. Read `notes agent-howto` and `notes guide` first if you
haven't this session.

Post one card, `[markdown, html]`:

```sh
notes post markdown brief.md --html before-after.html --title "PR #123: retry on 429" --session-title "Brief: PR #123"
```

1. `brief.md`: what the PR changes and why, in three to five short lines. Name
   the files or modules that carry the change.
2. `before-after.html`: one inline `<svg width="100%" viewBox="0 0 1040 H">`,
   before on the left and after on the right, each column headed "Before" and
   "After". Draw in O'Reilly line-art style: 1px strokes, plain boxes and
   arrows, `fill="none"`, small `ts` labels, no shadows or gradients. Stroke
   with `var(--color-border-primary)` and color only what the PR adds or
   changes with `var(--color-text-info)`, so the eye lands on the difference.
   Keep both columns on the same grid so unchanged parts sit in the same place.

If a diagram can't show the change (a pure rename, a dependency bump), drop the
html part and add `--diff` with the key hunk instead.

Reply in chat with one line pointing at the stage card.
