# Sidebar toggle (`s`)

`s` (or clicking `≡`) hides or shows the session sidebar; cards re-flow to the new width.
Visible HTML cards switch together once each has a frame at the new width.

## Sub-features

- Hide with Mermaid on screen (re-rasterized at the new width).
- Hide with HTML on screen (old pixels held until new-width frames arrive).
- Show again.

## How to get to it (user POV)

Press `s`, or click the `≡` at the top left.

## Driving it with drive.py

```sh
drive.py --session <fixture> --mode warm --runs 3 --keys 's@1.0,s@2.0' --quit-at 3.0
drive.py --session <fixture> --mode warm --runs 3 --keys 'k@0.8,s@1.2,s@2.2' --quit-at 3.2   # HTML on screen
```

Proof: `on_screen_at_end` entries are `one_to_one: true` and at the normal width again.

## Gotchas

- Keys sent in one burst are handled in one batch before stage draws, so space keys at least
  ~100 ms apart unless you're testing batching on purpose.
