# Stage scroll latency in Rex

## Operation and comparison

Measured on the Apple Silicon macOS host running Rex, using release Stage builds,
a private notesd, generated posts, and a PTY that drains terminal output. Baseline:
`bef8aaf` (the resize-correctness fix). Candidate: this patch. No installed binaries,
shared services, or foreground app selection changed.

The matched cohort starts at the first markdown post at 120×40 with the sidebar
visible, then sends 30 separate wheel ticks. Each tick moves three rows. Latency is
input to the last terminal output byte before a quiet interval, **not GPU presentation
or remote-client latency**. Frames include notesd work where applicable. The separate
Rex A/B test compares styled captures against `STAGE_NO_SCROLL_REGION=1`.

## Cause

Stage waited for more input before drawing every wheel/navigation event. The requested
8 ms `recv_timeout` sometimes lasted 50–62 ms in loop traces; a standalone timeout
probe reproduced late wakes outside Stage, including in a background Rex pane.
Drawing itself was typically a few milliseconds. Waiting for hypothetical future input
was on the critical path of input already received.

With the sidebar visible, Stage also disabled native row moves and rewrote the text
column. Rex supports horizontal margins, allowing only the feed rectangle to scroll
while the sidebar, title, and footer remain stationary.

## What Hunk does differently

Inspected `modem-dev/hunk` through GitHub, tree
`3368209089b06e06d9816bce54a81aa1db313555`. Hunk isn't installed on this host, so this
is a source comparison, not a runtime claim that Stage now beats Hunk.

- `packages/hunk/src/ui/components/panes/DiffPane.tsx` moves its OpenTUI scroll box
  independently of timer-deferred React viewport bookkeeping. Stage instead waited
  before its only presentation path.
- `packages/hunk/src/ui/lib/fileRenderWindow.ts` mounts a sparse visible window with
  overscan; `adaptiveScrollOverscan.ts` expands that window during rapid movement.
  Stage already caches layout and draws visible lines, so replacing its layout system
  wasn't needed to remove the measured wait.
- `packages/hunk/src/ui/lib/scrollAcceleration.ts` uses cadence-based acceleration
  capped at 3×. Stage's desktop wheel step is already three rows. This patch preserves
  wheel distances, reversals, and one-row narrow-pane behavior rather than masking
  latency by skipping more content.

## Results

Two completed baseline runs and three candidate runs, alternating builds:

| Warm matched sidebar scroll | Baseline | Candidate |
| --- | ---: | ---: |
| Output p95, 30 ticks per run | 72.50–72.79 ms | 1.00–1.02 ms |
| Terminal output per tick | 3.17 KiB | 0.64 KiB |
| Isolated resize output p95, 20/run | 1.46–4.34 ms | 4.11–4.54 ms |
| Idle output / wakeups, 3 seconds | 0 / 0 | 0 / 0 |

A third baseline attempt failed because a scroll produced no output within the
100 ms observation interval; it is a failed run, not a successful latency sample.
The candidate with native scrolling disabled measured 0.83 ms p95 and 3.16 KiB/tick,
separating removal of the wait from the roughly 80% byte reduction.

The resize-burst generator aims at 240 Hz but recorded 21–31 ms maximum scheduling
lateness, so its frame counts are not evidence of a precise frame-rate cap. Isolated
resize latency did not show a consistent improvement or a need for added debounce.
Same-size resize invalidation now resets reusable buffers instead of reallocating them.

## Guards and limitations

`cargo bench -p stage --bench frames` checks a dedicated wheel-latency budget, bounded
frames for a queued 120-tick burst, resize latency, and zero idle output/wakeups. Use
`STAGE_BENCH_BIN=/absolute/path/stage` to compare another build on the same fixture.
The benchmark disables the Swift helper and isolates its session directory explicitly.

`python3 scripts/stage-scroll-e2e.py REX_SESSION` checks 46 styled screens, including
wide-pane bursts and reversals. Wide-pane mouse coordinates now hit the feed rather
than accidentally hitting the sidebar. `stage-resize-e2e.py` still verifies coalesced
resize recovery and the PNG retransmission/placement path at three sizes.

Native rectangular scrolling is enabled only for Rex; other terminals retain the
full-width-only path. Graphics and resize transitions remain excluded from row moves.
Queued inputs still coalesce within a 4 ms work budget, but Stage no longer deliberately
waits before drawing or imposes an 8 ms presentation cap. Sustained event streams can
produce more frames; cheaper row moves reduce their output cost. Actual GPU/remote
presentation, live HTML scrolling, and very large sessions remain unmeasured.
