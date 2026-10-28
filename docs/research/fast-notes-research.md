# Fastest note-taking/board app for humans + agents: research ("sideshow GPUI")

Target: minimal native Rust + GPUI app that streams posts from a sideshow server (TS/Hono/SQLite, SSE, CLI), renders markdown/code/diff/terminal/json/image natively, plus a shared Loro-CRDT note type editable by humans and agents via CLI.

Convention: **[F]** = found in a cited source. **[H]** = hypothesis/guess (untested). Framing follows `performance-engineering/SKILL.md` (define operation → budget → account for limiting time → measure → falsifiable prediction → close with evidence).
X reads: native X MCP, 6 queries used (of 30/min). Note that X search is noisy; only the posts below were relevant.

---
## 1. Notion's speed techniques

### 1a. From X (native reads, Sep–Oct 2026)
| Post | Date (UTC) | What it says |
|---|---|---|
| https://x.com/NotionHQ/status/2101009298813849756 (quotes https://x.com/jrudansky/status/2101005736650179023) | 2026-09-18 | "Notion Calendar, rebuilt for speed": iOS Calendar rebuilt **natively**, "faster from the first tap". |
| https://x.com/iampgzp/status/2101063066792157442 (Ji Pei) | 2026-09-18 | Mobile databases: internal **native rebuild**: smoother scroll, cleaner selection, "no more random reloads". Amplified by https://x.com/NotionHQ/status/2101064213775229302 |
| https://x.com/iampgzp/status/2102510335957582301 (Ji Pei) | 2026-09-22 | **Offline Mode expansion**: auto-keep more pages available and fresh; asks about defaults (pages <100/1k/10k+, Recent vs Favorite, disk-usage concern). I.e. a prefetch/pin policy with a disk budget. |
| https://x.com/NotionHQ/status/2108732487925117405 | 2026-10-10 | iPad app "mostly a web view" → rebuilt natively: sidebar, inbox, AI, search native; **editor native "coming soon"**. |
| https://x.com/iampgzp/status/2108740262549721436 | 2026-10-10 | Sequencing: native sidebar/AI/search first → native editor on mobile → iPad. Staged rollout of native surfaces around a web core. |
| https://x.com/iampgzp/status/2107943324242137430 | 2026-10-07 | iOS **native editor** TestFlight beta: "faster and more reliable". |
| https://x.com/nbaschez/status/2106051107407695935 | 2026-10-02 | Nathan Baschez: "Notion is laser focused on performance and reliability improvements"; hears friends feel a difference. |
| https://x.com/AustenHendler/status/2109750665513689352 | 2026-10-12 | Notion employee: "quick capture is key + load times"; offline mode lets you pin a task DB "always on". |
| https://x.com/stoobe/status/2109736875883925637 | 2026-10-12 | User complaint: on spotty mobile connections Notion downloads/syncs slowly; won't sync on planes → capture is hard. (Evidence that capture speed/offline matters.) |
| https://x.com/gustav_caves/status/2100531935281922414 | 2026-09-17 | Third-party Notion client NotiZen: "much faster startup with local caching" (corroborates cache-first, not an official Notion source). |

I found no @simonlast posts on speed (query returned 0). Ji Pei (@iampgzp) is the Notion voice on speed.

### 1b. From blogs / releases
- **SQLite as client cache (desktop/mobile)**, Notion blog 2021-04-21: https://www.notion.com/blog/faster-page-load-navigation — SQLite → initial page load and navigation **50% faster** [F]; replaced IndexedDB (quota, bugs, Windows perf); also code splitting and better client+server caching. Next targets named: search, databases.
- **WASM SQLite in browser**: https://www.notion.com/blog/how-we-sped-up-notion-in-the-browser-with-wasm-sqlite — [F]
  - navigation **−20%** median; −28% Australia, −31% China, −33% India (slow-API users win most).
  - Initial load got *slower* at first: the WASM download blocked bootup, since boot already overlaps JS parse with API fetch. Fix: load WASM **fully async**; first page comes from network, SQLite only for subsequent navigations.
  - OPFS has no safe multi-tab concurrency → SharedWorker elects one active tab (Web Locks detect closure); chose OPFS SyncAccessHandle Pool VFS (no cross-origin isolation needed).
  - Slow-device tail: per secondary summary (https://granthlabs.github.io/cache-first-apps, third-party, not Notion's page) local read can lose to network on slow disks, so navigation **races** cache vs API. Treat as secondary-source [F-secondary].
  - HN comment by an ex-Notion eng (https://news.ycombinator.com/item?id=40949489): IndexedDB pays high per-row tax; Notion's records are fine-grained (row per paragraph) [F, anecdotal].
  - SQLite forum note: Notion's query path takes two message hops (tab → SharedWorker → worker); direct MessageChannel is one hop (https://sqlite.org/forum/forumpost/29dae09659).
- **Release notes**: Notion 3.2 (2026-01-20): Windows pages open 27% faster, Mac 11%, pages load in ⅓ less time over 2025 (https://www.notion.com/en-gb/releases/2026-01-20). Notion 3.4 pt2 (2026-04-14): initial page render 28% faster (https://www.notion.com/releases/2026-04-14).
- **Cold start**: Ji Pei LinkedIn: median desktop cold-start under 1s after 9 optimizations and 5 regressions fixed (https://www.linkedin.com/posts/ji-pei_i-made-a-promise-when-i-joined-notion-get-activity-7506106681160912898-ofDz). Regressions are as important as optimizations.
- **Profiling culture**: Palette case study (vendor page): regression in `initial_page_render` p50 traced via profile comparison to a feature initialized during page load; fix = defer init. Reported typing latency −15%, load −15–20% (https://palette.dev/blog/improving-notion-typing-performance).

### 1c. Technique list distilled (what to copy)
1. Local SQLite cache of records; serve nav from local, revalidate from network. [F]
2. Never put the cache engine on the critical path of first paint (async load). [F]
3. Race local vs network for tail protection. [F-secondary]
4. Pin/prefetch policy with explicit disk budget (Offline Mode). [F]
5. Native shells around hot surfaces (sidebar/search/editor/databases) over web views. [F]
6. Defer non-essential init off the boot path; track `initial_page_render` p50 per release and diff profiles on regressions. [F]
7. Single-writer ownership of the DB (election). Relevant to us: **daemon owns the DB**, UI and CLI are clients. [F→adapted]

---
## 2. Other apps' lessons

**Linear (sync engine)** — https://linear.app/now/scaling-the-linear-sync-engine, https://github.com/wzhudev/reverse-linear-sync-engine, https://www.fujimon.com/blog/linear-sync-engine, talk https://www.youtube.com/watch?v=bnOpm3a1fRE
- Local copy (IndexedDB) + in-memory object graph; UI mutates memory synchronously, transaction queued+persisted, then sent; server assigns monotonically ordered `lastSyncId`; clients get deltas over WebSocket and replay (`/sync/delta` from lastSyncId). Bootstrap loads essentials; large collections hydrate lazily. [F]
- Scale lesson: loading everything into memory worked until ~50k objects (~0.8–1s at startup on a modern computer, per the talk) → had to make access async/lazy. [F]
- Their model is total-order server (OT-like), not CRDT. For us: **sideshow server seq number = cheap total order for posts; Loro only for the shared note type**. [H design inference]

**Zed / GPUI**
- Frame budget 8.33 ms @120 Hz includes state update, layout, and writing the frame buffer; custom shaders per primitive (rects as SDF, shadows, glyphs, icons/images) via instanced draw calls; shaped-text cache keyed by text+font, evicted when absent from next frame; glyph alpha-only atlas (up to 16 subpixel variants), tinted in shader. https://zed.dev/blog/videogame [F]
- Presentation pitfalls: `wait_until_completed` blocks long in direct-display mode; moved to scheduled + triple-buffered instance buffers; ProMotion downclocks if you stop presenting, so Zed keeps presenting frames for 1 s after last input; later reverted to `present_drawable` + commit (draw ASAP) with CVDisplayLink. https://zed.dev/blog/120fps [F]
- `uniform_list` measures only the first item then lays out only the visible range, skipping full taffy layout. `list()`/`ListState` handles variable height, measures visible items only, with `with_uniform_item_height` hints so the scrollbar is right on frame 1 and `measure_all` as the costly option. https://docs.rs/gpui/latest/src/gpui/elements/uniform_list.rs.html, https://github.com/zed-industries/zed/blob/f4199ae0/crates/gpui/src/elements/list.rs [F]
- Local notes: `/workspace/zeron/docs/research/gpui.md` (list/markdown/text input), `mugen-pretext.md` (analytic row heights, Fenwick offsets, height memo, append-only row cache, persistent height cache keyed `width:rowKey`, syntax highlight as paint after plain text, stick-to-bottom). Directly reusable as design inputs; I did not re-verify their claims. `/workspace/zeron/crates/{markdown,syntax,text,ui,visual-gpu}` already exist as a code base to lift from. [F-local]

**Obsidian / Bear / Craft / Things**
- Obsidian: plain local Markdown + vault cache/index; startup slows with large vaults and enabled plugins, which load before the app is interactive; official guidance: measure plugin load time, defer non-essential plugins (https://docs.obsidian.md/plugins/guides/load-time). [F]
- Bear (native Apple, large DB, snappy search; reviewer reports no slowdown to ~4,000 notes) and Craft (native, fast daily use, but 185,000 blocks/space limit and weaker search) from one comparison article: https://klickreflex.substack.com/p/craft-docs-vs-bear-notes [F-secondary, review not engineering].
- Things: I did not find an engineering source; no claims made.
- Pattern: native + local store + lean boot + extension code off boot path. [synthesis]

**Loro CRDT**
- B4 (260K actions) native Rust: apply ~46 ms (after PR), snapshot import ~78 µs; B4×100 import 4.4 ms (22 MB). https://github.com/loro-dev/loro/pull/1033 (Apple M5 Pro, author's numbers) [F]
- Loro 1.0: 1.66M-op real doc: import 1.15 ms full snapshot, 375 µs shallow snapshot; shallow snapshot ≈28 KB, ~300 KB mem. https://loro.dev/blog/v1.0, https://github.com/loro-dev/latch-bench (M1) [F]
- JS/WASM B4 replay: 2,271 ms (Loro) vs 2,616 ms (Yjs) in the doc table at https://loro.dev/docs/performance (column mapping uncertain from scrape; verify before citing) — **a reason to use native Rust, not WASM**. [F, low confidence on column]
- Takeaway: for note-sized docs, CRDT cost is far below a frame; compaction (shallow snapshots) keeps cold open flat. Local `/workspace/zeron/docs/research/loro-rust.md` covers API/compaction.

---
## 3. Operations and performance budget

Hardware assumption [H]: Apple Silicon laptop, 120 Hz ProMotion; also test a 60 Hz Linux/Intel iGPU box as "slow device". Warm = daemon running; cold = fresh process, OS file cache warm (also report cold-disk separately).

| # | Operation (start → end) | Target p50 | Budget p99 | Notes / basis |
|---|---|---|---|---|
| 1 | **Cold start → first paint** (exec → first presented frame showing last-viewed board from cache) | ≤150 ms [H] | ≤400 ms | Notion's reported median desktop cold-start <1 s (Electron-class) is the reference *to beat by ~5×* [F ref, H target] |
| 1b | Cold start → fully interactive (SSE connected, search ready) | ≤400 ms | ≤1 s | Connection must never gate paint |
| 2 | **Post arrives → pixels** (server commit timestamp → frame presented containing it), text/markdown post | ≤30 ms server→screen on localhost; ≤ RTT+16 ms remote | ≤100 ms local | Split: commit→SSE write, SSE read→DB/cache, cache→layout, layout→present |
| 2b | Same, with code/diff (syntax highlight) | first paint plain text ≤1 frame; highlight within ≤50 ms | ≤150 ms | Highlight as paint-after-layout (mugen lesson) |
| 2c | Same, image | placeholder at layout ≤1 frame; decoded ≤100 ms for ≤2 MP | ≤300 ms | async decode |
| 3 | **Keystroke → pixel** (key event → presented frame with glyph) in note editor | ≤1 frame (8.3 ms @120) | ≤2 frames | Include display-link wake; cf. Zed ProMotion lesson |
| 4 | **Scroll frame time** over 10k-post board, mixed content | p50 ≤3 ms, p99 ≤8.3 ms (120 Hz); ≤16.6 ms on 60 Hz | 0 dropped frames in 10 s flick scroll | Zed's measured <4 ms frames is the cited reference [F] |
| 5 | **Search** (key → results painted), 100k posts | ≤16 ms | ≤50 ms | SQLite FTS5 [H], in-process |
| 6 | **CLI publish round trip** (`sideshow post` exec → exit 0 with id) | ≤10 ms warm daemon, ≤40 ms HTTP cold | ≤30 ms / ≤100 ms | Dominated by exec + connect + fsync [H] |
| 6b | CLI publish → visible in GUI | ≤50 ms | ≤150 ms | = op 6 + op 2 |
| 7 | **Note edit sync** (agent CLI edit → human GUI sees it; and inverse) | ≤50 ms local; ≤RTT+30 ms remote | ≤200 ms | Loro delta import is µs-scale [F], rest is transport + repaint |
| 8 | Memory: idle RSS, 10k posts loaded | ≤150 MB [H] | — | Linear's all-in-memory lesson; keep metadata in RAM, bodies lazy |
| 9 | Binary size/CLI cold exec | CLI ≤10 MB, exec ≤5 ms [H] | | |

Budgets are targets, not measurements. Replace with measured baselines before gating CI.

---
## 4. Architecture choices for maximum speed

1. **Daemon-owned local-first store.** One process (the GPUI app embeds it, or `sideshowd` headless) owns SQLite (WAL mode [H]) + the Loro docs. UI, CLI, agents are clients. Mirrors zeron's "engine in-process or daemon, same protocol" model (`/workspace/zeron/ARCHITECTURE.md`) and Notion's single-writer election [F]. Server remains source of truth for posts; local is a cache with `lastSeq` (Linear's `lastSyncId` pattern [F]).
2. **Sync protocol:** server assigns monotonic `seq` per post/event; client persists `lastSeq`; on reconnect, `GET /events?since=lastSeq` replay then live. Sideshow already uses SSE with 15 s `ping` events (`sideshow/server/app.ts` ~L1801–1813). Check that `Last-Event-ID`/since is honored [H to verify].
3. **SSE vs WebSocket.** Posts are server→client streams; writes are plain POSTs. SSE is simpler, proxy/CLI-friendly, resumable, and keeps the existing server. Linear uses WebSocket for bidirectional mutation push [F]; we don't need it for posts. For Loro notes (bidirectional, small deltas): still POST deltas + SSE downstream [H]; revisit WebSocket only if measured RTT per edit matters. **Falsifiable:** WS would cut op-7 p50 by <5 ms on localhost; if >5 ms, switch.
4. **Optimistic local apply.** UI/CLI writes apply to local store first, queue and send (Linear transaction queue [F]). Perceived latency = local.
5. **In-process cache.** Hold post metadata + layout-relevant heights in RAM; bodies and decoded images behind an LRU. Don't load everything (Linear's 50k lesson [F]).
6. **Virtualized list.** GPUI `list()`/`ListState` for variable heights, `with_uniform_item_height` hint for stable scrollbar, `uniform_list` for the uniform feeds (search results, sidebar) [F]. Consider mugen-style analytic heights + persisted height cache keyed `width:rowKey` for exact cold-open offsets [F-local, optional until measured].
7. **Native text rendering.** Use GPUI shaped-text cache + glyph atlas (don't draw text yourself or via webview) [F]. Cache shaped lines per (text, font) so unchanged rows cost ~0 on re-render. Pre-parse markdown once per post (append-only content → build rows once; mugen rowsCache lesson [F-local]).
8. **Avoid readback shaders / CPU round trips.** Keep everything as instanced primitives; never `glReadPixels`-style readback or screenshot-style effects in the frame path [H, general GPU practice]. Blur/shadow via GPUI's closed-form shadow [F]. Terminal/diff/json views = styled text runs + rects, not offscreen textures.
9. **Async decode.** Image decode, syntax highlight, markdown parse of large posts, JSON pretty-print run off the UI thread; first frame shows plain text/placeholder at final size (reserve height from metadata) to avoid re-layout [F-local pattern; H for our implementation].
10. **Prefetch.** Predict next: decode images adjacent to viewport (overscan), prefetch bodies of top-N posts after boot, warm syntax grammars lazily by language seen in cache. Notion's Offline Mode default policy (Recent/Favorite + disk cap) is the model [F]; start with "last N=500 posts + pinned".
11. **Never let network gate paint.** Paint from cache first, revalidate (Notion async-WASM lesson [F]); if the cache is empty, show skeleton immediately.
12. **Presentation loop.** Draw ASAP on dirty; keep presenting ~1 s after last input to hold ProMotion rate; triple-buffer instance buffers [F: Zed]. Idle = no frames (battery).
13. **Loro notes.** Keep one `LoroDoc` per open note; apply remote deltas incrementally; store snapshots, periodically `shallow-snapshot` compact for fast cold open [F]. Text editing widget operates on the Loro text with the UI buffer as a view [H]. Run Loro natively in Rust (not WASM) [F: JS/WASM numbers].
14. **Boot discipline.** Keep boot path to: open DB → read last board → paint. Defer SSE connect, Loro doc loads, syntax grammars, search index warm, update checks (Obsidian plugins / Notion regression-hunting [F]).

### Renderers (native, per type)
markdown → pre-parsed block rows; code → plain text immediately, highlight tokens applied as style runs (tree-sitter, `crates/syntax` in zeron); diff → line-numbered rows with per-line bg rects (uniform height → `uniform_list` eligible); terminal → fixed-cell grid of styled runs from ANSI parse (cell grid cheap, uniform height); json → collapsible tree rows (virtualized); image → async decode to GPU texture, size reserved from metadata.

---
## 5. CLI speed

- **Single static binary**, no Node: Rust (musl on Linux) [H]. Node startup is tens of ms [H, verify with `hyperfine`]; this is the main reason a TS CLI can't hit ≤10 ms. The sideshow repo's TS CLI is the baseline to measure (`sideshow/bin`, bench `process` suite measures process startup: `sideshow/bench/README.md`).
- **Transport:** unix domain socket (named pipe on Windows) to the local daemon vs HTTP to server. Prediction: UDS on warm daemon saves the TCP/HTTP parse and (if the daemon is local) a network hop; expect p50 publish ≤10 ms vs ≥20 ms HTTP+TLS-less localhost [H]. Falsifier: if `hyperfine` shows <3 ms difference at p50, drop UDS and use HTTP only (less surface).
- **Daemon fallback:** if the socket is absent, CLI POSTs directly to the server (and the GUI later syncs); never block on spawning a GUI. Writes ack = durable local commit (fsync) then async upload [H].
- **Minimize work in exec:** no config scanning beyond one file, no update check, no colors/TTY probing beyond isatty, lazy-init TLS only when HTTP is used [H].
- **Agent ergonomics:** `sideshow note edit <id> --stdin` applies text diff to the Loro doc via the daemon (one request); returns version vector; supports `--wait` for sync ack. Batch mode (`post --jsonl`) amortizes exec cost for agents [H].

---
## 6. Measurement plan (Gregg-style loop)

**Principle:** define op boundary → measure the whole op first → subdivide the largest interval → one change → compare same cohort. Separate cold/warm, hit/miss; report p50/p95/p99 with sample counts; timeouts are censored; don't sum stage p95s into end-to-end percentiles.

**Instrumentation**
- One monotonic clock; spans with IDs carried through the pipeline: `post.id` → `server.commit_ts`, `sse.write_ts`, `client.recv_ts`, `store.commit_ts`, `layout.done_ts`, `present_ts` (frame callback / `on_request_frame` hook [F: Zed pattern]). Clocks: localhost = same machine, so deltas are valid; remote needs offset estimation (hypothesis, report as bounds).
- Frame timing: wrap `draw()+present()` in a span (Zed uses a `measure("frame duration")` wrapper [F]); also count missed display-link deadlines. macOS: `MTL_HUD_ENABLED=1` [F: Zed blog], Instruments Time Profiler / Metal System Trace. Linux: `perf`, flamegraph.
- Keystroke→pixel: synthetic input injection timestamp → present timestamp; external check with a 240 fps camera or photodiode for a calibration sample (hypothesis that software timestamps under-report by about one frame).
- Allocation/heap: dhat or heaptrack for RSS and alloc churn per frame (goal: ~0 allocs/frame for unchanged scene [H]).
- CLI: `hyperfine --warmup 10 --runs 200` for exec and publish; `strace -c`/`-tt` on cold exec.
- Loro: criterion benches mirroring B4-style trace plus a note-sized doc (10 KB–1 MB) for edit/import/export/shallow compaction.
- Regression gate: adopt sideshow's `bench` pattern (`bench:check` vs committed `baseline.json`) [F-local] with a Rust `criterion` + custom end-to-end harness; track `initial_page_render`-style first-paint p50 per commit and profile-diff on regressions (Notion/Palette [F]).

**Workloads:** synthetic board of 100 / 10k / 100k posts; content mix (60% markdown, 15% code, 10% diff, 5% terminal, 5% json, 5% image ≤2 MP) [H mix]; streaming burst (100 posts/s for 10 s) for op 2; 2 agents + 1 human editing a 50 KB note for op 7.

### Falsifiable predictions (technique → expected effect → what rejects it)
| # | Change | Prediction | Falsifier |
|---|---|---|---|
| P1 | Paint from SQLite cache before network | Cold-start→first-paint drops from ≥(network RTT+fetch) to ≤150 ms regardless of RTT | first paint still scales with RTT, or cache read >50 ms for last-board query |
| P2 | `uniform_list`/virtualized `list` vs rendering all | Scroll p99 frame time independent of N (10k vs 100k within ±1 ms) | p99 grows with N → something is O(N) per frame (heights sum, hit-testing) |
| P3 | Height hint (`with_uniform_item_height`) + persisted height cache | No scroll-jump on cold open; scrollbar stable at frame 1; first-paint unchanged within noise | visible jumps, or cache load >10 ms |
| P4 | Shaped-text cache per row | Scrolling frame time with unchanged rows drops ≥2× vs uncached; allocations/frame ~0 | <1.3× improvement → shaping isn't the bottleneck, look at paint/primitive count |
| P5 | Highlight/decode off-thread, placeholders at final height | Op 2b/2c first-paint ≤1 frame after arrival; no layout shift | main-thread span >4 ms during large-code post arrival |
| P6 | Keep presenting 1 s after input (ProMotion) | Keystroke→pixel p99 on ProMotion Mac ≤2 frames vs >2 without | no difference → display link not downclocking in this setup |
| P7 | SSE with `since=seq` replay vs full refetch on reconnect | Reconnect catch-up time ∝ missed events, not board size | catch-up time grows with board size |
| P8 | Rust CLI static binary vs Node CLI | exec→exit ≥5× faster (e.g. ≤5 ms vs ≥40 ms [H numbers]) | <2× improvement |
| P9 | UDS-to-daemon vs HTTP | publish p50 lower by ≥30% | <3 ms absolute difference |
| P10 | Loro shallow snapshot compaction | Note cold-open time flat as history grows 10×; ≤1 ms for typical note | open time grows with history length |
| P11 | Native Loro vs WASM in a webview | Native edit apply ≥2× faster | within 20% (then runtime choice is moot) |
| P12 | Boot deferral (SSE, search index, grammars after first paint) | First paint improves by exactly the removed work; time-to-interactive unchanged ±10% | first paint unchanged → deferred work was not on critical path |
| P13 | Batching SSE frames to ≤1 repaint per display frame | Streaming burst (100 posts/s) keeps p99 frame ≤8.3 ms vs spikes without | frame time unchanged → repaint wasn't coalescing-bound |

**Close each experiment** with: baseline vs candidate (alternate runs), uncertainty (n, CI), behavior checks (content correctness, ordering, no lost edits via Loro convergence tests), and causal confidence; a "no win" result is valid.

### Known gaps
- No Simon Last speed posts found via X. No primary-source engineering write-ups for Things; Bear/Craft only secondary.
- Loro JS/WASM table column assignment unverified.
- All budgets are targets [H]; I did not run any benchmark. Not verified that sideshow SSE supports resume-from-seq or that GPUI at the pinned rev has the cited `with_uniform_item_height` API (found on a zed GitHub blob at commit f4199ae0).


## Updated budgets: fully local app (Oct 16)
Targets, not measurements. Warm start (daemon resident): window visible <=30 ms, first content <=50 ms. True cold start: <=100 ms to first paint (floor is GPU/window setup, ~20-40 ms on macOS; tighten after measuring). Tactics: resident daemon, paint last-viewed post from cached snapshot first, lazy-load fonts/shaders beyond first screen, precompile shaders, no JS runtime/webview/network before first frame.

## Stretch goals: pushing past "fast enough" (Oct 16)
All are targets to test in the prototype, not measurements.
1. Input latency: handle input on the next frame; render at display refresh with no extra buffering. Test: keypress-to-pixel via high-speed capture or signposts; falsifier: more than one frame at p50.
2. Instant start: resident daemon + cached snapshot of last view painted first. Test: window-visible <=30 ms warm.
3. Zero-copy streaming: new posts via shared memory/mmap ring over the unix socket so CLI-to-pixels avoids parse and copy. Test: publish-to-pixel p50 <=30 ms; falsifier: no gain over plain socket at p50.
4. Precompute everything: shaped-text cache, server-side syntax highlighting, persisted list heights. Test: scroll p99 <=8.3 ms.
5. Predictive prefetch: decode next/previous posts before click. Test: post-switch p50 <=16 ms.
6. Frame pacing: skip frames when idle, draw only on change; measure GPU/battery at idle (target ~0 redraws).
Display note: the user's main display is an Apple Studio Display (refresh rate unconfirmed; 60 Hz on the original model, 120 Hz on XDR). MacBook Pro M5 internal display supports ProMotion.

## Motion rules (from Emil Kowalski, https://x.com/emilkowalski/status/2110429909160264027) (Oct 16)
- No animation on things used 100+ times a day (note switching, list selection): instant.
- Micro-interactions (hover, selection) 100-150 ms; tooltips/popovers 150-250 ms; panels 200-300 ms; page transitions 300-400 ms.
- Enter/exit use ease-out; exits about 60% of enter duration. No bounce or springs on frequent actions.
- Pause toasts on hover and when the window is unfocused.

## Mac/Metal and GPUI findings to apply
See /workspace/composio-speed-findings.md, /workspace/x-speed-findings.md, /workspace/ui-inspo/findings.md, /workspace/ui-inspo2/findings.md. Key: keep ProMotion awake for 1 s after input, notify only affected views and batch streamed updates to one per frame, NSGlassEffectView before custom blur, launchd socket activation, SQLite WAL + mmap, Instruments signposts.
