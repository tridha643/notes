# fff-search go/no-go — milestone 2, step 1

**NO-GO for embedding the unmodified crates.io `fff-search = "=0.11.0"` in `notesd`.** Path search and ordinary content hits are fast and the external watcher works, but its idle timer violates the project's zero-wakeups rule. The unique-identifier probe and plain-miss-plus-fuzzy retries also exceed the CLI spec's ≤10 ms warm-call target before IPC or CLI startup is added. This rejects this version/configuration as shipped; it doesn't establish that another search engine would be faster.

## Setup and measurement boundaries

Measured locally on 2026-10-18, Apple M5 (10 logical CPUs, 24 GiB), macOS 26.6.2 (25G83), `rustc 1.99.1`, `aarch64-apple-darwin`. The spike uses Cargo's default release profile, the default `ripgrep` walker feature, the system allocator, `FFFMode::Ai`, content indexing and mmap warmup enabled, and the library's automatic cache budget. `zlob`, mimalloc and nightly fff builds were not tested. Version `0.11.0` was the latest stable version in the fetched crates.io index; Cargo also advertised a newer nightly.

The Python driver generates 10,000 Markdown files in a temporary vault, with five top-level folders and nine children each (50 directories, 45 leaf folders). Seed 249 produces 108,509,770 bytes (103.48 MiB), 1,025–20,480 bytes per note. Bodies contain headings, paragraphs, lists, wiki links, week numbers, a unique note identifier, and a mixture of 20 work/personal/research topics. This is synthetic data with repeated vocabulary, so common hits deliberately exercise early pagination while a unique identifier exercises selective search. Temporary vaults and LMDB state are removed after each run; no personal notes are read.

Each recorded run starts a fresh resident Rust process, empty fff index and empty frecency/query databases. Corpus generation runs in the separate driver before `/usr/bin/time` starts, so it doesn't inflate the search process's peak RSS. These are **cold process/index scans with warm OS file caches**, immediately after generating the files; no disk-cache eviction was performed. Scan timings start just before `FilePicker::new_with_shared_state`; the separate exec-to-ready figure includes launch/loader/pipe overhead. The library's startup wait helpers poll every 10 ms, so scan/index timings have that granularity plus scheduling delay.

All 40 query cases (20 find, 20 search) run in the same process. Each gets two excluded warmups, then 50 timed samples, with rotating query order: 1,000 samples per verb per run. The timer includes query parsing, acquiring the shared read lock, fff execution and copying up to 50 returned paths or match lines into an owned JSON value. It excludes JSON serialization, stdin/stdout transport, CLI startup and UI work. Grep uses smart case, automatic regex detection, a first page of 50 matches, no context and no time budget. On zero matches the spike explicitly retries in fuzzy mode; both calls are included in the timing. These are first-page latencies, not exhaustive retrieval times or a notes CLI benchmark.

Two final runs were sequential, with no build or profiler launched alongside their query phases. This was a working developer Mac, not an isolated benchmark host: `top` records system load, compressed memory and swap activity; its load averages can include the benchmark's preceding burst. An earlier diagnostic overlapped compilation and is excluded from the committed measurements. The complete final samples, correctness checks and OS output (with trailing whitespace removed) are in [results](../spikes/fff-check/results/).

## Cold scan and warm query results

| Boundary (ms) | Run 1 | Run 2 |
|---|---|---|
| Initial directory/file scan | 61.256 | 45.920 |
| Scan + content index + mmap warmup (cumulative) | 237.009 | 154.008 |
| Initialization through watcher subscription (includes LMDB setup) | 270.175 | 186.056 |
| Driver launch through ready reply | 565.938 | 199.709 |

| Verb | Run | Samples | p50 ms | p95 ms | max ms |
|---|---|---|---|---|---|
| find | 1 | 1000 | 0.391 | 0.861 | 1.670 |
| search | 1 | 1000 | 0.483 | 106.233 | 199.035 |
| find | 2 | 1000 | 0.386 | 0.654 | 1.866 |
| search | 2 | 1000 | 0.568 | 89.622 | 144.605 |

Pooled percentiles use all 1,000 compatible raw samples for each verb, with equal weight per query and the nearest-rank definition. They are not averages of per-query percentiles. Stored result examples are truncated to three paths/matches after checking the full returned page; all timing samples are retained. The two runs remain separate because host conditions differ.

| Content cohort | Run | Samples | p50 ms | p95 ms | max ms |
|---|---|---|---|---|---|
| Common plain/regex content hits | 1 | 850 | 0.391 | 0.735 | 2.495 |
| Shared-prefix unique identifier hit | 1 | 50 | 34.035 | 47.860 | 67.873 |
| Plain miss + fuzzy typo hit | 1 | 50 | 65.504 | 94.016 | 112.663 |
| Plain miss + fuzzy miss | 1 | 50 | 112.050 | 163.935 | 199.035 |
| Common plain/regex content hits | 2 | 850 | 0.543 | 0.775 | 3.917 |
| Shared-prefix unique identifier hit | 2 | 50 | 33.822 | 48.184 | 52.637 |
| Plain miss + fuzzy typo hit | 2 | 50 | 63.915 | 84.520 | 89.622 |
| Plain miss + fuzzy miss | 2 | 50 | 114.651 | 137.870 | 144.605 |

The unique identifier is present in exactly one file, but its prefix is shared by all 10,000 notes and its digits also occur in wiki links. It stresses this identifier pattern, not rare-word search in general; no general selective-search conclusion follows from this one case. Common phrases often hit the page limit early. The typo `fermenttaion` has no literal match and must find `fermentation`; the impossible query scans the fuzzy fallback without finding a result. Those cases explain why a low median doesn't satisfy the warm latency target. Both two-character path queries and both two-character content queries return relevant matches, and both path typos find the intended title family.

Per-query timings below are from run 1 (50 samples each); run 2 retains its complete per-query table and samples in [queries.json](../spikes/fff-check/results/run-2/queries.json). All times are milliseconds.

### find

| Query | p50 | p95 | max | Execution |
|---|---|---|---|---|
| `database-migration` | 0.450 | 0.879 | 1.020 |  |
| `databse-migrtion` | 0.400 | 0.668 | 0.851 |  |
| `meeting-notes` | 0.383 | 0.872 | 1.138 |  |
| `meting-notes` | 0.390 | 0.877 | 1.209 |  |
| `release-plan` | 0.368 | 0.882 | 1.160 |  |
| `research-paper` | 0.372 | 0.879 | 1.223 |  |
| `travel-itinerary` | 0.409 | 1.333 | 1.670 |  |
| `recipe-sourdough` | 0.455 | 1.169 | 1.361 |  |
| `project-orion` | 0.635 | 1.256 | 1.462 |  |
| `weekly-review` | 0.301 | 0.661 | 1.373 |  |
| `api-design` | 0.225 | 0.612 | 0.661 |  |
| `db-backup` | 0.149 | 0.299 | 0.454 |  |
| `performance-budget` | 0.490 | 1.024 | 1.116 |  |
| `book-highlights` | 0.282 | 0.552 | 0.766 |  |
| `garden-log` | 0.180 | 0.381 | 0.541 |  |
| `invoice-audit` | 0.323 | 0.695 | 0.845 |  |
| `macos-watcher` | 0.313 | 0.675 | 0.915 |  |
| `ui` | 0.577 | 1.174 | 1.541 |  |
| `db` | 0.468 | 0.986 | 1.148 |  |
| `zzzzunfindable999` | 0.150 | 0.323 | 0.473 |  |

### search

| Query | p50 | p95 | max | Execution |
|---|---|---|---|---|
| `database migration` | 0.588 | 1.013 | 2.495 |  |
| `meeting agenda` | 0.518 | 0.821 | 0.874 |  |
| `release checklist` | 0.542 | 0.761 | 1.376 |  |
| `research findings` | 0.238 | 0.354 | 0.421 |  |
| `train reservation` | 0.513 | 0.774 | 0.845 |  |
| `bread fermentation` | 0.236 | 0.348 | 0.394 |  |
| `Orion launch` | 0.234 | 0.334 | 0.442 |  |
| `weekly retrospective` | 0.232 | 0.337 | 0.526 |  |
| `API contract` | 0.515 | 0.898 | 1.236 |  |
| `backup recovery` | 0.234 | 0.391 | 0.575 |  |
| `latency budget` | 0.236 | 0.455 | 0.765 |  |
| `noteidentity09999` | 34.035 | 47.860 | 67.873 |  |
| `seed germination` | 0.239 | 0.356 | 0.922 |  |
| `invoice reconciliation` | 0.528 | 0.835 | 1.692 |  |
| `Observation\s+[0-9]+` | 0.338 | 0.480 | 0.655 |  |
| `borrow checker` | 0.508 | 0.711 | 1.668 |  |
| `fermenttaion` | 65.504 | 94.016 | 112.663 | fuzzy retry |
| `UI` | 0.526 | 0.735 | 0.787 |  |
| `db` | 0.236 | 0.313 | 0.412 |  |
| `zzzzunfindable999` | 112.050 | 163.935 | 199.035 | fuzzy retry |

## External watcher correctness and visibility

The driver, a different OS process from the resident picker, performs five trials of each operation per run:

- Create a new Markdown file and require its exact relative path in fuzzy results plus its sentinel in content results.
- Atomically replace a pre-existing, indexed note with a same-size edit after priming a content search on it. Require the new sentinel and the disappearance of the old sentinel.
- Delete the newly created note and require its exact path and both sentinels to disappear.

The resident process also subscribes with `SharedFilePicker::watch` and must receive `created`, `modified` and `removed` callbacks for the relevant path. There are no explicit rescans, refreshes or `handle_create_or_modify` calls. All 40 query checks and all 15 mutation checks passed in each run, including callbacks. The original indexed file count was exactly 10,000 in each run.

| Run | Mutation | Observation | p50 ms | p95 ms | max ms |
|---|---|---|---|---|---|
| 1 | create | path visibility | 95.145 | 194.014 | 194.014 |
| 1 | create | content visibility | 95.515 | 194.361 | 194.361 |
| 1 | create | watch callback observed | 82.987 | 181.031 | 181.031 |
| 1 | edit | path visibility | 1.121 | 2.211 | 2.211 |
| 1 | edit | content visibility | 1.494 | 2.655 | 2.655 |
| 1 | edit | watch callback observed | 83.360 | 84.900 | 84.900 |
| 1 | delete | path visibility | 90.916 | 207.631 | 207.631 |
| 1 | delete | content visibility | 0.982 | 1.037 | 1.037 |
| 1 | delete | watch callback observed | 274.451 | 300.855 | 300.855 |
| 2 | create | path visibility | 112.704 | 215.749 | 215.749 |
| 2 | create | content visibility | 113.128 | 216.574 | 216.574 |
| 2 | create | watch callback observed | 99.676 | 202.450 | 202.450 |
| 2 | edit | path visibility | 1.393 | 1.918 | 1.918 |
| 2 | edit | content visibility | 2.036 | 2.740 | 2.740 |
| 2 | edit | watch callback observed | 82.007 | 87.958 | 87.958 |
| 2 | delete | path visibility | 91.386 | 96.445 | 96.445 |
| 2 | delete | content visibility | 1.213 | 3.970 | 3.970 |
| 2 | delete | watch callback observed | 336.523 | 387.525 | 387.525 |

Visibility is measured from immediately before the external filesystem operation through the first matching search reply; callback time is through the first driver reply that contains the notification. These are observed upper bounds, including file I/O, pipe round trips, search time and 10 ms sleeps between probes. Probes check exact returned paths, never just whether fuzzy search returned something. A missing scope can cause fff to fall back to a broader grep, so probe cost can exceed the sleep interval; the table isn't a pure FSEvents latency measurement. Five samples per action give descriptive percentiles, not a stable tail estimate.

For edits the path remains present, so its near-zero path time isn't evidence of edit propagation. Live grep can see replacement/deletion bytes before the debounced callback arrives; the separate callback requirement is what verifies the watcher actually processed the operation. The default macOS backend is recursive FSEvents, not kqueue. That distinction matters if `notesd` intends to share a single kqueue-driven vault watcher.

## Peak RSS and indexed-idle cost

`/usr/bin/time -l` wraps the release binary for its full lifetime (startup, warm queries, watcher tests, idle window and clean shutdown). After mutations settle for three seconds, the driver runs exactly `top -l 2 -stats pid,cpu,idlew -pid PID`, then a second capture with `-s 30`. During these captures the Rust main thread blocks on stdin: no harness requests, searches or polling. Darwin `proc_pid_rusage(RUSAGE_INFO_V2)` snapshots bracket the longer capture to quantify CPU and wakeups even when `top` rounds CPU to zero. CPU tick differences are converted with `mach_timebase_info` (125/3 ns per tick on this M5), rather than treating the raw counters as nanoseconds.

| Metric | Run 1 | Run 2 |
|---|---|---|
| Peak RSS (`time -l`) | 96,550,912 bytes | 96,206,848 bytes |
| Idle RSS at end (`proc_pid_rusage`) | 96,501,760 bytes | 96,108,544 bytes |
| Idle physical footprint at end | 16,892,408 bytes | 13,287,928 bytes |
| Idle observation interval | 30.706 s | 30.687 s |
| Idle CPU time / one-core utilization | 21.287 ms / 0.0693% | 28.047 ms / 0.0914% |
| Idle interrupt wakeups / rate | 1142 / 37.19/s | 1143 / 37.25/s |
| Idle package-idle wakeups / rate | 193 / 6.29/s | 162 / 5.28/s |
| `top -s 30` second sample: %CPU, cumulative IDLEW | 0.0, 238+ | 0.0, 190+ |

`top`'s `IDLEW` column is cumulative; subtract its snapshots rather than treating the last number as a per-second rate. `proc_pid_rusage` package-idle wakeups and interrupt wakeups are distinct counters, and neither is a count of all context switches. Peak RSS, idle resident bytes and physical footprint are also distinct: file-backed mappings can contribute to RSS without representing private physical footprint. The raw `time.txt` files additionally retain peak footprint and process context-switch counts.

The idle-wakeup mechanism is visible in the exact dependency sources: fff sets a 50 ms debounce timeout and explicitly passes `Some(DEBOUNCE_TIMEOUT / 2)`, selecting a **25 ms tick**. `fff-notify-debouncer-full 0.9.4` unconditionally calls `std::thread::sleep(tick)` in its debounce loop and checks queues after each wake, even when no events exist. That's a nominal 40 timer activations/second; the measured interrupt-wakeup rate is consistent with that timer, while package-idle wakeups depend on whether the rest of the Mac permits the package to sleep. The unconditional timer itself violates the rule regardless of how many measured wakeups it caused. No watcher-disabled control was run, so these counters do not isolate its contribution from every other thread. Low average CPU doesn't make this event-driven. Sources: [fff watcher](https://docs.rs/fff-search/0.11.0/src/fff_search/watcher/background_watcher.rs.html), [debouncer](https://docs.rs/fff-notify-debouncer-full/0.9.4/fff_notify_debouncer_full/).

The full-lifetime CPU cost is substantial despite low idle utilization. Run 1: 51.39 s wall, 6.69 s user + 47.30 s system CPU, 311,818 involuntary context switches. Run 2: 50.97 s wall, 6.72 s user + 46.95 s system CPU, 293,046 involuntary context switches. CPU seconds sum across threads and can exceed wall time. Almost all CPU precedes the idle snapshot, but the existing captures do not separate indexing, benchmark queries and watcher probes, so no per-phase CPU or kernel-cost attribution is established.

For the USE check, utilization is measured by the idle CPU/RSS/I/O figures above; host load and compression are retained in `top`, but per-query scheduling/lock saturation wasn't profiled, so no particular cause is claimed for the slow searches. Error checks include successful indexing, expected positive/negative queries, old-content disappearance, actual watcher callbacks and successful OS captures. There were no benchmark correctness failures. During each idle snapshot window the target's disk-byte counters and page-in count did not increase.

## Exact library calls used

The executable implementation is [src/main.rs](../spikes/fff-check/src/main.rs); the following is its integration sequence with error/JSON handling omitted. All APIs are from [fff-search 0.11.0](https://docs.rs/fff-search/0.11.0/fff_search/).

```rust
use fff_search::{
    FilePicker, FilePickerOptions, FFFMode, SharedFilePicker, SharedFrecency,
    FrecencyTracker, QueryTracker, QueryParser, GrepConfig, Constraint,
    FuzzySearchOptions, PaginationArgs, GrepSearchOptions, GrepMode, Casing,
    WatchOptions,
};
use std::time::Duration;

let shared = SharedFilePicker::default();
let frecency = SharedFrecency::default();
frecency.init(FrecencyTracker::open(state.join("frecency"))?)?;
let tracker = QueryTracker::open(state.join("queries"))?;
FilePicker::new_with_shared_state(
    shared.clone(), frecency.clone(),
    FilePickerOptions {
        base_path: vault.to_string_lossy().into_owned(),
        enable_mmap_cache: true,
        enable_content_indexing: true,
        mode: FFFMode::Ai,
        watch: true,
        ..Default::default()
    },
)?;
assert!(shared.wait_for_scan(Duration::from_secs(60)));
assert!(shared.wait_for_indexing_complete(Duration::from_secs(60)));
assert!(shared.wait_for_watcher(Duration::from_secs(60)));
let count = shared.read()?.as_ref().unwrap().live_file_count();
```

```rust
// Find: hold the read guard until borrowed file results have been copied.
let guard = shared.read()?;
let picker = guard.as_ref().unwrap();
let parser = QueryParser::default();
let query = parser.parse(text);
let result = picker.fuzzy_search(&query, Some(&tracker), FuzzySearchOptions {
    pagination: PaginationArgs { offset: 0, limit: 50 },
    ..Default::default()
});
let paths: Vec<_> = result.items.iter().map(|f| f.relative_path(picker)).collect();

// Search: regex detection and zero-match fuzzy retry are caller policy.
let parser = QueryParser::new(GrepConfig);
let mut query = parser.parse(text);
// Watcher probes only: query.constraints.push(Constraint::FilePath(relative_path));
let mut options = GrepSearchOptions {
    page_limit: 50,
    casing: Some(Casing::Smart),
    mode: if fff_search::has_regex_metacharacters(text) {
        GrepMode::Regex
    } else {
        GrepMode::PlainText
    },
    time_budget_ms: 0,
    ..Default::default()
};
let mut result = picker.grep(&query, &options);
if result.matches.is_empty() { // enabled for benchmark searches, disabled for watcher probes
    options.mode = GrepMode::Fuzzy;
    result = picker.grep(&query, &options);
}
for matched in &result.matches {
    let path = result.files[matched.file_index].relative_path(picker);
    let line = matched.line_number; // 1-based
    let text = &matched.line_content;
}
// Also inspect regex_fallback_error and next_file_offset; the spike records both.
// Drop the read guard before waiting for filesystem events or requesting shutdown.
```

```rust
let id = shared.watch("", WatchOptions::default(), move |_, events| {
    // The spike copies event.path and event.kind.as_str() to a mutex-protected queue.
    // It drains that queue on explicit driver requests; it adds no idle timer.
})?;
// Shutdown: stop subscribers, take picker out of the lock, then join background work.
shared.unwatch(id);
shared.shutdown_watches_and_wait();
let picker = shared.write()?.take();
if let Some(mut picker) = picker {
    picker.stop_background_monitor();
}
```

The spike leaves frecency/query history empty and doesn't simulate repeated note opens, so it establishes embedding and matching behavior, not the quality of learned ranking. It owns `QueryTracker` directly and passes `Some(&tracker)` into fuzzy search; only frecency uses a shared tracker. Regex detection and fuzzy retry are explicit above, not automatic behavior of `FilePicker::grep` alone.

## Reproduce and decision

From this worktree, on macOS Apple Silicon:

```sh
cargo build --release -p fff-check
python3 spikes/fff-check/run_check.py --output /tmp/fff-check-run-1
python3 spikes/fff-check/run_check.py --output /tmp/fff-check-run-2
cargo build
```

The driver prints the resident PID, retains all raw samples, invokes the requested OS tools and exits unsuccessfully if an expected match, watcher transition or callback fails. In the Codex execution sandbox, `top` was denied, so these recorded runs were launched in Rex's host terminal with `rex run -s "$REX_SESSION" -c "$PWD" -- …`; the measured binary remained the same local release build. Don't run compilation concurrently with the benchmark. Output directories should be fresh. The results cover only this synthetic 10k-note workload and a short indexed-idle interval, not cold-disk startup, a long battery soak, crash recovery or a full daemon/CLI round trip.

Keep fff out of the daemon integration for now. Reconsider after an event-driven debounce implementation (or using `watch: false` behind the daemon's own watcher) demonstrates zero idle timers, and the intended selective/typo search contract fits the warm-call budget or explicitly revises it. Neither mitigation was implemented or benchmarked here. `crates/core` and `crates/ui` source files were untouched; both the release spike build and workspace `cargo build` passed. As requested, the spike is a root workspace member: default workspace builds now include fff and its 76 added packages, and shared dependency features may be unified. No pre-existing locked package version was removed. The spike also passes `cargo fmt -p fff-check --check` and `cargo clippy -p fff-check -- -D warnings`. A read-only Claude CLI review checked the implementation and evidence; the report narrows the identifier and wakeup attribution claims accordingly.
