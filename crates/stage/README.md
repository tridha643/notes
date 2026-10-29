# stage

A terminal UI (ratatui + crossterm) that shows agents' stage posts as cards. It's built for a
Rex pane beside the agent, and draws images, mermaid and html through kitty graphics. The design and its history are in [docs/stage-plan.md](../../docs/stage-plan.md).

```sh
stage                 # every session in ~/Desktop/Stage, newest first
stage --session <id>  # open one
```

## Keys

`j/k` next/previous post · `J/K` session · `↑↓` line · `⏎` fold, or open the selected session ·
`space/b` page · `g/G` top/bottom · `z` focus the card under the cursor (full-width, `z`/`Esc` to return) · `s` or the `≡` header control toggles the sidebar · `/` search sessions · `c` comment (anchored to the selected
lines on code and diff) · `r` rename the selected session · `d` delete the card under the
cursor, else the shown post · `D` delete the selected session (deletes wait for `y`) · `v`
older version · `o` open html in the browser · `:` commands (`open copy versions rerender
help`) · `q` quit. The sidebar lists sessions only; a session's posts are in the feed.

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

[`probe.rs`](src/probe.rs) queries terminal capabilities before the first frame, ending with
DA1 so stage stops waiting as soon as the terminal has answered.

## Security

Post data is untrusted. [`sanitize.rs`](src/sanitize.rs) is the only path from post data to
terminal cells: it drops ESC, C0 (except tab), DEL and C1, so a post can't move the cursor,
set the clipboard (OSC 52), open links (OSC 8) or start APC/DCS strings. It also caps the size
of every part and post.

## Debug and bench

| Variable | Effect |
|---|---|
| `STAGE_HELPER` | path to a different `stage-helper` |
| `STAGE_TRACE`, `STAGE_LOOP_TRACE`, `STAGE_HELPER_TRACE` | trace logs |
| `STAGE_HELPER_LOG` | helper stderr |
| `STAGE_LATENCY_LOG` | per-post paint times, for `scripts/stage-latency.sh` |

```sh
cargo test -p stage
cargo bench -p stage            # frame and visual budgets: benches/budget.txt
scripts/stage-latency.sh        # 100 posts, fails if publish→paint p95 ≥ 250 ms
```
