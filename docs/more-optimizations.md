# More optimizations for sideshow GPUI (not already in plan)
Targets: warm start <=50 ms, cold <=100 ms, keystroke->pixel 1 frame, ~0% idle CPU, CLI <=10 ms. macOS only, Rust + GPUI, launchd daemon (SQLite + Loro).
Labels: **[F]** = found in cited source. **[H]** = my hypothesis/guess, untested. Nothing here was benchmarked by me.
Not repeated (already in fast-notes-research.md / composio / x findings): ProMotion keep-alive, present_drawable, triple buffering, CAMetalDisplayLink, NSGlassEffectView, Kawase blur, uniform_list/list, shaped-text cache, launchd socket activation, UDS vs shm ring, WAL+mmap pragmas, Loro shallow snapshots, os_signpost, MTL HUD, Notion/Linear lessons, basic dylib trimming.

---
## PART A. Method: Brendan Gregg framing (goal = LEAST CPU/GPU/memory, not only speed)
Source for USE: https://www.brendangregg.com/usemethod.html [F]: "For every resource, check utilization, saturation, and errors"; errors can be checked first; short bursts of 100% hide in long averages; any non-zero saturation can be a problem; software resources (locks, thread pools, fd capacity) can be treated the same; follow with workload characterization / drill-down. The page lists a Mac OS X USE checklist in its nav (I did not open it: check https://www.brendangregg.com/USEmethod/use-macosx.html [H: URL guessed from nav]).
Other Gregg methods (from the brief; I did not fetch their pages): eliminate unnecessary work first; active benchmarking (analyze the benchmark while it runs, so you know what limits it); flame graphs (on-CPU); off-CPU analysis (why threads block).

### A1. Order of operations (rule)
1. **Eliminate unnecessary work**: an idle app must do zero work: no timers, no redraws, no polling, no display link when nothing changed. Cheapest CPU/GPU is the one never spent.
2. Workload characterization (who/what/why/how): per scenario record events/s (keystrokes, SSE posts, CLI calls, scroll), bytes/event, which thread. Scenarios: IDLE (window hidden), IDLE-visible, TYPE, STREAM (100 posts/s), SCROLL, CLI-BURST, BG-MAINT (checkpoint/compaction).
3. USE per resource (table below), then flame graph + off-CPU drill-down on whatever is non-zero.
4. Active benchmark: run the load generator and watch powermetrics/Instruments while it runs; verify the benchmark itself isn't the limiter.

### A2. USE checklist for this app (macOS tools)
| Resource | Utilization | Saturation | Errors | Tool |
|---|---|---|---|---|
| CPU (P/E cores, per process) | `CPU ms/s` per process; per-QoS ms/s | runnable-but-waiting (scheduler latency), main-thread frame overrun, thread-pool queue length (software) | panics, crash logs, SQLITE_BUSY retries | `sudo powermetrics --samplers tasks --show-process-energy -i 3000 -n N` (CPU ms/s, QoS, timer + package-idle wakeups) [F: man page, https://keith.github.io/xcode-man-pages/powermetrics.1.html]; `--show-process-qos` shows time per QoS class [F: Apple energy guide, https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/PrioritizeWorkAtTheTaskLevel.html]; Instruments Time Profiler / `sample <pid>` |
| Wakeups | idle wakeups/s, timer wakeups/s ("package idle exit" wakeups are the expensive subset) [F: https://firefox-source-docs.mozilla.org/performance/powermetrics.html] | timers with <2 ms / 2-5 ms deadlines [F: same] | n/a | powermetrics tasks; spindump of a near-idle process |
| GPU | `GPU ms/s` per process [F: Firefox doc: `--show-process-gpu`]; GPU ms/frame from Metal HUD / Metal System Trace | frames missed, drawables waiting (nextDrawable stalls), encoders per frame [F: HUD flags excess encoders, https://developer.apple.com/documentation/xcode/gaining-performance-insights-with-metal-performance-hud] | Metal validation errors, device lost | `sudo powermetrics --samplers gpu_power`; Instruments Metal System Trace; MTL_HUD_ENABLED=1 |
| Memory capacity | resident/dirty+swapped footprint (see Part C) | memory-pressure level, compressed/swapped bytes, page-ins | failed allocs, jetsam/OOM kills | vmmap, footprint, Activity Monitor, `vm_stat` |
| Storage I/O | bytes/s written, fsyncs/s | WAL growth / checkpoint stall time | SQLITE_FULL/IOERR | `fs_usage -w -f filesys <pid>`, Instruments File Activity, `sqlite3_wal_hook` counters |
| Unix socket / IPC | msgs/s | accept backlog, writer blocked on slow client (off-CPU) | EPIPE/ECONNRESET | `lsof -U`, `fs_usage -f network` [H: flag set], app counters |
| Energy | composite energy impact | n/a | n/a | Instruments Energy Log; Activity Monitor Energy tab; powermetrics cpu_power/gpu_power (estimates; not comparable across devices) [F: man page] |
| Software resources | mutex held time on DB writer; channel queue depth; frame-budget | queued SSE frames per frame | dropped/coalesced count | own counters + signposts |

### A3. Per-resource budgets (all [H] targets; calibrate after first measurement, then ratchet)
| Scenario | CPU | GPU | Wakeups/s | Notes |
|---|---|---|---|---|
| Window hidden/occluded, daemon idle | 0.0 CPU ms/s (<1 ms/s) | 0 | <=1 (target 0) | no display link, no timers; blocked in kqueue/poll |
| Visible, nothing changing | <0.1% | 0 ms/frame, 0 presents | <=2 | redraw only on invalidation; stop display link after ~1 s idle tail |
| Cursor blink | <0.3% | <0.2 ms/frame | <=2 (blink) | blink via coalesced timer with tolerance, or no blink animation after N s [H] |
| Typing (10 keys/s) | <5% of one P core | <1 ms/frame (scene is small) | per frame only | 1 frame latency; allocations/frame ~0 |
| Streaming 100 posts/s | <10% of one core | <2 ms/frame | <=display rate | coalesce to <=1 repaint/frame |
| Scroll | <15% one core | <2 ms/frame at 120 Hz (8.33 ms budget, https://zed.dev/blog/videogame [F]) | display rate | |
| Glass blur (sidebar) | n/a | <=0.5 ms/frame incremental; 0 when sidebar unchanged | | native compositor blur vs in-app: measure |
| Maintenance (checkpoint/compaction) | utility/background QoS, never on main | none | batched | |
| Daemon energy | "Energy Impact" ~0 when idle | | | verify in Activity Monitor over 1 h |
| Resident memory | see Part C | | | |

### A4. Falsifiable predictions (Gregg-style)
- U1: With window occluded for 60 s, `powermetrics tasks` shows <=1 idle wakeup/s and <1 ms/s CPU for both UI and daemon. Falsifier: >5 wakeups/s -> find the timer via spindump/`sample` (candidates [H]: SSE keepalive, blink timer, display link left running, tokio/async runtime timer wheel, notify/FSEvents).
- U2: Visible-idle GPU ms/s is 0 and present count is 0 after the 1 s tail. Falsifier: any steady GPU time -> something invalidates each frame (animation, caret, spinner).
- U3: Flame graph of 30 s typing shows <20% of main-thread time outside {shape(cached), layout of dirty blocks, paint, present}; if parse/DB/serialize appear on main, move them off. Falsifier: those frames show up.
- U4: Off-CPU profile of main thread during STREAM shows no blocking on DB mutex/socket write (blocked time <0.1 ms/frame). Falsifier: main blocks in `__psynch_mutexwait`/`write`/`fsync` -> priority inversion or sync I/O on main.
- U5: Replacing in-app blur by native NSGlassEffectView lowers GPU ms/s during scroll. Falsifier: equal within noise -> keep whichever is simpler. (compositor cost is attributed to WindowServer, not the app: measure system-wide GPU, not only per-process [H].)
- U6: Pre-scale of Retina-unneeded surfaces / smaller drawable (only for the near-opaque reading pane): halving pixel count of redrawn region reduces GPU ms/frame ~proportionally [H]. Falsifier: GPU time flat -> vertex/encoder bound, not fill bound.
- U7: Scissoring/damage-limited redraw (draw only dirty rect) lowers GPU for caret-only changes vs full-window repaint. Falsifier: GPU time same (Metal loads full attachment anyway, then use fewer/cheaper passes instead) [H].

---
## PART B. Optimization areas (item: payoff / test / falsifier / priority)

### B1. Release profile
- What [F]: Cargo defaults for release are opt-level=3, lto=false, codegen-units=16, panic=unwind, strip=none, debug=false. Options: `lto="fat"|"thin"`, `codegen-units=1`, `panic="abort"`, `strip`, `opt-level` "s"/"z" (Cargo doc warns level 3 can be slower than 2 and "s"/"z" not necessarily smaller; benchmark each). https://doc.rust-lang.org/cargo/reference/profiles.html
- Proposed: `[profile.release] lto="fat"; codegen-units=1; panic="abort"; strip="symbols"; debug="line-tables-only"` (keep a separate `release-profiling` profile with debuginfo + symbols for flame graphs/Instruments) [H on exact values].
- Payoff [H]: smaller binary -> fewer pages faulted at launch; LTO/CGU=1 often 5-20% CPU on hot loops; panic=abort removes unwind tables. Caveat: GPUI/dep code that relies on catch_unwind (e.g. panic isolation per window) breaks under abort; daemon should have a supervisor (launchd KeepAlive) instead.
- Test: `hyperfine` of CLI exec, cold-first-paint signpost, `size`/`ls -l`, criterion on hot paths, A/B alternated. Falsifier: <3% on every metric and no size win -> skip long LTO build times. Priority: **high** (cheap, one-time).
- panic=abort tradeoff: lose backtrace-on-panic unless you install a panic hook that writes before abort. Test crash-report quality.

### B2. PGO / BOLT on macOS
- PGO: rustc supports `-Cprofile-generate` / `-Cprofile-use`, merge with llvm-profdata of the same toolchain. https://doc.rust-lang.org/rustc/profile-guided-optimization.html [F]
- **macOS caveat [F]**: rustc's own CI does not PGO on macOS due to symbol-matching problems (https://github.com/rust-lang/rust/issues/103595); cargo-pgo issue shows "Runtime and instrumentation version mismatch" on macOS (https://github.com/Kobzol/cargo-pgo/issues/68), where a maintainer says PGO on macOS "doesn't work well". Mixed reports (works on one local Mac).
- **BOLT: ELF-only, Linux-only [F]** (same rust issue #103595). Not usable for Mach-O.
- Payoff [H]: maybe 5-15% on hot CPU paths, little effect on the 100 ms cold start (dominated by dyld/GPU init). Test: build PGO with `-Cllvm-args=-pgo-warn-missing-function` to detect profile mismatch; compare criterion. Falsifier: warnings for most functions or <3% -> drop. Priority: **low** (try only after B1; measure after you have flame graphs showing CPU-bound code).

### B3. dyld / binary size / launch (additional to existing dylib-trimming note)
- [F, secondary] A macOS Rust hello-world is dynamically linked via libSystem and carries a startup floor (~7 ms and ~1 MB RSS during dyld init, per https://minicompiler.dev/guide/footprint/); stripped Rust hello-world ~421 KB vs 33 KB C. The floor applies to the CLI: CLI <=10 ms budget leaves ~3 ms for everything else. [H] consider a CLI that links only libSystem (no GPUI/Foundation/AppKit, no tokio multi-thread runtime, std::net::UnixStream blocking, no clap derive bloat -> hand-rolled arg parse).
- Separate crates: `sideshow` CLI (tiny) vs `sideshowd`/UI. Test: `hyperfine -N --warmup 20 'sideshow ping'` (-N for no shell, important for <5 ms commands) [H on flag use; hyperfine docs not fetched], `dyld_info -dependents`, `otool -L`. Falsifier: CLI exec within 1 ms of an empty Rust main -> nothing left to gain. Priority: **high** for CLI.
- Mach-O ordering [H]: `-order_file` / `-dead_strip` for locality; test page faults with `/usr/bin/time -l` (page faults / reclaims). Priority low.
- Launch-time global init: no `lazy_static` ICU/regex compile/syntax-grammar load before first frame; defer. (Covered partly in P12.) Test: signpost from `main` to first present with budget per phase.

### B4. Allocator (macOS)
- [F] mimalloc on macOS is not a guaranteed win: older issue showed mimalloc 2.0.9 slower than libmalloc on a Crystal build, similar by 2.1.4 (https://github.com/microsoft/mimalloc/issues/676). Turborepo PR: Apple Silicon user-CPU −6% (14/15 pairs better), +13% memory on one Linux case (https://github.com/vercel/turborepo/pull/13237). Boa PR: up to ~8.6% on allocation-heavy, compute-bound unchanged (https://github.com/boa-dev/boa/pull/5431). There is an M4 Max allocator benchmark including tantivy (https://github.com/zooko/bench-allocators/blob/main/benchmark-results/AppleM4Max.darwin25/COMBINED-REPORT.md; numbers not extracted).
- For *least* CPU/memory in an idle resident daemon, allocator choice matters mostly for: (a) churn during STREAM, (b) how fast memory returns to the OS (Part C). Default recommendation [H]: stay on system malloc until Allocations trace shows alloc cost >5% of a flame graph; then A/B mimalloc with purge delay tuned.
- Test: same binary, feature-flag allocator, interleaved A/B; metrics: CPU ms/s under STREAM, RSS after 10 min idle, p99 frame. Falsifier: <2% CPU and higher idle RSS -> system malloc wins. Priority: **med**.

### B5. QoS / P-E core scheduling (idle energy)
- [F] QoS drives P/E placement on Apple Silicon; Background QoS runs on E cores (Eclectic Light: QoS<=9 threads exclusively on E cores) https://eclecticlight.co/2024/12/05/inside-m4-chips-cpu-core-management/ ; Apple docs https://developer.apple.com/documentation/apple-silicon/tuning-your-code-s-performance-for-apple-silicon.md ; set with `pthread_set_qos_class_self_np`; setting raw priority opts out of QoS permanently (https://developer.apple.com/videos/play/tech-talks/110147/). Apple guidance: run at utility or lower >=90% of the time when no user activity (energy guide link above); main thread is user-interactive; verify with `powermetrics --show-process-qos`.
- Plan [H]: UI main + render thread = user-interactive; input-triggered DB reads / parse of visible note = user-initiated; syntax highlight, thumbnails, search index, checkpoints, Loro compaction = utility/background; SSE fan-out in daemon = utility unless a user is typing. Daemon launchd `ProcessType` (Interactive/Adaptive/Background/Standard), `LowPriorityIO`, `Nice` exist [F: https://www.manpagez.com/man/5/launchd.plist/]; [H] Adaptive for daemon so it is elevated when UI/XPC active, throttled otherwise; Standard if Adaptive hurts CLI latency (test).
- Beware priority inversion [F]: main waiting on low-QoS lock holder; system boosts for pthread_mutex/dispatch_sync but not across multiple locks.
- Test: powermetrics QoS columns during IDLE/TYPE/STREAM; CLI p99 with background maintenance running. Falsifier: CLI p99 rises >2x when daemon is Background -> use Adaptive/Standard. Priority: **high**.

### B6. App Nap / activity assertions / timers
- [F] Apple: avoid polling timers, use tolerance >=10% of interval so timers coalesce; use NSBackgroundActivityScheduler for periodic work; `beginActivity(options: .background)` for maintenance (https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html , .../SchedulingBackgroundActivity.html , .../AppNap.html).
- Real-world idle-wakeup bug [F]: RustDesk had ~280 idle wakeups/s from 11 threads each sleeping 20-35 ms; fix pattern is condvar parking instead of sleep loops (https://github.com/rustdesk/rustdesk/discussions/14409). Rule: **no `thread::sleep` loops, no `tokio::time::interval` for housekeeping**; all housekeeping via event-driven or one coalesced timer.
- Test: U1. Falsifier: wakeups already ~0 -> done. Priority: **high**.

### B7. Redraw-on-change only (GPUI)
- [F] Zed keeps presenting for 1 s after input to hold ProMotion (existing). Complement: after that tail, **stop the display link / present nothing** (zero GPU). Also hide-window/occlusion: pause link. [F, existing note] "pauses link on inactive windows".
- [H] Event-source audit: mark every `cx.notify()` source; count invalidations/s in IDLE with a debug counter; assert 0 in CI via a headless test that idles 5 s and counts frames.
- Streaming: one repaint per display frame (exists as P13). Add: if the post is below the fold (not in viewport), update model but don't notify the view [H].
- Test/falsifier: frame counter == 0 over idle 5 s; falsifier: >0. Priority: **high**.

### B8. Text editing data structure
- [F] Ropey: O(log n) edits, cheap clones; Zed's rope is a SumTree: copy-on-write B+tree, 128-byte chunks, summaries for O(log n) offset/line lookup and snapshots (https://zed.dev/blog/zed-decoded-rope-sumtree , https://github.com/cessen/ropey/ , https://zed.dev/blog/zed-decoded-rope-optimizations-part-1). Gap buffers can beat ropes for local edits (https://coredumped.dev/2023/08/09/text-showdown-gap-buffers-vs-ropes/).
- Notes are small (KB-MB). [H] Use Loro's text (already a rope-ish tree) as source of truth + a cheap render snapshot (Rope or String); avoid maintaining two full copies; hold render copy as `Arc<str>`-chunks so the UI thread gets snapshot clones without copying.
- Test: criterion: insert-at-cursor, line-index lookup, snapshot clone for 10 KB / 1 MB; memory per copy. Falsifier: plain `String` + line-start Vec within 2x of rope at 1 MB -> skip rope dependency. Priority: **med**.

### B9. Markdown parsing
- [F] tree-sitter-md supports incremental reparse with the edited old tree (block+inline trees separate), but grammar mainly for highlighting, has known incremental inline correctness issues (https://docs.rs/tree-sitter-md/latest/src/tree_sitter_md/parser.rs.html , https://github.com/tree-sitter-grammars/tree-sitter-markdown/issues/92 , https://github.com/tree-sitter/tree-sitter/issues/5636). pulldown-cmark: fast pull parser with byte ranges but not incremental (https://pulldown-cmark.github.io/pulldown-cmark/).
- [H] For notes <~200 KB, reparse only the *edited block range* (split on blank lines / fenced boundaries) with pulldown-cmark and cache per-block layout; no need for tree-sitter. Keep tree-sitter only for code fences if needed.
- Test: criterion full reparse of 10 KB/100 KB/1 MB; keystroke-parse p99; falsifier: full reparse <0.5 ms at 100 KB -> don't bother with block-level incremental. Priority: **med**.

### B10. Search
- [F] SQLite FTS5 trigram supports substring and indexed LIKE/GLOB; <3-char queries don't use the index; detail=none/column shrinks index when only LIKE/GLOB used (https://www.sqlite.org/fts5.html). Tantivy ngram(2,3) supports 2-char queries but larger/slower to build (example commit https://github.com/FirenzeLor/gemini-collector/commit/7244a1737a96b5563b173611cfba9c54b053e333). One small benchmark: FTS5 p50 ~0.3 ms vs Turso/Tantivy ~18 ms, `LIKE` ~3 ms (https://blog.tsugumisys.com/posts/20260601-uWqLq.html; narrow workload).
- Rec [H]: FTS5 (in-process, same DB file, no second store/memory/file handles). For 1-2 char queries: prefix-less fallback to in-memory title scan. Update index in the writer transaction batch, not per keystroke; low-QoS rebuild.
- Test: p50/p99 at 10k/100k notes, index size, RSS delta; falsifier: FTS5 trigram >16 ms at 100k -> evaluate tantivy. Priority: **med**.

### B11. Durability vs latency (autosave, WAL)
- [F] WAL+synchronous=NORMAL: consistent but may lose recent commits on power loss/OS crash; app crashes lose nothing; with NORMAL only checkpoints sync, so running checkpoints on a separate thread keeps writers from blocking on fsync (https://www.sqlite.org/pragma.html , https://sqlite.org/wal.html). macOS: `PRAGMA checkpoint_fullfsync` uses F_FULLFSYNC at checkpoints, default off, Mac-only (same pragma page; practical use: https://github.com/openclaw/openclaw/pull/99067).
- Rec [H]: Loro op log append is the durability unit (cheap append, group-commit every ~50-200 ms or on idle/blur/quit), SQLite synchronous=NORMAL + `wal_autocheckpoint` disabled and checkpoints triggered at idle with `checkpoint_fullfsync=1` (cost off the hot path); `synchronous=FULL` only at explicit "save"/quit if you want power-loss durability. Define RPO explicitly (e.g. "lose <=200 ms on power loss, 0 on app crash").
- Test: kill -9 daemon during typing -> 0 lost ops; pull-power can't be tested, use fault-injection VFS [H]. Measure main-thread stalls: signpost around commit; p99 keystroke latency with/without checkpoint running. Falsifier: commit p99 <1 ms either way -> keep simplest (NORMAL + autocheckpoint). Priority: **high**.

### B12. Loro op batching & undo
- [F] Batch edits into one transaction/commit (one event, one Change); UndoManager `mergeInterval` groups typing into one undo step; `groupStart/groupEnd` (https://loro.dev/docs/concepts/transaction_model , https://loro.dev/docs/advanced/undo). Loro fixed O(n²) typing/event-compose paths (https://github.com/loro-dev/loro/commit/4d577ad9ac6831cba65aa6bc7e9155b02a755725) -> pin a version >= that fix. [JS API shown; Rust equivalents assumed [H]].
- Rec [H]: commit once per input event-loop tick (not per char), UndoManager merge ~500-1000 ms, don't emit per-char events to CLI subscribers (coalesce deltas to <=1 per frame).
- Test: replay B4-style typing trace, count commits/s, events/s, allocations/keystroke; falsifier: no difference vs per-char commit at note sizes <100 KB -> keep simple. Priority: **med**.

### B13. Image / asset decode
- [F] ImageIO: `CGImageSourceCreateThumbnailAtIndex` with `kCGImageSourceCreateThumbnailFromImageAlways`, `kCGImageSourceShouldCacheImmediately`, `ThumbnailMaxPixelSize` decodes+downsamples at creation, run on a serial background queue (https://developer.apple.com/documentation/imageio/kcgimagesourceshouldcacheimmediately , WWDC18-219 https://wwdcnotes.com/documentation/wwdc18-219-image-and-graphics-best-practices/).
- [H] Decode at display pixel size (not source), cap decoded cache bytes, upload as one texture; don't decode off-screen images at all (lazy at <1 screen away). Memory: a 2 MP image decoded is ~8 MB RGBA.
- Test: Allocations/VM Tracker with 50 image notes; falsifier: decode total <5 ms and <50 MB -> skip. Priority: **med**.

### B14. Large notes
- [H] Virtualize by block (existing uniform_list/list); additionally cap work per frame: parse/shape only blocks within viewport ±1 screen; background-prepare rest at utility QoS; persisted height cache (exists). Add cut-off: for notes >N MB use plain-text mode (no inline parse). Test: 5 MB note open and scroll; falsifier: open >50 ms or scroll p99 grows with size. Priority: **med**.

### B15. Accessibility cost
- [F] GPUI gained AccessKit integration (PR merged ~May 2026), reported working with macOS VoiceOver; Zed main editor remains largely inaccessible; exposes nodes through IDs/roles (https://github.com/zed-industries/zed/pull/56065 , https://github.com/zed-industries/zed/blob/20a3f770/crates/gpui/src/_accessibility.rs). Cost unknown.
- [H] Ensure a11y tree build is lazy (only when an assistive client is attached), else it taxes IDLE and typing. Test: toggle VoiceOver off; CPU/alloc/frame with feature compiled in vs out; falsifier: <1% -> always compile in. Priority: **low-med** (but correctness matters for users).

### B16. Crash-free startup / robustness
- [H] Daemon must never crash-loop (launchd KeepAlive + ThrottleInterval); migrations idempotent and transactional; open DB read-only fallback; corrupted Loro blob -> quarantine and restore from last snapshot; panic=abort + hook writing a minimal crash file; first-frame path avoids fallible init (fonts/shaders cached, fall back to system font). Test: fuzz DB/Loro blobs, kill -9 at random points (1000 iterations), assert next start <100 ms and no data loss beyond RPO. Falsifier: any non-start. Priority: **high** (a crash-loop burns CPU too).

### B17. Measurement / CI gates
- [F] Wall-clock on shared CI runners is noisy; instruction-count benchmarking (iai-callgrind / Callgrind) is stable for PR gates, but Valgrind lacks Apple Silicon support so run it on Linux (https://github.com/bheisler/iai , https://github.com/clockworklabs/iai-callgrind). Keep criterion for local wall-clock.
- Plan [H]: (1) Linux CI: instruction-count gates (5% fail) for Loro apply, parse, FTS query, height calc. (2) Self-hosted/local Apple Silicon runner (or nightly on the dev Mac): hyperfine CLI, signposted first-paint, idle-wakeup test via powermetrics (needs sudo), frame counter-at-idle test, RSS budget via `footprint`. (3) Track medians + p99, store baseline JSON (existing bench:check pattern).
- Falsifier: gate flaps >2% false positives over 50 runs -> raise threshold / switch metric. Priority: **high**.

---
## PART C. Memory management (no GC in Rust, but GC-like pauses and growth still exist)
Budgets (all [H], calibrate): daemon resident (dirty+swapped, per `footprint`/vmmap) <=40 MB idle with 10k notes; UI process <=80 MB idle at 1 window; growth after 1 h of mixed use <5%; **allocations per frame ~0 for unchanged scene, <=50 for typing**; no single free/drop >1 ms on main; RSS returns to within 20% of baseline 10 s after a large-note close.

### C1. Heap growth, fragmentation, returning memory to the OS
- [F] `vmmap` reports %FRAG = 100 − 100·Allocated/(Dirty+Swapped) per malloc zone; footprint is dirty+swapped (https://keith.github.io/xcode-man-pages/vmmap.1.html , WWDC22 https://developer.apple.com/videos/play/wwdc2022/10106/).
- [F] mimalloc purges unused pages after `MIMALLOC_PURGE_DELAY` ms (default shown as 1000 in v3 docs; options page says 10), 0 = immediate (less memory, slower), -1 = never; `PURGE_DECOMMITS=1` (default) uses decommit (MADV_DONTNEED on mmap systems, drops RSS immediately) vs reset (MADV_FREE, doesn't reduce RSS immediately) (https://microsoft.github.io/mimalloc/environment.html). mimalloc keeps virtual ranges, decommits within them. Note: MADV_FREE-style reuse on macOS may keep footprint high until pressure [H].
- [F] System allocator: `malloc_zone_pressure_relief(NULL, 0)` asks all zones to release as much as possible, returns bytes released (https://opensource.apple.com/source/Libc/Libc-825.26/include/malloc/malloc.h.auto.html).
- Rec [H]: call `malloc_zone_pressure_relief` (or mimalloc `mi_collect(true)`) from an *idle hook* after large frees (closing a big note, finishing import, after maintenance), debounced ≥5 s, never per frame. A resident daemon should look small in Activity Monitor.
- Test: open+close 5 MB note x20, record `footprint` before/after, with and without relief; falsifier: footprint returns to baseline without relief -> drop the hook. Priority: **med-high** (daemon lives forever).

### C2. Per-frame allocation (arena/bump)
- [F, existing] GPUI shapes text via OS and caches by text+font, evicting entries unused in the next frame (zed.dev/blog/videogame).
- Rec [H]: per-frame bump arena (`bumpalo`) for transient layout/vectors/strings, reset each frame (no per-object free); reuse `Vec` capacity across frames (`clear()` not drop); `SmallVec`/fixed arrays for per-frame lists; avoid `format!` in render paths (cache string per block); `Arc<str>` share rather than clone.
- Test: Allocations instrument filtered to main thread per frame; or a counting `#[global_allocator]` wrapper that exposes allocs/frame in a debug HUD and fails a CI headless test if idle-frame allocs >0. dhat for call sites. Falsifier: idle-frame allocs already 0 -> done. Priority: **high** (it also reduces CPU/wakeups of malloc locks).

### C3. Drop storms / big deallocations on main thread (GC-like pauses)
- [H] Dropping a large `Vec<Block>`, a decoded image, or an entire Loro doc on the UI thread can block for ms. Rec: move owned large values to a background "reaper" thread (`std::thread` at background QoS with channel; or `drop` inside the worker that produced them); drop old note state lazily one note per idle tick; prefer a few big buffers over millions of small nodes (Zed uses 128-byte chunk B-tree for this reason, https://zed.dev/blog/zed-decoded-rope-sumtree [F on chunking]).
- Measure: signpost around note switch/close, Time Profiler filtered to main thread for `drop_in_place`/`free`; off-CPU view for `free` locks. Falsifier: max drop <0.5 ms at 1 MB note -> no reaper. Priority: **med**.

### C4. macOS memory pressure, compressed memory, purgeable
- [F] Register `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` (NORMAL/WARN/CRITICAL). Apple man page: on elevated pressure, *reduce cache sizes of newly initiated operations*; do NOT traverse/discard all existing caches when pressure rises since that can trigger VM operations that worsen pressure (https://keith.github.io/xcode-man-pages/dispatch_source_create.3.html). XNU doc: WARN = consider relaxing caching policy; CRITICAL = expect latencies, consider dropping all caches (https://newosxbook.com/src.php?file=%2Fdoc%2Fvm%2Fmemorystatus_notify.md&tree=xnu). Also levels list includes a macOS-only low-swap notification (root restricted).
- Rec [H]: WARN -> halve cache budgets (decoded images, glyph/shape caches, SQLite cache_size) applied to *new* inserts, and let LRU shrink naturally; CRITICAL -> drop decoded images + shape cache + `malloc_zone_pressure_relief`. Keep first-screen data pinned.
- Purgeable memory (NSPurgeableData / `vm_purgable` for decoded-image buffers): not researched in this pass [gap]; [H] could let the kernel discard decoded bitmaps for free. Jetsam: daemon is a launchd job not subject to iOS-style jetsam in the same way; macOS may still terminate under extreme pressure [H, unverified]; keep the daemon small and restartable (B16).
- Test: `memory_pressure -l warn|critical` (macOS CLI, [H: exists on macOS; not fetched]) or allocate-balloon helper; check footprint drops and no frame spike. Falsifier: handler causes >1 frame stall -> make it incremental.

### C5. Cache eviction policy table [H design; each needs a measured size]
| Cache | Key / value | Policy | Budget | Notes |
|---|---|---|---|---|
| Shaped text | text+font -> glyph run | GPUI built-in: evict if unused next frame [F] | auto | keep stable strings to avoid thrash |
| Glyph atlas | glyph+subpixel variant -> alpha bitmap (up to 16 variants [F]) | LRU / rebuild on overflow; cap texture size | <=16 MB | single atlas page; avoid reallocating mid-scroll |
| Decoded images | note+size -> downsampled texture | byte-weighted LRU, viewport pinned | <=64 MB, halve on WARN | decode at target size (B13) |
| Per-block layout/height | `width:rowKey` -> height | persisted + bounded LRU | few MB | exists |
| SQLite page cache | per connection | default cache_size −2000 (~2 MB) allocated as needed [F] (https://sqlite.org/pragma.html); mmap_size shares OS page cache [existing] | small (2-8 MB) per connection; few connections | more connections x cache = silent bloat; keep 1 writer + small reader pool |
| Loro docs in memory | note id -> LoroDoc | keep only open notes + recent N; others as bytes on disk | <=N docs, <=X MB | evict by LRU after export of unsaved ops; shallow snapshot on close when history > threshold [F: https://loro.dev/docs/concepts/shallow_snapshots] |
| Search / FTS | SQLite-managed | n/a | n/a | |
| CLI result buffers | | streaming, no full materialization | | |
- Test: per-cache hit rate + bytes exposed in a debug panel; falsifier for each: hit rate <50% at its budget -> shrink or remove (cache not paying for memory).

### C6. Flush / checkpoint / snapshot spikes
- [F] WAL checkpoint: auto at 1000 pages (~4 MB); passive; the WAL file is not truncated unless `journal_size_limit` set; checkpoint is slower than commits (needs syncs + seeks); with synchronous=NORMAL it's the only fsync point so run it off the main/writer thread (https://sqlite.org/wal.html , pragma page). `wal_checkpoint(TRUNCATE)` can be blocked by readers/writers.
- Rec [H]: `wal_autocheckpoint=0`; own policy: checkpoint (PASSIVE, then TRUNCATE if WAL >N MB) when idle ≥1-2 s or WAL > 16 MB hard cap, on a background-QoS thread with its own connection; `journal_size_limit`=16 MB. Same idea for Loro: snapshot/compaction (shallow) at idle or on note close, not on keystroke; export *incremental updates* between snapshots (existing doc advice). Autosave batching: group-commit window 50-200 ms plus flush on blur/quit/CLI "sync".
- Test: inject 1 h of typing+streaming; signpost every commit/checkpoint/snapshot; plot max main-thread stall and keystroke p99 vs time. Falsifier: no p99 spike aligned to checkpoint -> keep SQLite defaults. Priority: **high**.

### C7. Measurement recipes (Memory)
- `footprint <pid>` / `vmmap --summary <pid>` (dirty+swapped = footprint; zone FRAG) [F: vmmap man page]; `vmmap -d <secs>` diff snapshots over an interval [F: https://developer.apple.com/library/archive/documentation/Performance/Conceptual/ManagingMemory/Articles/VMPages.html]; `heap <pid>`, `leaks <pid> --outputGraph x.memgraph`, `heap -diffFrom`, `malloc_history` with `MallocStackLogging=1` [F: WWDC22/24 links above and https://developer.apple.com/library/archive/documentation/Performance/Conceptual/ManagingMemory/Articles/FindingPatterns.html]. Observer cost [F, WWDC24 https://developer.apple.com/videos/play/wwdc2024/10173/]: MallocStackLogging/Allocations add CPU+memory overhead; Leaks/VM Tracker/memory graphs suspend the app (stutter), so never use them while measuring latency. Use Instruments Allocations + VM Tracker for growth over time (Allocations template includes both).
- Rust side [H]: dhat-rs or counting global allocator for allocs/frame; `heaptrack` is Linux-only; Tracy (existing) for allocation plots.

### C8. Memory falsifiable predictions
- M1: Idle-frame allocations/frame == 0 after arena+reuse work (counting allocator). Falsifier: any steady >0 -> find via dhat.
- M2: 1 h mixed soak: `footprint` growth <5% after warmup; `heap -diffFrom` shows no growing class. Falsifier: linear growth -> leak or unbounded cache.
- M3: After closing a 5 MB note, footprint within 20% of baseline within 10 s when pressure-relief/purge is applied; without it, stays >50% above [H]. Falsifier: baseline reached without it.
- M4: Moving large drops to a reaper thread removes main-thread stalls >1 ms during note switch. Falsifier: no such stalls before.
- M5: Moving checkpoints to idle/background removes keystroke p99 spikes during streaming. Falsifier: spikes remain -> culprit is elsewhere (flame graph + off-CPU).
- M6: On `memory_pressure` WARN, budgets halve without frame >16 ms. Falsifier: stall -> handler too heavy.
- M7: mimalloc w/ `PURGE_DELAY=0` vs system malloc: lower idle RSS but higher CPU under STREAM; pick by A/B (B4).

---
## Priority summary
High: B1 release profile; B3 slim CLI binary; B5 QoS plan; B6 timers/wakeups; B7 redraw-on-change; B11/C6 durability + checkpoint scheduling; B16 crash-free startup; B17 CI gates; C2 per-frame allocation.
Med: B4 allocator A/B; B8 text structure; B9 incremental markdown; B10 FTS5; B12 Loro batching; B13 images; B14 large notes; C1 memory release hooks; C3 drop storms; C4 pressure handler.
Low: B2 PGO (BOLT is ELF-only); B15 accessibility cost.

## Gaps / not verified
- No benchmarks run. M-series numbers from third parties only; mimalloc default purge delay differs between pages (1000 v3 doc vs 10 options page).
- Not opened: Gregg's Mac OS X USE checklist, flame graph and off-CPU pages; hyperfine docs; purgeable-memory docs; jetsam on macOS daemons; `memory_pressure` CLI; Energy Log docs; Metal System Trace specifics beyond earlier notes. X namespace not used (web sources sufficed).
