# Scrolling to visuals that weren't on screen

Off-screen visuals aren't rendered at open; the nearest one above and below are fetched once
the visible ones land, and the rest as they come on screen.

## Sub-features

- Paging up to an already-prefetched neighbor (should appear at once).
- Paging further, to a visual that was never requested (renders on arrival).

## How to get to it (user POV)

`PageUp`/`PageDown`, `space`/`b`, arrows, mouse wheel; `g`/`G` jump to the top/bottom.

## Driving it with drive.py

```sh
drive.py --session <fixture> --mode warm --runs 3 --keys 'PageUp@1.0,PageUp@1.4,PageUp@1.8' --quit-at 3.0
```

Proof: `keys[].first_placement_ms` is small (tens of ms) for a prefetched neighbor, and
`on_screen_at_end` is all `one_to_one`.

## Gotchas

- stage opens at the bottom (the last post), so `PageDown` first does nothing; page up.
- A key that only moves already-placed images has `first_new_image_ms: null`; that's expected.
