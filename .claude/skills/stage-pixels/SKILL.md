---
name: stage-pixels
description: Model-check stage's diagram pixel lifecycle (focus z, sidebar s, async rasters, zoom made ahead, parked feed pixels) with TLA+/TLC, so no order of events leaves a Mermaid card resampled, stale, doubled or leaking terminal memory. Use when changing crates/stage/src/gallery.rs (place, retarget, raster, show, handle, prefetch_zoom, park, replace_all) or the focus/visual handling in crates/stage/src/app.rs.
---

# stage-pixels: no order of events leaves a diagram on the wrong pixels

## The promise

Stage draws Mermaid diagrams as kitty images. Rasters finish on background threads in any
order relative to key presses and draws, and the gallery (`gallery.rs`) and the app
(`app.rs`) only agree on focus at each draw. The promise, as four rules:

| Rule | In plain words | Harm when broken |
|---|---|---|
| `OneToOne` | After every draw, each image showing for the card has exactly the pixel size the layout gives it. | kitty resamples it: blurry or squashed diagram, for a frame or for good. |
| `OneImage` | After every draw, the card shows at most one image. | Old pixels visible under or beside new ones. |
| `Crisp` | Once every raster has landed and been drawn, the card shows one image made for where it is now (zoomed when focused, else for the sidebar state). | Card stuck on a narrower or wider raster until something else happens. |
| `NoLeak` | After every draw, the terminal stores only pixels stage can still show. | Terminal memory grows with every resize, zoom and replacement. |

Tests check the orders someone wrote down; this checks every order within the bounds.

## Run

```sh
.claude/skills/stage-pixels/scripts/check.sh
```

Prints `PASS` or the violated rule and the shortest trace. It downloads TLA+ tools 1.8.0
once (sha256-pinned) to `~/.cache/notes-gpui/` and uses Homebrew's `openjdk`
(`brew install openjdk` if `java` is missing). About 5 s. From Claude Code, run it with the
sandbox disabled (the first run needs the network). Pass a spec path to check a mutated copy:
`scripts/check.sh /path/to/StagePixels.tla`.

Read every result, pass or fail, with the pstack `/statechart` skill: on a violation it
gives the shortest path to the bad state in plain words; on a pass it draws how the card
moves between states.

## Scope and bounds

- One Mermaid card, always on screen once it has pixels. Three pixel sizes: feed with the
  sidebar (`N`), feed without it (`W`), zoomed (`Z`), all different (the worst case; a small
  diagram capped at 2x has equal `N`/`W`).
- Events: `z`, `s` (no HTML card waiting), draws (only when one is due), and raster
  completions in any order. Bounds: 7 image ids, 2 jobs in flight per size
  (`model/StagePixels.cfg`). 79,042 distinct states at the sync commit.
- Zoom levels and panning (`ZoomView`) aren't modeled: every zoomed raster is `Z`. A
  change of view is a new zoom target, like the width changes the model does cover.
- Left out: HTML cards and the sidebar's wait for their frames, PNG/decoded images,
  terminal resizes (`replace_all`), the 128 MB eviction, several cards competing, the helper
  (SVG arrives once, at the start), cell-rect geometry within a frame.

## What each part stands for (coverage)

Sync commit: `a60df09` (the model describes the code at this commit). When any file below
changes after it, recheck that the model still describes the code; move the sync commit
forward only after you have.

| Model | Code |
|---|---|
| `RasterDone` prefetch branch | `Gallery::handle`, `Gfx::Raster` arm storing `self.prefetch`; app `Msg::ZoomReady` |
| `RasterDone` drop branch | `Gallery::handle`, arm `made_for != self.raster_target(&key)` (clears `rastered`, `zoom.made_for`) |
| `RasterDone` shown branch, `Shown`, `ShowFreed`, `Parked` | `Gallery::handle` accepted arm, `Gallery::park`, `Gallery::show`; app `Msg::VisualReady` (updates `unzoomed`), `Msg::ZoomShown` (focused or `left_focus`) |
| `PressZ` | `App::toggle_card_focus`, `App::enter_card_focus` (`unzoomed`, `left_focus`, `zoom_ahead`), `App::leave_focus` |
| `PressS` | `App::toggle_sidebar` with no visible HTML card |
| `Draw` retarget part | `Gallery::retarget` (prefetch take, unpark into `dead`) |
| `Draw` place part | `Gallery::place` (`ready_zoom` shown or resent, `raster()` for on-screen cards, the resize hold on `OnScreen.px`, previous delete, parked unplace, `dead` delete) |
| `Draw` after_draw part | `App::after_draw` → `App::zoom_ahead` → `Gallery::prefetch_zoom` (and its repeated `ZoomReady`) |
| `held`, `shown` | kitty: `kitty::transmit*` add, `kitty::delete` (`d=I`) frees, `kitty::unplace` hides |

One TLA+ action is one turn of `main.rs`'s loop doing that thing: a draw (`ui::draw`,
`retarget`, `place`, `after_draw`) is one action; a raster completion with the app's
reaction to its message is one action (both happen before the next draw).

## When the check fails

1. Read the trace with `/statechart`. Retell it as the steps a user takes (keys, rasters
   landing) and what the screen shows.
2. Decide: real bug or modeling error. Open the code the failing step stands for (table
   above) and walk the same steps through it. If the code does what the trace says, it's a
   real bug: report it with that sequence; fix the code only if asked. If the code doesn't,
   the model is wrong there: fix the model to match the code, never by weakening a rule.
3. Every fix to code that the model covers needs the matching edit in the model, then a
   passing run.

## Proof that it can fail

Each mutation below, applied to a copy of the model the way the regression would look in
the code, makes TLC report the rule shown (checked at the sync commit):

| Regression in code | Rule that fails |
|---|---|
| `Gallery::park` keeps the held `previous` | `OneToOne` |
| `kitty::delete` with `d=i` (only unplaces) | `NoLeak` |
| `place`'s resize hold ignores `OnScreen.px` (the old rect heuristic) | `OneToOne` |
| `App::leave_focus` doesn't restore the feed size | `OneToOne` |
| `App::enter_card_focus` ignores `left_focus` | `OneToOne` |

Writing this model found seven real orderings in the code that broke these rules (commit
`a60df09` fixes them), all rare races between a raster landing and `z`/`s` in the same loop
turn.

## Keeping it in step

`/maintain-formal-verification-skill` brings the model back in line after changes that
didn't recheck it.
