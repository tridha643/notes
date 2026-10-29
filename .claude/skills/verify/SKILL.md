---
name: verify
description: Drive stage (the Rust TUI that shows sideshow posts, with Mermaid/HTML/image cards over kitty graphics) the way a user does and prove what reached the screen — first visual latency cold and warm, 1:1 pixel geometry, keys like PageUp, z focus and s sidebar. Use after changing crates/stage or the Swift stage-helper, or to check a claim about stage's speed or pixels.
---

# verify: drive stage in a PTY and prove what's on screen

stage is a terminal app. A user sees it in Rex (kitty graphics). This skill runs the real
`stage` binary in a pseudo-terminal that answers stage's probes the way kitty does, presses
keys on a schedule, and records every kitty image placement stage sends. That stream is the
ground truth for "what's on screen": which image, where, how many cells, which source pixels.

Everything here talks to the real notesd (read-only, except the one-time fixture session) and
never to the user's running stages or their shared stage-helper.

## Launch

Build once per change; each drive then starts its own stage in its own PTY.

```sh
cargo build --release -p stage
swift build -c release --package-path apps/Notes --product stage-helper   # only if Swift changed (or first time)
.claude/skills/verify/scripts/fixture.py    # prints the fixture session id; creates it once
```

The fixture is a session titled `stage verify fixture`: 8 Mermaid posts (Diagrams 3 and 6
are taller than a screen), a static HTML post, and a last post with a Mermaid diagram and
markdown. stage opens a session on its last post.

From Claude Code, run the scripts with the sandbox disabled: the sandbox blocks connecting to
notesd's socket and creating POSIX shared memory (`EPERM`).

## Doctor

```sh
.claude/skills/verify/scripts/doctor.sh
```

Read-only. Checks that notesd answers, that the release `stage` is newer than every source in
`crates/stage`, that the release `stage-helper` sits next to its `Notes_NotesCore.bundle` (a
copied binary without it never renders Mermaid), that the fixture exists, and the load
average. Load above ~5 makes timings meaningless (a parallel `cargo`/`swift` build doubles them);
wait or say so next to any number you report.

## Drive

```sh
.claude/skills/verify/scripts/drive.py --session <id> [--mode cold|warm] [--runs N] \
    [--keys 'PageUp@1.0,z@1.6,Esc@2.4'] [--quit-at 3.0] [--size 120x40] [--cell 10x20]
```

- `--mode cold`: a fresh temp `HOME` per run, so stage launches its own helper (process start,
  pele warm-up and WebKit setup are all in the number). `--mode warm`: one temp `HOME` for every
  run; run 0 starts the helper and is flagged `"prewarm": true` — leave it out of warm numbers.
- `--keys`: `KEY@seconds` after launch. `KEY` is one character (`z`, `s`, `j`, `k`, `G`, `g`, `q`)
  or `PageUp`, `PageDown`, `Up`, `Down`, `Esc`, `Enter`. Never send `c`, `r`, `d` or `D`: those
  write comments, renames and deletes to the real notesd.
- stage quits on `q` at `--quit-at`; the script then TERMs and KILLs if it has to (`stopped_by`).

One JSON line per run on stdout:

- `first_placement_ms`: process start → first kitty placement (the first pixels a user sees).
- `rasters`: resvg jobs stage ran (Mermaid). Fewer is better when they're off screen.
- `keys[]`: per key, ms to the next placement (`first_placement_ms`) and to the next new image
  (`first_new_image_ms`; null when the key only moved existing images).
- `on_screen_at_end`: what kitty shows when `q` is sent, after every place and delete. Each has
  `cells`, `src` (x, y, w, h) and `one_to_one` (`src` w×h == cells × cell px, i.e. kitty draws
  it without resampling). This is the geometry proof, not `all_one_to_one` over everything sent:
  a stale image can sit on screen without being placed again.

## Evidence

Each run writes `target/verify/<mode>-<n>/`: `result.json` (everything above plus every
placement), `pty.bin` (the raw terminal stream), `stage-trace.log` (`STAGE_TRACE`: request →
pixels per visual, placements), `loop-trace.log`, `helper.log` (helper phase timings; in warm mode only run 0, which launched it) and
`stderr.log` (`resvg N ms` per raster). A later run with the same mode and index overwrites it,
so copy a run you're citing somewhere stable first.

Proof standards: drive the real binary through keys, never by calling app internals; cite
`on_screen_at_end` for pixel claims and `first_placement_ms` for latency, over several runs
with load noted; for a fix, show the failing run before and the passing run after.

## Cleanup

Automatic. Each run removes its temp `HOME` and kills the helper it launched, matched by that
`HOME`'s socket path, never by process name, so the user's stages and helper are untouched.
Evidence under `target/verify` stays. Check for strays with
`pgrep -fl '/T/sv.*Caches/stage'` (should print nothing). The fixture session stays for
the next run; `notes session delete <id>` removes it.

## Features

[features/README.md](features/README.md) maps what to drive for each user-facing feature.
