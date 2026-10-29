# Posting to stage: design guide for agents

Your posts appear as cards in stage, a terminal pane beside the user, grouped
into this conversation's session. Read this once before your first post.
Adapted from sideshow's agent design guide. `notes post` checks every post
against the user's card: it refuses one that can't fit (an `error:` line says
why, and nothing is sent) and warns about the rest (`warning:` lines).

## Say less

- One idea per post. Publish a series of small posts with distinct titles
  rather than one long page.
- Sentence case for titles, headings and labels ("Retry flow", not
  "Retry Flow"). No emoji.
- Lead with the answer; cut filler. A card is read at a glance.
- To refine a post, update it (`notes post update <id> <kind> <file>`): same
  card, new version, same place in the feed. Don't publish a near-duplicate.

## Show code as code

When the point is code (a change you made or propose, how something works,
where a bug is), put the code on the card. A paraphrase of code makes the
user trust you; the code itself lets them check you.

- A change, made or proposed: a `diff` part with the real patch, even for
  three lines. Pipe it: `git diff --no-color -- src/retry.rs | notes post diff - --title "Retry on 429"`.
  For a proposal, write the patch by hand.
- Why and what on one card: markdown above, diff below. Keep the markdown to
  what the diff can't show (the cause, the tradeoff, what you ruled out):
  `notes post markdown why.md --diff change.patch --title "Retry on 429"`.
- How existing code works: a `code` part with only the lines that matter,
  numbered as in the file, then a sentence on what to look at:
  `notes post code src/store.rs --lines 480:505 --md note.md --title "How posts are ordered"`.
- A path through several files: a `mermaid` flowchart of the calls, then a
  code excerpt per step that matters.
- A big change: one [markdown, diff] post per piece (the data shape, the call
  site, the test), not one 400-line patch.
- Options to choose between: one diff per option, titled by the option, so
  the user compares code, not descriptions of it.
- Fenced code inside markdown is for a one-to-five line snippet in the middle
  of an explanation. Never put code in a table cell.
- Diffs read best unified. Use `--layout split` only on a card wider than 140
  cells.

## Fit the card

A card is read without scrolling sideways. Its size follows the user's pane:
"Right now" at the end of this guide gives the live numbers, and `notes post`
holds every post to them.

- Text parts (markdown, code, diff, terminal, json) wrap at the card's width
  in cells. A markdown table wider than the card is refused: shorten cells or
  cut columns. Keep tables to 5 columns of a few words each.
- Markdown parts under 40 lines; past that, split into posts. Paragraphs
  under 60 words.
- Fenced code lines under the card's width; wider lines wrap.
- html gets a fixed viewport: the card's width in CSS px, and up to the
  feed's height. A wider page is zoomed out until it fits, so its text
  shrinks; content below the height is cut off.
- mermaid lays out to the html width. A tall diagram scrolls; a wide one
  shrinks.

## Pick the kind

Each part is data the viewer renders, except html, which you draw.

- `markdown`: prose, plans, tradeoffs, tables. Tag fenced code with a language.
- `code`: a whole file or an excerpt (`--lines A:B`); the language comes from
  the file name.
- `diff`: a unified or git patch; prefer it over pasting before/after code.
- `terminal`: shell output; ANSI colors are kept.
- `json`: structured data, shown as a collapsible tree.
- `mermaid`: when the shape of a system is the point. Use `flowchart TD`;
  wide `LR` graphs shrink until the text is unreadable. Keep labels short and
  split big maps into several diagrams. Don't set colors: stage themes it.
- `image`: a screenshot or generated picture.
- `html`: anything you draw (SVG, a bespoke layout, an animation). See below.

## Html parts

An html part runs live in an offscreen web view, and its frames stream into
the pane, so animation works. stage wraps it in a page of its own, so send a
body fragment (a whole document works too). That page gives you:

- Theme tokens, the same as sideshow's: backgrounds
  `--color-background-primary|secondary|tertiary` and `-info|-success|-warning|-danger`,
  text `--color-text-primary|secondary|tertiary` and the same semantic variants,
  borders `--color-border-primary|secondary|tertiary` and the semantic variants,
  fonts `--font-sans|serif|mono`, radii `--border-radius-md|lg|xl`. Use them
  instead of hex colors.
- A dark page: 14px sans text on `--color-background-primary`, 12px padding,
  and plain `button`, `input` and `select` already styled.
- SVG classes: `t`, `ts`, `th` (text), `box`, `arr`, `leader`, and the color
  ramps `c-blue`, `c-teal`, `c-amber`, `c-coral`, `c-green`, `c-red`, `c-gray`
  on a shape or a `<g>`. End a line with `marker-end="url(#arrow)"` for an
  arrowhead.
- Kits with `notes post html F --kit issues` (trees, badges, chips, status
  bars) or `--kit slides` (a stepped deck). `notes kits` lists them.

Your own CSS wins over all of these. The rules `notes post` holds you to:

- Draw SVG as `<svg width="100%" viewBox="0 0 W H">` with W near the card's
  html width. It scales to the card; one taller than the viewport at that
  width is refused, so split it or make it wider than tall.
- No fixed `width` or `min-width` wider than the card; use % or `max-width`.
  Images and SVG with a viewBox already shrink to the card.
- Keep content in normal flow; never `position: fixed`. No `vh` units: the
  page is measured to its content, so there's no viewport height to fill.
- One self-contained file, under 512 KB. No network: inline your CSS, SVG
  and images (`data:` URIs).
- Flat and clean: no gradients, drop shadows or decoration. Two font weights.
- Give an `id` to text and elements you'll change later. An update that only
  changes stylesheets, inline styles of elements with an id, or the text of
  identified leaf elements (headings, `p`, `span`, `li`, `a`, `button`,
  `label`) is patched in place, so animations and state keep running. Any
  other change reloads the page.

## Feedback

The user comments on a post with `c` in stage. Comments print on stderr as
`feedback:` lines after your next post write; to block for one, run
`notes wait`. Answer with a post update for anything substantial.
