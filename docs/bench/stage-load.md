# Stage latency and load tests — 2026-10-30

18-core Apple Silicon, 48 GiB RAM. All tests use isolated temporary homes, sockets,
vaults and databases. No benchmark publishes to the user's real stage session.
Storage measurements and five alternating daemon A/B runs are in
[daemon-load.md](daemon-load.md).

## What is measured

- `scripts/stage-load.py`: real notesd, a PTY running stage, concurrent publishers,
  an independent watch client, and a VT replay of terminal output. Reports publish
  acknowledgement, event arrival, first row paint, body visibility, input latency,
  CPU and peak stage RSS. Answers terminal probes before sending keys.
- `scripts/visual-load.py`: helper protocol to SVG / actual first RGBA pixels, with
  1/4/16/32 in-flight operations. Checks dimensions, pixel colors, shared-memory
  size, errors, and final pixels after bursts of HTML updates. RSS is **helper only**,
  excluding WebKit XPC processes. This does not measure terminal decoding or display.
- `cargo bench -p stage --bench visuals`: gallery request through helper and raster
  workers to terminal escape output. `warm` repeats a diagram in one Gallery;
  `unique` changes the rendered labels to force parse misses. `restart` creates a
  new Gallery connected to the same helper. These are pipeline microbenchmarks,
  not a claim that pixels are already visible on a physical display.

The existing latency log marks a post row being added to the feed, not necessarily
its body appearing on screen. The PTY harness additionally checks body tokens and
walks through every post after the burst, so follow-mode skipping intermediate
posts is distinguished from losing their content.

## Text path

The stage worker alternated two baseline/new runs against the same daemon. It
removed repeated full post-list fetches for events already reflected in the list,
quadratic post-version lookups, and whole-feed rebuilding for append-only changes.
Append reuse checks the complete existing feed key and local calendar day.

| Scenario | Metric | Before | After stage changes |
|---|---|---|---|
| 1,000-post burst, 8 writers | row p95 | 4,057–4,425 ms | 1,226–1,228 ms |
| Same burst | stage CPU | 8.5–9.1 s | 0.90–0.94 s |
| 1,000 preloaded + 200 paced | row p95 | 30.6–32.4 ms | 12.3–16.1 ms |
| Same large feed | stage max RSS | 80–83 MB | 67 MB |
| Same large feed | page-through key p95 | 9.4–18.1 ms | 0.9–2.2 ms |

After integrating the daemon changes and message-drain budget, one final run reported:

| Scenario | Row p50 / p95 / p99 | Longest UI update turn | Correctness |
|---|---|---|---|
| Burst, 1,001 total posts | 385 / 549 / 576 ms | 380 ms | All bodies verified |
| Paced, 301 posts | 3.7 / 5.1 / 5.8 ms | 3.3 ms | All bodies verified |
| Large feed, 1,200 posts | 11.5 / 15.3 / 16.4 ms | 10.7 ms | All bodies verified |
| 200 revisions, 20 posts | 3.5 / 4.8 / 5.4 ms | 1.6 ms | Final revision verified |

Large-feed startup to the newest body fell to 125 ms in this combined run (the
stage-only A/B was 562–600 ms). This is not an isolated stage-only improvement:
the cheaper daemon reads account for much of it. The remaining burst stall is a
single synchronous UI-thread loading turn; do not describe its row latency as
sub-frame input responsiveness. A final 4 ms message-drain budget stops a continuous
stream from starving paint **between** messages, but cannot interrupt one long
synchronous message handler.

Input probes cancelled by the next key before paint are now counted as unanswered,
not accidentally matched to the response to a later probe. Load-test input percentiles
cover **answered probes only**, so also inspect their unanswered count and longest
UI turn; they are not proof that input remains sub-frame throughout a burst.

An asynchronous-request experiment was rejected: burst input appeared responsive,
but paced visible-body p50 regressed from 3.2 to 47.5–49 ms. Passing state-machine
tests and fast row markers were not sufficient to ship it. The accepted synchronous
path remains the default.

## Graphics path

The SVG parse cache is keyed by exact source, retains one tree, and preserves
pixel output at different sizes. Parsing misses occur outside the cache mutex.
Raster work now uses at most eight CPU workers rather than one thread per request,
coalesces superseded jobs by visual key, prioritizes focused jobs, and bounds
outstanding distinct visuals at 256 (explicit error on saturation). Owned replies
reclaim shared memory when obsolete, unread, undeliverable, or dropped at teardown.

| Pipeline microbenchmark | Before p50 | Combined p50 / p95 |
|---|---|---|
| Repeated Mermaid | 7.6 ms | 5.0 / 5.1 ms |
| New Mermaid source | ~7.8 ms | 7.8 / 11.4 ms |
| HTML, retained pages | 28.6 ms | 26.0 / 27.7 ms |
| HTML, new Gallery on warm helper | 29.0 ms | 5.2 / 27.5 ms |

The HTML improvement reuses **closed** WebViews and windows. It does not make every
newly retained page render in 5 ms. Recycled views clear scripts, navigate to a blank
document, and reject callbacks from obsolete navigations. A separate non-persistent
website data store per view is cleared asynchronously **before** reuse, so session
storage cannot leak into the next document or be erased from another live page.
Blank navigation alone failed a same-URL session-storage regression probe.

The final pool bounds ready + resetting views at **32**, allocated on demand, not
preallocated. The initial four-slot pool was fast but, after adding proper storage
isolation, failed the 512-request/16-in-flight stress test at 382 completions. With
32 slots, 512 requests each at concurrency 1/16/32 and 1,000 requests at concurrency
32 all completed with exact pixels and no errors. This larger bound is justified
by that correctness/stability test, not just a noisy small-sample median.

### Mixed open/close workload

128 requests per scenario, half distinct diagrams and half HTML pages with unique
known colors/sizes. Baseline was separately built from the unchanged Swift source;
final runs include the 32-slot pool, storage isolation and update correctness fixes.

| In flight | Before requests/s | After requests/s | HTML p50 before → after | HTML p95 before → after |
|---|---|---|---|---|
| 1 | 62.7 | 221.9 | 27.2 → 4.1 ms | 29.0 → 5.7 ms |
| 4 | 79.5 | 358.2 | 104.3 → 8.1 ms | 142.8 → 142.2 ms |
| 16 | 83.2 | 152.3 | 378.6 → 269.1 ms | 524.7 → 374.7 ms |

Every request and pixel check passed. Cold WebKit startup remains in these samples;
its tail dominates the small four-in-flight sample, so that p95 did **not** materially
improve. In a longer 512-request/16-in-flight run, final HTML p50 was 57.1 ms and
throughput 322.5 requests/s; do not substitute that warm-heavy number into the
128-request A/B comparison. Helper-only RSS in the final 128-request matrix was
76–143 MiB versus 164–169 MiB in the baseline; this excludes WebKit child processes.

The original helper stalled twice in a 256-request sequential run, completing 180
requests on one captured failure. The final recycled version also completed the
larger tests described above. Treat those as stress/correctness evidence, not clean
comparative timing. At 32 in flight, tails can still approach or exceed a second.

### Revision correctness

The baseline failed a burst of 256 mixed style patches and script-changing reloads:
the final pixels belonged to an older revision. The new serialized update path
returned all 256 replies and the correct final pixels in 198 ms in the final run,
without emitting intermediate stale frames. Regression tests also cover pause/resume
without an ack, recycled-page globals and session-storage isolation, preservation
of another live page's storage, and recovery after navigation failure.

## Reproduce

```sh
cargo build --release -p notesd -p notes-cli -p stage
(cd apps/Notes && swift build -c release --product stage-helper)
python3 scripts/stage-load.py --only burst,large-1000,patch --out /tmp/stage-load.json
python3 scripts/visual-load.py --requests 128 --concurrency 1 4 16 --updates 256
python3 scripts/visual-load.py --requests 512 --concurrency 16 32
cargo bench -p stage --bench visuals
cargo bench -p stage --bench frames
scripts/stage-latency.sh
```

Rust build/tests and strict stage all-target Clippy pass at this checkpoint. The frame benchmark reports input p50/p95 **0.7/1.2 ms**, resize **5.2 ms**,
one frame per 240-key paste, and **zero idle wakeups** over three seconds. Five
new Swift regression tests pass. Full Swift testing on this machine needs explicit
CommandLineTools framework/linker paths; 58 of 59 tests pass. Its pre-existing
`animatingPageStreamsFramesWhileAcked` test fails (one frame instead of >=20 fps).
The same failure reproduces with the installed, unchanged helper. Snapshot and
window-placement experiments did not fix it and were reverted; no test was disabled
or weakened to report a green suite. Telemetry finds one visible, unoccluded AppKit
window but a WebKit document marked hidden: CSS animation time remains zero and
requestAnimationFrame callbacks do not run. Interval-driven DOM changes do render.
Changing activation policy or using transparent content layers did not fix it.
This remains an open rendering-quality issue.

Other limits: resident HTML documents in a long-running Gallery are not yet bounded
(the spare pool only bounds **closed** views); paused capture does not stop arbitrary
JavaScript in an offscreen page. These require a separate page-lifecycle change,
not a claim that unlimited RAM or worker threads solves them.
