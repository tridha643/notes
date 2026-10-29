# Card focus (`z`, `Esc`)

`z` shows the card under the cursor alone, full width, without the sidebar; `z` or `Esc`
returns to the feed where it was. A focused Mermaid diagram is zoomed: re-rasterized as large
as the pane allows in both directions (nothing to scroll) at device pixels, made ahead while
the cursor rests on it so `z` shows it in its first frame. Leaving focus puts the feed-size pixels back in the same frame. An HTML
card waits for a full-width frame.

## Sub-features

- Focus a Mermaid card; focus an HTML card; leave focus with `z` and with `Esc`.
- Leaving focus restores crisp, 1:1 normal-width pixels (regression: they used to stay stale).
- Zoom fills the width even for a small diagram (regression: it stopped at 2x the diagram's size).
- No in-between frame: every placement after `z` and after `z` back is `one_to_one`.

## How to get to it (user POV)

Move the cursor onto a card (`j`/`k` post, arrows), press `z`.

## Driving it with drive.py

```sh
# The fixture's HTML card is one post above the last: k, then z in and z out.
drive.py --session <fixture> --mode warm --runs 3 --keys 'k@0.9,z@1.3,z@2.3' --quit-at 3.4
```

Proof: in `on_screen_at_end`, the HTML card is back at its normal width (84 cells at 120x40)
with `src` w == 840 and `one_to_one: true`. Before the fix, it showed `src` w 1150 in 84 cells.

Zoom (the last post's diagram is under the cursor at open):

```sh
drive.py --session <fixture> --mode warm --runs 4 --keys 'z@1.0,z@1.8' --quit-at 2.8
```

Proof, from `target/verify/warm-N/result.json` `placements`: after the first `z`, one image
whose `src` is the whole raster (no crop) with `one_to_one: true` within ~30 ms; after the
second `z`, the original image id back at its feed size within ~30 ms, and no placement in
between with `one_to_one: false`.

Zoom and pan a heavy diagram (the "stage verify heavy" session from `heavy.py`; its 80-state
statechart is two posts above the last):

```sh
drive.py --session <heavy> --mode warm --runs 3 --keys 'k@1.0,k@1.5,z@2.2,+@3.0,+@3.6,+@4.2,l@4.8,l@5.4,j@6.0,0@6.6,z@7.2' --quit-at 8.0
```

Proof: each `+` places a wider 1:1 image (fit → 1.5x → 2.25x → pane-wide), each pan a 1:1
image of the same size within ~30 ms, `0` the fitted one again. Reference (load ~10): zoom
steps 32–40 ms, pans 21–26 ms, window rasters 5–11 ms (`stderr.log`).

## Gotchas

- `on_screen_at_end` only shows the end; jitter shows up in `placements` (look for a
  non-1:1 placement between keys).
