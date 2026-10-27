# Posting to stage: design guide for agents

Your posts appear as cards in stage, a terminal pane beside the user, grouped
into this conversation's session. Read this once before your first post.
Adapted from sideshow's agent design guide; `notes post` warns (on stderr,
`warning:` lines) when a post breaks one of these rules.

## Say less

- One idea per post. Publish a series of small posts with distinct titles
  rather than one long page.
- Sentence case for titles, headings and labels ("Retry flow", not
  "Retry Flow"). No emoji.
- Lead with the answer; cut filler. A card is read at a glance.
- To refine a post, update it (`notes post update <id> <kind> <file>`): same
  card, new version. Don't publish a near-duplicate.

## Fit the card

A card is read without scrolling sideways, and wider content wraps. Its
width follows the user's pane: "Right now" at the end of this guide gives
the live width when stage is open (80 otherwise), and `notes post` warns
against it. Size posts to the card:

- Markdown parts under 40 lines; past that, split into posts.
- Paragraphs under 60 words: two or three sentences.
- Tables up to 5 columns with short cells (a few words, no sentences).
  Stage fits the table to the card and wraps long cells, which reads badly.
- Fenced code lines under 80 characters.

## Pick the kind

Each part is data the viewer renders, except html, which you draw.

- `markdown`: prose, plans, tradeoffs, tables. Tag fenced code with a language.
- `code`: a whole file or excerpt; the language comes from the file name.
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
the pane, so animation works. Keep it a light artifact:

- One self-contained file, under 512 KB. No network: inline your CSS, SVG
  and images (`data:` URIs).
- Keep content in normal flow; never `position: fixed`. The card sizes itself
  to the document height.
- The card is narrow (often 400 to 800 px). A wider page is zoomed out to fit,
  so design for the card width or the text gets small.
- Dark background. stage's palette: background `#1e1e2e`, panel `#181825`,
  text `#cdd6f4`, muted `#7f849c`, border `#45475a`; accents blue `#89b4fa`,
  green `#a6e3a1`, yellow `#f9e2af`, red `#f38ba8`, mauve `#cba6f7`,
  teal `#94e2d5`.
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
