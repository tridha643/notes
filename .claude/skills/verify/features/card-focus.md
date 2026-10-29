# Card focus (`z`, `Esc`)

`z` shows the card under the cursor alone, full width, without the sidebar; `z` or `Esc`
returns to the feed where it was. A focused Mermaid diagram is re-rasterized at the full
width (shown 1:1 meanwhile, never upscaled); an HTML card waits for a full-width frame.

## Sub-features

- Focus a Mermaid card; focus an HTML card; leave focus with `z` and with `Esc`.
- Leaving focus restores crisp, 1:1 normal-width pixels (regression: they used to stay stale).

## How to get to it (user POV)

Move the cursor onto a card (`j`/`k` post, arrows), press `z`.

## Driving it with drive.py

```sh
# The fixture's HTML card is one post above the last: k, then z in and z out.
drive.py --session <fixture> --mode warm --runs 3 --keys 'k@0.9,z@1.3,z@2.3' --quit-at 3.4
```

Proof: in `on_screen_at_end`, the HTML card is back at its normal width (84 cells at 120x40)
with `src` w == 840 and `one_to_one: true`. Before the fix, it showed `src` w 1150 in 84 cells.

## Gotchas

- Focusing a small diagram changes nothing visible: rasters are capped at 2x the diagram's
  own size, so full width doesn't make them bigger. Use a wide card to see focus at work.
