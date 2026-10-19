# `notesd` bench

Measured 2026-10-18 on an Apple M5 (10 cores, 24 GiB), macOS 26.6.2, rustc 1.99.1,
release builds. This was a working developer Mac (load average 3–6 from other work), not an
isolated host; no compile ran alongside the measured runs.

**Verdict:** every verb except `move` and the unique-identifier search answers in under
2.5 ms p95 over the socket on a 1,000-note vault. Indexed idle is zero CPU, zero wakeups
and zero disk I/O over 30 s, with or without an open `watch` stream. A whole `notes` CLI
call against the real daemon is 5.6–6.1 ms mean, the same as `notes --version`, so process
startup dominates.

## Setup

- `crates/daemon/examples/bench.rs` generates 1,000 notes (4.6 MB, 1–8 KB each, seed 249) in
  10 topic folders × 5 subfolders. Each note has headings, `## Todo`, a unique
  `noteidentityNNNNN` line and wiki links. Then it spawns `target/release/notesd` on
  `/tmp/nd-bench` with a fresh database.
- Each sample is one fresh unix-socket connection, one request line and one response line,
  timed from `connect` to the parsed reply. That's a CLI call without process startup.
- Each case gets 5 excluded warmups, then 200 samples (50 for `move`). Percentiles are
  nearest rank. Write verbs run against the same vault, so later cases see slightly larger
  notes and more history.
- fff runs with the retest config from docs/fff-check.md (`watch: false`, content indexing,
  mmap cache, `FFFMode::Ai`). One change: `time_budget_ms: 0`, so every search runs to
  completion, as docs/cli-spec.md requires. Fuzzy only runs with `fuzzy: true`.

## Per-verb socket latency (ms)

| Verb | Run 1 p50 | Run 1 p95 | Run 2 p50 | Run 2 p95 |
|---|---:|---:|---:|---:|
| doctor | 0.039 | 0.045 | 0.040 | 0.048 |
| manifest | 0.050 | 0.060 | 0.050 | 0.063 |
| list (root) | 0.053 | 0.058 | 0.052 | 0.062 |
| list -r (1,000 notes) | 1.809 | 1.888 | 1.863 | 1.940 |
| find `release-plan` | 0.152 | 0.165 | 0.150 | 0.165 |
| find typo `recipe-sourdugh` | 0.078 | 0.087 | 0.077 | 0.085 |
| search common `latency budget` | 1.053 | 1.342 | 1.036 | 1.330 |
| search unique `noteidentity00777` | 4.954 | 7.859 | 4.844 | 5.718 |
| search miss `zzzzunfindable999` | 0.051 | 0.072 | 0.050 | 0.057 |
| search `--fuzzy` typo `sourdugh` | 0.321 | 0.422 | 0.295 | 0.449 |
| read | 0.064 | 0.070 | 0.066 | 0.076 |
| read `--lines 1:5` | 0.057 | 0.064 | 0.057 | 0.067 |
| read (bare title) | 0.053 | 0.065 | 0.056 | 0.064 |
| history | 0.061 | 0.080 | 0.062 | 0.092 |
| new | 0.231 | 0.445 | 0.234 | 0.425 |
| append | 0.328 | 0.516 | 0.323 | 0.427 |
| append `--under Todo` | 0.324 | 0.406 | 0.322 | 0.450 |
| prepend | 0.331 | 0.413 | 0.327 | 0.404 |
| edit (unique old) | 0.341 | 0.433 | 0.341 | 0.437 |
| write `--force` | 0.273 | 0.350 | 0.289 | 0.367 |
| diff (prev → current) | 0.098 | 0.112 | 0.102 | 0.110 |
| revert | 0.055 | 0.076 | 0.056 | 0.064 |
| mkdir | 0.099 | 0.261 | 0.095 | 0.127 |
| rmdir | 0.104 | 0.130 | 0.105 | 0.145 |
| delete / restore (alternating) | 0.227 | 0.501 | 0.196 | 0.296 |
| batch (3 reads, atomic) | 0.201 | 0.290 | 0.203 | 0.228 |
| move (rename + link scan) | 25.380 | 40.013 | 21.606 | 27.346 |
| watch: append → event on open stream | 0.421 | 0.976 | 0.410 | 0.784 |

Max values sit at 0.1–2.8 ms for most verbs, with a few outliers. They're in the raw output
of each run, which the bench prints.

What the slow rows are:

- **`move` is 21–25 ms p50.** After the rename it opens and reads every note to find
  `[[Old]]` / `[[old/path]]` links, and only syncs and rewrites the notes that contain one.
  This is about 1,000 file opens, so the cost grows with vault size (expect roughly 10×
  at 10k notes). The upgrade is to ask fff's grep for `[[old` instead; there's a
  `ponytail:` note at that spot.
- **The unique-identifier search is 4.8–5.0 ms.** All 1,000 notes share the
  `noteidentity` prefix, so fff's prefilter can't skip files and the search reads every one.
  That's the same pattern as the spike's 33 ms at 10k notes, and an fff property, not daemon
  overhead.
- **Writes are 0.23–0.34 ms p50.** That covers a temp-file write, `fsync`, rename, a
  directory `fsync`, one SQLite transaction (notes row, full-text history snapshot, event)
  and the fff update. They use `fsync`, not `F_FULLFSYNC`, which is the same durability SQLite
  gives by default on macOS.
- **Frecency runs off the request path.** fff's frecency write is a synced LMDB commit
  (LMDB uses `F_FULLFSYNC` on macOS, about 4 ms each). With it inline, `read` took 4.0 ms
  and a 3-read batch 12 ms. Deferring it on the connection thread only moved the cost: the
  queued LMDB fsyncs delayed the writes that came next (`new`/`append` went to 4 ms). Now a
  single `notesd-frecency` thread, blocked on a channel, records at most one access per note
  per minute, and `read` is 0.06 ms.

## Whole CLI calls against the real daemon

```
hyperfine -N --warmup 50 --runs 500 'notes doctor' 'notes read work/plan' \
  'notes append work/plan x' 'notes search Todo' 'notes --version'
```

| Command (release `notes` → release `notesd`) | Mean ± σ (ms) |
|---|---:|
| `notes doctor` | 6.1 ± 0.6 |
| `notes read work/plan` | 5.9 ± 0.7 |
| `notes append work/plan x` | 5.9 ± 0.5 |
| `notes search Todo` | 5.6 ± 0.8 |
| `notes --version` (no socket) | 5.7 ± 0.8 |

This used a one-note vault, so it measures the CLI process plus one real round trip. The
socket numbers above cover vault-size effects. All five land within the spec's ≤10 ms
warm-call budget.

## Indexed idle

After the latency cases, the bench waits 3 s for the vault watcher to settle after its own
writes. It then brackets two 30 s windows with `proc_pid_rusage(RUSAGE_INFO_V2)`, converting
CPU ticks with `mach_timebase_info` as the spike did. The bench sends nothing during either
window.

| Window | Run | Seconds | CPU ms | Interrupt wakeups | Pkg-idle wakeups | Disk bytes | RSS at end | Footprint at end |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| watch stream open | 1 | 30.004 | 0.000 | 0 | 0 | 0 | 17,432,576 | 11,616,640 |
| no clients | 1 | 30.000 | 0.000 | 0 | 0 | 0 | 17,399,808 | 10,699,136 |
| watch stream open | 2 | 30.005 | 0.000 | 0 | 0 | 0 | 20,398,080 | 11,551,152 |
| no clients | 2 | 30.004 | 0.000 | 0 | 0 | 0 | 15,204,352 | 11,518,384 |

The process has 12 threads at the end (`ps -M`):

- main, blocked in `accept`
- `notes-vault-kqueue`, in `kevent` with a NULL timeout
- `notesd-frecency`, in a channel `recv`
- fff's 5 + 4 Rayon pool threads, in untimed condvar waits (docs/fff-check.md)

An open watch adds two threads: one in an untimed condvar wait for new events, and one
blocked in `read` to notice the client hanging up. Neither shows up in the counters. Nothing
in `notesd` uses a timer, a sleep or a poll once it serves. The only polling is fff's
`wait_for_scan` / `wait_for_indexing_complete` helpers (10 ms steps), and only during
startup.

Startup from spawn to the first `doctor` reply took 108–594 ms across four runs on this
loaded machine. That includes bind, SQLite open, the fff scan, content indexing and
reconciling 1,000 notes into SQLite. The first run on a fresh database also writes a
version-1 history snapshot for every note.

## Crash safety (from `cargo test -p notesd`)

`kill_9_during_large_write_never_tears_the_note` writes a 40 MB note, sends a 40 MB
`write --force` and `kill -9`s the daemon. In one round the kill comes the moment the temp
file appears, so mid-write by construction. In five more rounds it comes after 0–150 ms.
Every round leaves the note fully old or fully new, and the restarted daemon serves the
same bytes. It also removes the orphaned `.big.md.notesd-tmp`.

In release builds, the mid-write and early rounds ended old and the 80 ms and 150 ms rounds
ended new. In debug builds every round ended old, because JSON parsing outlasts the delays.
If the kill lands between the rename and the SQLite commit, the restart reconciles the file
and records it as an `external` version, so the file and the database never disagree.

## Not measured

- No vault larger than 1,000 notes, no cold disk cache, no long battery soak. The 30 s idle
  windows aren't a guarantee for hours of idle.
- Watcher latency from an outside edit to its recorded version isn't timed, only checked
  for correctness by the integration test.
- launchd socket activation works: a throwaway job booted out afterwards started
  `notesd` on the first connect, which answered in 195 ms including spawn and index, and
  `doctor` reported the launchd socket. It isn't benchmarked, though, and the effect of
  `ProcessType Background` (E-core scheduling, low-priority I/O) on warm latency is
  unmeasured. If it hurts, `Adaptive` or `Standard` is the switch in
  crates/daemon/launchd/dev.notes.notesd.plist.
- Power-loss durability: writes `fsync`, not `F_FULLFSYNC`, and only crash (kill -9)
  consistency is tested.
- Peak fd use: the watcher holds one fd per note and folder, and raises the soft limit to
  min(hard, 20,000).

## Behaviour notes and known limits

- **Link rewriting on `move` is implemented** for `[[Title]]`, `[[Title|alias]]`,
  `[[Title#heading]]` and `[[old/path]]`. Matching is case-sensitive, and bare-title links
  are only rewritten if the old title was unique. The new target is the new title when
  that's unique, else the path.
- **Writes are serialized vault-wide.** One global lock (the SQLite connection mutex) covers
  every note, which gives per-note serialization for free. Edits re-read the file and
  re-match `old` under that lock, which is how two concurrent clients editing different parts
  both land (tested with 2 × 25 edits).
- **In-place outside writes.** A truncate-then-write save (e.g. `echo > note.md`) can be
  snapshotted between the two steps, recording an intermediate `external` version before
  the final one. The final content always lands. Atomic-rename saves record exactly one
  version.
- **`list --limit` applies to notes only**, so folders still come first.
- **`Request.cwd` is ignored.** Refs are vault-relative and absolute paths are rejected.
- **Trash** keeps notes 30 days and is purged at startup and after each `delete`, never on
  a timer.
- **fff's post-scan capacity.** fff 0.11.0 can add 1,024 files after its initial scan. Past
  that, `notesd` triggers one background rescan; writes that land during it are picked up
  on their next touch.

## Reproduce

```sh
cargo build --release -p notesd --bins --examples
target/release/examples/bench 1000 200     # don't pipe into head: the bench must reach its cleanup
cargo test -p notesd
```
