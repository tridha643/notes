# stage

A terminal UI (ratatui + crossterm) that shows agents' stage posts as cards. It's built for a
Rex pane beside the agent, and draws images, mermaid and html through kitty graphics. The design and its history are in [docs/stage-plan.md](../../docs/stage-plan.md).

```sh
stage                 # every session in ~/Desktop/Stage, newest first
stage --session <id>  # open one
```

## Keys

`j/k` next/previous post · `J/K` session · `↑↓` line · `⏎` fold, or open the selected session ·
`space/b` page · `^D/^U` half page · `]/[` next/previous diff hunk (else card) · `./,` next/previous card · `n/N` next/previous match of the last search · `g/G` top/bottom · `z` focus the card under the cursor (full-width, `z`/`Esc` to return; a diagram: `+`/`-` zoom, arrows or `hjkl` pan, `0` fit) · `s` or the `≡` header control toggles the sidebar · `/` search sessions · `c` comment (anchored to the selected
lines on code and diff) · `r` rename the selected session · `d` delete the card under the
cursor, else the shown post · `D` delete the selected session (deletes wait for `y`) · `v`
older version · `o` open html in the browser · `:` commands (`open copy versions rerender
help`) · `q` quit. The sidebar lists sessions only; a session's posts are in the feed.

## Phone and narrow panes

Below 60 columns, the feed stays full-width. Tap `≡` (or press `s`) for a
full-width session picker; tap a session or press Enter to open it, and Escape
or `s` to return without losing your place. Search results remain visible even
when the sidebar is hidden. The compact bottom controls are tappable: up/down
page with one line of overlap, find, reply, and focus (as space permits).

Wheel events move one line in narrow panes or feeds shorter than 20 rows,
including when a phone keyboard reduces the viewport. Already-queued wheel events
are coalesced within a 4ms work budget, without waiting before painting the first
input. On text feeds, native scroll regions move existing rows and only changed
cells are sent. Rex's horizontal margins let this work beside a stationary sidebar;
other terminals use the fast path only on full-width feeds. Resize transitions and
visible graphics use normal cell diffs. Set `STAGE_NO_SCROLL_REGION=1` to compare
the fallback.
Every resize notification invalidates the screen, even when coalesced signals
report the previous dimensions after a shrink/grow round trip. Stage clears and
repaints text, then restores Kitty images after that clear.
Scrolling back within
the newest post pauses auto-follow until you reach the bottom again. Stage
handles the wheel events Rex sends; native swipe recognition and inertia remain
Rex's responsibility. Mouse drags still select/copy text.

Wide markdown tables stack into labelled fields when columns would become
unreadable. Long inputs keep their insertion point visible. HTML and newly
rendered diagrams scale typography relative to high-density terminal rows
rather than assuming every client uses a 2x display; the agent's reported CSS
viewport uses the same scale. This does not remove HTML's viewport-height cap.

## Structure

The code splits IO from decisions, so nearly everything is testable without a terminal:

- **[`app.rs`](src/app.rs)** holds all the state and every decision, with no IO. `App::update`
  takes a message (key, mouse, notesd event, reply) and returns effects for `main` to perform.
- **[`ui.rs`](src/ui.rs)** draws header, sidebar, title bar, cards and keybar, purely from `App`.
- **[`cards.rs`](src/cards.rs)** builds a card once per (post, version) from `post-show` and
  `post-render`, then lays it out to the pane width with borders and folds.
- **[`client.rs`](src/client.rs)**, the `StageClient`, can only call read verbs plus
  `comment-new`, `session-rename`, `session-delete`, `post-delete` and `post-part rm`. stage
  can't add to or change a post even if it has a bug, and it deletes only after the user
  presses `y`.
- **[`screen.rs`](src/screen.rs)** presents reusable frame buffers, optionally scrolling a
  text region before diffing the remaining cells (rectangular regions on Rex). It owns presentation through
  ratatui's manual-rendering API; don't mix it with `Terminal::draw`/`flush`.
- **[`main.rs`](src/main.rs)** handles the terminal and the event loop. crossterm's reader and
  the notesd `watch` stream feed one channel, and the loop blocks on it, drains everything
  pending, then draws once. An idle stage never wakes, and a 240-key paste is one frame.
- **[`commands.rs`](src/commands.rs)**: a new `:` command is one row in `COMMANDS`.

## Graphics

[`gallery.rs`](src/gallery.rs) owns the pixels; `App` only learns each visual's size.

- `stage-helper` (Swift, [`apps/Notes/Sources/StageHelper`](../../apps/Notes/Sources/StageHelper),
  installed next to `stage`) turns mermaid into SVG with pele, decodes images, and renders html
  in an offscreen WKWebView, sending frames through POSIX shared memory while a page animates.
- resvg rasterizes SVG on a worker thread at the card's pixel width.
- [`kitty.rs`](src/kitty.rs) transmits each image once over shared memory (`t=s`) and places it
  in cells. Every escape carries `q=2`, so the terminal never answers into stage's input.
- Without the helper, those cards fall back to text ("no stage-helper").
- [`page.rs`](src/page.rs) wraps every html part in stage's own page (sideshow's theme tokens in
  stage's palette, base styles, SVG classes, kits) before the helper loads it, as Notes.app does.
- An html page is laid out at the card's width and may grow to the feed's height. stage writes
  that space, with the card's text width, to the `stage.space` setting whenever it changes, and
  `notes guide` and `notes post` hold agents to it.

[`probe.rs`](src/probe.rs) queries terminal capabilities before the first frame, ending with
DA1 so stage stops waiting as soon as the terminal has answered.

## Security

Post data is untrusted. [`sanitize.rs`](src/sanitize.rs) is the only path from post data to
terminal cells: it drops ESC, C0 (except tab), DEL and C1, so a post can't move the cursor,
set the clipboard (OSC 52), open links (OSC 8) or start APC/DCS strings. It also caps the size
of every part and post.

## Debug and bench

The [Rex scroll latency investigation](../../docs/bench/stage-scroll-latency.md)
records the Hunk source comparison, matched before/after measurements, and remaining
GPU/remote-client verification gaps.

| Variable | Effect |
|---|---|
| `STAGE_HELPER` | path to a different `stage-helper` |
| `STAGE_TRACE`, `STAGE_LOOP_TRACE`, `STAGE_HELPER_TRACE` | trace logs |
| `STAGE_HELPER_LOG` | helper stderr |
| `STAGE_LATENCY_LOG` | per-post paint times, for `scripts/stage-latency.sh` |

```sh
cargo test -p stage
cargo bench -p stage --bench frames # isolated text input/resize latency and output budgets
cargo bench -p stage            # includes helper-backed visual budgets: benches/budget.txt
python3 scripts/stage-scroll-e2e.py REX_SESSION  # live Rex A/B, isolated generated fixtures
python3 scripts/stage-resize-e2e.py REX_SESSION # background Rex resize recovery, isolated text/PNG
scripts/stage-latency.sh        # 100 posts, fails if publish→paint p95 ≥ 250 ms
```
