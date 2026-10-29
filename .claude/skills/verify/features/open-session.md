# Opening a session: first visuals on screen

stage opens on a session's last post and draws its visual cards (Mermaid rasterized by stage
from the helper's SVG, HTML as live frames from the helper, images as PNG or decoded pixels).
Only visuals on screen are requested; the nearest one above and below follow once those land.

## Sub-features

- Cold start: no helper running yet (first stage after login, or after the helper idled out).
- Warm start: a helper already running (every later stage).
- Pixel geometry: every visual is drawn 1:1, whole cells, no kitty resampling.

## How to get to it (user POV)

`stage --session <id>`, or pick the session in the sidebar.

## Driving it with drive.py

```sh
drive.py --session <fixture> --mode cold --runs 5 --quit-at 2.0
drive.py --session <fixture> --mode warm --runs 6 --quit-at 2.0   # drop run 0 (prewarm)
```

Proof: `first_placement_ms` per run (state the load), `rasters` ≤ on-screen visuals + 2, and
every entry of `on_screen_at_end` has `one_to_one: true`. `memory_at_quit` gives stage's and
the helper's RSS and the image bytes the terminal still holds (freed only by `d=I`/`d=A`).

Memory reference (warm, after paging the fixture, a z round trip and three session switches):
stage 9 MB RSS, helper 83–90 MB, terminal 10 images / 16 MB.

Reference (release, 120x40 at 10x20 px, M-series, load < 5, 2026-10-28): warm 50–64 ms;
cold 109–153 ms with the helper that answers Mermaid off its main thread.

## Gotchas

- Run 0 of a cold batch right after a build is slower (page cache); use several runs.
- HTML frames are as tall as their content, so an HTML card's height isn't a whole number of
  cells and its `one_to_one` can be false; that's the HTML path, not a Mermaid regression.
- Load from a parallel build inflates cold numbers 2–4x.
