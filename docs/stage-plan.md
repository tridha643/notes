# stage — sideshow as a TUI in a Rex pane (task-prep plan, v8)

Status: v8, 2026-10-25. The user picked the TUI after seeing two mocks (a log in scrollback, and a
hunk-style TUI; `scratchpad/mock/mock_tui.py`). Every Codex grill finding that still applies is
carried over from v3.1 (session 01a17533-b523-78c1-bf1b-fb60af35861d): read path, sanitization,
size caps, latency clock, comment lookup, feedback cursor.
History: v1 GPUI full-frame viewer (dropped after spike S0: scrolling) → v2/v3 append-only
log (dropped after the mock: no folding, weak boundaries) → v4 ratatui TUI → v5 OpenTUI TUI
(reverted: idle wakeups, no margin scroll, duplicated proto types, churn) → v6 = v4 restored →
v7: session data moves from SQLite to one JSON file per session in `~/Desktop/Stage/`; the
sidebar lists sessions from that folder and `/` searches them → v8: SOLID applied at the real
seams and strict API contracts (see "Engineering rules").

## Context

- **Goal & success criteria.** `stage` runs in a Rex pane beside the agent. The sidebar lists
  every session in `~/Desktop/Stage/` (newest first) with the open session's posts nested
  under it; the selected post renders as a card per part; JSON and traces fold in place; `c`
  comments (anchored to a line on code and diff parts), and the agent gets them on its next
  write or `wait`; `/` searches sessions; `:` runs commands. Images, mermaid and HTML
  snapshots draw inside their cards through kitty graphics. HTML posts (the `/teardown`
  drawing included) show a snapshot, and `o` opens the live page in the browser. A new post
  appears within p95 < 250 ms of publish (measured over 100 posts).
- **Current state.**
  1. `notesd` owns the sideshow data model and a resumable `watch` stream
     (`crates/daemon/src/main.rs:158-205`); events carry ids only
     (`crates/proto/src/lib.rs:409-425`).
  2. Sessions, posts, post versions, comments, assets and trace steps live in SQLite tables
     created in `crates/posts/src/lib.rs:110`; every read and write goes through
     `crates/posts/src/store.rs` (`session_*`, `post_*`, `comment_*`, `asset_*`, `trace_*`).
     The feedback cursor is `sessions.agent_seq` (`store.rs:321-332`).
  3. `post-show` and `post-render` take `{id, version}` (`proto/src/posts.rs:378-382`,
     `posts/src/render.rs:341`); `comment-new` takes `author: "user"` and a `CommentAnchor`
     (`Point | Rect | LineRange`, `proto/src/posts.rs:187-197,385-397`).
  4. Only `terminal` parts are sanitized (`posts/src/ansi.rs:125-200`).
  5. `PartRender::Native` covers html, markdown, mermaid, json, image, trace
     (`proto/src/posts.rs:287-303`); stage renders those from `post-show`'s raw parts.
  6. Agents speak notes verbs, not sideshow's MCP tools (`docs/ref/sideshow/server/mcpSpec.ts`).
     Notes.app already shows sideshow posts through the same verbs (commit `94141b8`).
- **Scope.** All work is in the existing `notes-gpui` repo. S0 (JSON session store) and S1
  (TUI shell with every text part kind) are cleared to build. Images, mermaid, HTML snapshots
  and the sideshow API follow in gated slices. **Non-goals:** GUI/GPUI, a log mode, remote Rex
  hosts, Linux, editing posts, Notes.app changes, running HTML inside the terminal, a
  configurable storage path, storing session data anywhere but `~/Desktop/Stage/`.

## Research conclusion

Nia auth returned `Forbidden`; grounded in current source clones, Composio Exa/context.dev/
GitHub, and live tests in Rex Beta 0.1.0.

- **The mock proves the TUI path in Rex.** Alternate screen, SGR mouse (wheel, clicks),
  synchronized output, kitty images placed per frame with `a=p` and cropped with `y`/`h` as
  cards scroll (`mock_tui.py`), OSC 52 copy; a pele → resvg diagram and a headless-Chrome HTML
  snapshot both render inside cards. Two traps found: raw mode needs absolute cursor moves
  (no `\n`), and a SIGWINCH handler must not draw (re-entrant stdout write crashed it).
- **S0 (GPUI full frames)** rendered crisply but scrolled badly; a cell TUI repaints only
  changed cells and scrolls a line per wheel tick, which is the normal TUI feel.
- **Rex kitty support (measured).** `t=s`, `t=f`, `t=d` (large single payloads → `EINVAL`),
  `a=p` placements, z-index, `U=1`; mouse 1000/1002/1003/1006/1016, 2026 sync, 2048 resize,
  kitty keyboard flags 31. New panes report 80x24 / 8x16 px until sized (real 14x30 px here).
- **Convergence.** Leaf (Rust: ratatui, pulldown-cmark, pele → resvg, kitty placements, diff
  comments to an agent pane) is this design in production form; hunk is the UI reference the
  user picked.
- **HTML snapshots.** Warm WKWebView `takeSnapshot` measured 4-21 ms median (cold 1.6-2.8 s);
  headless Chrome `--screenshot` produced the mock's teardown snapshot. teardown is one
  self-contained HTML page, so a snapshot shows its default view; the live page stays in the
  browser.
- **Desktop folder.** iCloud Desktop sync is off on this Mac (`FXICloudDriveDesktop = 0`), so
  files won't be evicted or conflict-copied. `~/Desktop` is TCC-protected; whether the
  launchd-run `notesd` can write there without a grant is unverified (first S0 step).

## Locked decisions

| Topic | Decision |
| --- | --- |
| Home | New crate `crates/stage` in `notes-gpui`; `notesd` stays the only authority. |
| Session store | One JSON file per session in `~/Desktop/Stage/`, the only place session data lives. Path is fixed (`$HOME/Desktop/Stage`), created by notesd on first write, no env var or setting. File name `<yyyy-mm-dd>-<slug>-<id>.json` (local creation date, slug from the session title or agent, the full id so names can't collide even on a case-insensitive disk; a rename keeps the file name). |
| File shape | `{id, title, created_at, agent_seq, posts: [{id, title, versions: [{version, at, parts}]}], comments: [...], traces: [...]}`. Every version is kept (no 20-version prune). Assets are shared by content hash across sessions, so they live once in `~/Desktop/Stage/assets/<sha256>` with `<sha256>.json` metadata; a session delete removes its assets unless another post still references them. |
| Writer | notesd is the only writer: serialize the session, write a temp file in the same folder, `fsync`, `rename`. One write per post/comment/rename event. Agents, the CLI, Notes.app and stage all go through notesd verbs. |
| store.rs swap | `crates/posts/src/store.rs` keeps its verbs and takes a `notes_posts::Store` (connection + folder) instead of a bare `Connection`. Every session is held in memory, loaded at startup; `stage_index.rs` maps post/comment ids to sessions. Inside a daemon unit, `Store::begin/commit/rollback` journal file writes so a failed op or atomic batch restores the folder and reloads memory. The verbs, the wire format and Notes.app don't change. |
| What stays in SQLite | `events` (ids-only notification log for `watch`; derived, not session data), `render_cache` (keyed by content hash), `settings`, and the notes vault tables. The `sessions/posts/post_versions/comments/assets/asset_refs/trace_steps` tables are dropped after migration. |
| Migration | On first start after S0, notesd exports every SQLite session (versions, comments with their seqs, the feedback cursor, traces, assets) to JSON, re-reads each file and compares, then drops the old tables. A failed export leaves the tables in place, retries next start, and `notes doctor` reports it. An unreadable folder (e.g. macOS privacy) doesn't stop notesd: notes keep working, sessions are empty, `doctor` shows the error. |
| Folder watch | notesd watches `~/Desktop/Stage/` with kqueue (`EVFILT_VNODE` on the directory). A session file added or removed outside notesd (Finder, `rm`) becomes `session-created`/`session-deleted`; a file that fails to parse is skipped and listed by `notes doctor`. Hand edits to a file's content aren't supported. |
| UI framework | ratatui, not OpenTUI. OpenTUI 0.7.0 (2026-10-25) was evaluated: its Zig renderer diffs rows and its kitty image placement/crop/occlusion code is ahead of anything we'd hand-write, but Bun wakes 6-14 times per 20 s idle (our rule is zero), its ScrollBox repaints the column per tick (no DECSLRM), it compiles to a 72 MB binary that leaves a 4.2 MB dylib in `$TMPDIR` per launch, and it shipped three breaking minors in four months. Use its `native/src/terminal-image.zig` and `renderer.zig:200-260` as the reference for our image placement. Flip only if zero idle wakeups and margin scrolling stop mattering. |
| UI | `ratatui` + `crossterm`. Kitty query and size probes run before crossterm owns stdin, because crossterm misparses APC replies. Alternate screen, raw mode, SGR mouse, 2026 sync. |
| Draw first, probe after | Startup never waits on terminal replies. Enter raw mode and the alt screen, write the probe queries, draw the first frame with the default 8x16 cell px, then read the replies (100 ms timeout) before crossterm's event reader starts. Drawing only writes stdout, so it doesn't conflict with the probe reading stdin. When the replies land, update cell px and redraw. S1 has no images, so the default cell px never shows; S3 image cards wait for the real value. Bytes that aren't probe replies (keys typed in the first ~100 ms) are dropped. |
| Clicking a visual | Clicking an image card (mermaid, image, html snapshot) opens its source: mermaid as SVG and images as files in the default viewer, html in the browser. `enter` on a focused image card opens a full-pane view inside the TUI. |
| Layout | Header line · sidebar (sessions newest first; the open session expands to its posts: title, version, comment count) · post title bar · one bordered card per part (label in the border) · comments under the cards · keybar footer. Borders `#7f849c`, selected post accent blue. |
| Opening | `stage` opens the most recently active session; `stage --session S` opens S. Any session can be opened from the sidebar or `/`. |
| Keys | `j/k` move through sidebar rows (sessions and posts) · `enter` open session/post · `↑↓` scroll or move node/line · `enter`/`space`/click fold · `c` comment · `o` open html · `v` versions · `/` search sessions · `:` command · `q` quit. |
| Search (`/`) | `session-search {query, limit?}`: case-insensitive match on session title, agent or cwd first, then on post titles, part text and comments, newest first within each rank. A linear scan over the sessions notesd already holds in memory (no second fff index; add one if it shows up in profiles). Results are sessions with the first matching post and line. |
| Commands (`:`) | `open`, `copy` (OSC 52, raw part), `versions`, `rerender`, `help`. New commands are entries in one table. |
| Sockets | Two connections: one held by `watch`, one for requests (`daemon/src/main.rs:248-252`). |
| Subscription | `watch {since, kinds: ["post","comment","session"]}`; session events update the sidebar; post and comment events rebuild only the open session's cards, other sessions just get an unread dot. |
| Bootstrap | After the first frame: read `doctor.seq` (the latest committed event), open the watch from that seq, then `session-list` and `post-list {session}` for the open session; events and listings overlap at most, never gap, and `(post, version)` dedupes the overlap. |
| Read path | `post-show {id, version}` + `post-render {id, version}`; latest version by default, `v` to pick another. |
| Updates | A new version replaces the card content in place; sidebar shows `v<N>` and a dot until viewed. |
| Comments | `comment-new {post, text, author: "user", anchor}`; anchor = `LineRange` when the cursor is on a code/diff line, else none. stage never calls `wait` or writes posts, so it can't consume the agent's feedback cursor. Comment events → `comment-list {session}` matched by id. |
| Sanitization | Sanitize every untrusted fragment (drop ESC, C0 except `\t`, C1) before styling; content escapes are never interpreted; `terminal` spans styled from `class` only. |
| Size caps | Per part 400 lines / 64 KiB, per post 1500 lines / 256 KiB, then a "… N more lines" row with `:copy` to get the raw part. |
| Image sizing | Fit each visual to its card: the largest size within the card width and half the visible height, never upscaled past native, aspect kept, centered. For mermaid, render both horizontal and vertical layouts (pele `maxWidth`) and keep the one that scales larger under those caps (a long chain goes wide and short). Click or `enter` for the full-size view. Cell px comes from `CSI 16t`, re-queried on every resize. |
| Bars | Header, title bar, sidebar rows and keybar fill their full width with one background (pad with the row's background, never the terminal default). |
| Images | Transmit once per (content hash, width) with `t=f` from `~/Library/Caches/stage/`, place per frame with `a=p` and crop rows with `y`/`h`; delete placements (`a=d,d=a`) each frame; ids are per-process counters (no hash collisions). |
| HTML | Snapshot the part to PNG (warm WKWebView in a tiny Swift helper, or headless Chrome if the helper slips), cache by content hash, show in the card; `o` writes the part to the cache and runs `open` (default browser). Never run HTML in the terminal. |
| Frame output | Diff against the last frame per row, with the sidebar and main columns tracked separately; a frame with no change writes nothing. Every frame is wrapped in `CSI ?2026h/l`. ratatui's buffer diff gives this for free; keep it, don't force full redraws. |
| Scrolling | Deferred in S1 (ratatui's diff: 3.66 KiB per tick, measured; `ponytail:` note in `ui.rs`). Target: when only the scroll offset changed, set margins on the main column (`DECSTBM` + `DECSLRM` with `?69h`; Rex supports 69), shift with `CSI S`/`CSI T`, and draw only the exposed rows. Measured in the mock: 15.5 → 0.7 KiB per wheel tick, rows verified contiguous in Rex. |
| Image updates | Place with a fixed placement id (`i=…,p=1`) so a move or crop replaces the placement in place; delete only images that left the view. No delete-all per frame (it caused 1 delete per frame and flicker risk). |
| Event loop | Block on stdin + a self-pipe (`signal.set_wakeup_fd`) for SIGWINCH + the notesd socket; wake on a timer only when a toast or deadline is due. Zero idle wakeups (mock: 12 per 3 s → 0); resize repaints in ~23 ms. Never draw in a signal handler. |
| Input coalescing | Drain all pending input, then draw once (240 pasted keys → 1 frame). |
| Card cache | Built card lines cached per (post, version, comments, folds, cursor, width, cell px); draw p95 10.8 → 4.3 ms in the mock. |
| Navigation feel | Each post remembers its scroll and cursor; title bar shows `↕ first–last / total` on long posts; `space`/`b`, `PgDn`/`PgUp`, `g`/`G`; diff rows shade the full width. |
| Animation | App-driven frames: render each frame in stage, write it to a fresh POSIX shm object, send `a=T,t=s,i=<id>,p=1,q=2,C=1` inside `CSI ?2026h/l` so it replaces the previous frame in place. Cap 60 fps, demand-driven: the loop gets a frame timer only while something on screen animates, so idle stays at zero wakeups. Pause when the pane loses focus (`?1004` focus events) or the card scrolls out of view. Measured in Rex: 60.1 fps sustained, frame cost p50 0.30 ms / p95 0.86 ms (480x160), Rex app ~25% CPU while animating. Kitty's native animation (`a=f` frames + `a=a` loop) also plays in Rex and suits fixed loops like spinners. |
| Mermaid | Its own design pass (pele headless measurer vs merman); until then the card shows the source. |
| Latency | Benchmark records ns before each `post-new`; stage logs `post id, version, painted ns`; joined on post id. |
| Sideshow API | Tool-by-tool compatibility matrix vs `mcpSpec.ts` before any shim code. |

## Engineering rules

SOLID is applied where there's a real seam, not as a trait per struct: a trait exists only
when two implementations exist today (or a test needs to swap one), and an exhaustive `match`
is preferred over dynamic dispatch where the set of cases is closed.

| Principle | Where it applies | Rule |
| --- | --- | --- |
| Single responsibility | every module | One reason to change per module. Storage: `json_store.rs` (file format + atomic IO), `stage_index.rs` (id → file lookup), `migrate_json.rs` (one-time export). stage: `app.rs` holds state and decides, `ui.rs` draws, `main.rs` does IO, `sanitize.rs` is the only path from post data to cells. |
| Open/closed | `:` commands, part kinds | Commands are rows in one table, so a new command touches no existing code. Part kinds stay a closed enum with exhaustive `match`: a new kind is a compile error at every site that must handle it, which is safer than a registry that silently skips it. |
| Liskov substitution | — | Built without a trait: the old SQLite store survives only as a one-time exporter (`sqlite_store.rs`), so a `SessionRead` trait would have one implementation, which the rule below forbids. Migration correctness is checked by exporting legacy rows and reading them back through the live verbs. |
| Interface segregation | stage client | stage talks to notesd through `StageClient`, which exposes only the verbs it uses (`watch`, `doctor` for the start seq, `session-list`, `session-search`, `post-list`, `post-show`, `post-render`, `comment-new`, `comment-list`), so it can't call `wait` or write posts even by mistake. |
| Dependency inversion | notesd → store, stage app → IO | The daemon calls `notes_posts` verbs and never touches session files itself. stage's `App` never does IO: `App::update(&mut self, Msg) -> Vec<Effect>` returns effects (`Request`, `Copy`, `Open`, `Redraw`) that `main.rs` performs, so every key, mouse and event path is unit-tested without a socket or terminal. |

**Strict API contracts**

| Contract | Rule |
| --- | --- |
| Wire protocol | `notes-proto` is the single definition. Every verb argument struct gets `#[serde(deny_unknown_fields)]`, so a typo'd or stale field is a `usage` error naming the field, not silently ignored. The `Request` envelope flattens `verb` (`proto/src/lib.rs:51`), which serde can't combine with that attribute, so notesd checks the envelope's top-level keys against an allowlist (`v`, `id`, `verb`, `args`, `cwd`, …) before parsing. `PROTOCOL_VERSION` (already enforced at `daemon/src/main.rs:237`) bumps on any breaking change. |
| Typed ids | `SessionId`, `PostId`, `CommentId`, `AssetId` newtypes (`#[serde(transparent)]`) replace the 16 `String` id fields in `proto/src/posts.rs`, so a post id can't be passed where a session id is expected; wire shape is unchanged. |
| Errors | Every failure returns an `ErrorCode` variant (already typed, `proto/src/lib.rs:384`); no new stringly errors. A daemon test pins the codes for each sideshow verb's failure modes (missing ids → `not_found`, bad args → `usage`). |
| Boundary validation | Parse, don't validate: notesd turns wire input into validated types once (`posts/src/validate.rs`), and the store only accepts the validated types. |
| Contract fixtures | `crates/proto/tests/fixtures/<verb>.request.json` for all 44 verbs and `<verb>.response.json` for the sideshow result shapes. Rust round-trips them byte-for-byte; Notes.app's Swift decoder decodes the same files. Changing a fixture is the visible signal of a contract change in review. |
| Session file format | Each file carries `"schema": 1`; reads are strict (`deny_unknown_fields`); an unknown schema or field means the file is skipped and reported, never rewritten. A format change bumps `schema` and adds a migration. |

## Program Map (S0 + S1)

### Call-stack tree

```diff
 notesd start
+  stage_dir(): $HOME/Desktop/Stage, create if missing; write probe (TCC check)
+  migrate_sqlite_sessions(): export → verify round-trip → drop tables (once)
+  index: scan *.json → {session id → file, post id → session}
+  kqueue: EVFILT_VNODE on the folder → rescan names → session-created|deleted events
 store.rs session_*/post_*/comment_*/asset_*/trace_*  (same signatures)
+  read: index → parse file (cached per mtime)
+  write: mutate → serialize → temp + fsync + rename → events row → notify

 stage [--session S]          (Rex pane; opened by `rex split --split=right -- stage`)
+  probe::send(): kitty a=q, CSI 16t cell px, 2048 on   (write queries only)
+  first frame (default cell px)                       (never waits on replies)
+  probe::read(100 ms) → cell px, kitty support        (before crossterm's event reader)
+  conn_w: Watch{since: None, kinds: [post, comment, session]}  → channel
+  conn_r: SessionList → sidebar; PostList{session: open} → nested posts
+  App::run (ratatui loop, redraw on input / event / resize flag)
+    event session-created|deleted → sidebar
+    event post-created|updated (open session, unseen (id,v)) → load(id, v); other sessions → dot
+    event post-deleted → remove from sidebar
+    event comment-created → CommentList{session} → match id → attach to post
+    load(id, v): PostShow + PostRender → Vec<Card>   (cards built once per version, cached)
+      card::markdown | code | diff | terminal | json(tree) | trace | image* | mermaid* | html*
+      (* placeholder text in S1)
+    key / → SessionSearch{query} → results list → enter opens session
+    key c → input line → CommentNew{post, text, author: "user", anchor}
+    key : → command table
+  on watch EOF → reconnect with since = last_seq
```

### File-tree diff

```diff
 crates/
   posts/src/
~    store.rs              # every sideshow verb over Store (folder + settings)
+    json_store.rs         # Store: file shape, atomic write, journal, refresh, doctor
+    stage_index.rs        # session id → file, post id → session
+    sqlite_store.rs       # read-only export of the legacy SQLite tables
+    migrate_json.rs       # one-time SQLite → JSON export + verify
+  posts/tests/stage.rs  # folder layout, rollback, outside changes, migration, search
~    lib.rs                # drop session tables after migration
   daemon/src/
~    main.rs               # stage dir + kqueue folder watch; session-search verb
~    search.rs             # second fff index over ~/Desktop/Stage/*.json
   proto/src/
~    lib.rs                # SessionSearch request/response; deny_unknown_fields
~    posts.rs              # id newtypes; deny_unknown_fields
+  proto/tests/contract.rs  # fixture round-trips, unknown-field rejection
+  proto/tests/fixtures/    # <verb>.request.json / <verb>.response.json
+  stage/Cargo.toml        # ratatui, crossterm, comrak, serde_json, notes-proto
+  stage/src/main.rs       # args, probe, event channel, reconnect; performs Effects
+  stage/src/client.rs     # StageClient: the eight verbs stage may call
+  stage/src/app.rs        # App::update(Msg) -> Vec<Effect>; no IO
+  stage/src/ui.rs         # layout: header, sidebar, title bar, cards, comments, keybar, search
+  stage/src/cards.rs      # Part + PartRender → styled lines per kind; json/trace fold trees
+  stage/src/sanitize.rs   # the only path from post data to cells; size caps
+  stage/src/commands.rs   # `:` command table
+  stage/src/latency.rs    # painted-ns log
```

### Key types / signatures

```rust
// proto: typed ids, same wire shape
#[serde(transparent)] struct SessionId(String);   // also PostId, CommentId, AssetId

// posts: the store; every verb takes &Store
pub struct Store { pub db: Connection, root: PathBuf, stage: RefCell<Stage>, .. }
impl Store {
    fn open(db: Connection, root: PathBuf) -> Result<Store>;   // load folder, migrate legacy tables once
    fn begin(&self); fn commit(&self); fn rollback(&self);      // journal per daemon unit
    fn refresh(&self) -> Result<Vec<Event>>;                    // files added/removed outside notesd
}
#[serde(deny_unknown_fields)]
struct SessionFile { schema: u32, id: SessionId, title: String, created_at: i64, agent_seq: u64,
                     posts: Vec<PostRec>, comments: Vec<Comment>, traces: Vec<TraceStep> }
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()>;   // temp + fsync + rename

// stage: pure state machine + narrow client
enum Msg { Key(KeyEvent), Mouse(MouseEvent), Resize, Notesd(Event), Reply(Response) }
enum Effect { Request(StageRequest), Copy(String), Open(PathBuf), Redraw }
impl App { fn update(&mut self, msg: Msg) -> Vec<Effect>; }
enum StageRequest { Watch(..), SessionList, SessionSearch(..), PostList(..), PostShow(..), PostRender(..), CommentNew(..), CommentList(..) }

// stage: reuse notes_proto::{Request, Verb, Event, posts::{Post, Part, PartRender, CommentAnchor, ...}}
struct Card { label: String, body: CardBody }
enum CardBody { Lines(Vec<Line<'static>>), Tree(FoldTree), Placeholder(String) }
struct FoldTree { nodes: Vec<Node>, open: HashSet<NodePath> }     // json + trace
enum Mode { Nav, Search { query: String, hits: Vec<SessionHit> }, Comment { input: String, anchor: Option<CommentAnchor> }, Command { input: String } }
fn build_cards(post: &Post, render: &PostRender) -> Vec<Card>;   // sanitized + capped inside
fn sanitize(s: &str) -> Cow<'_, str>;
struct Command { name: &'static str, run: fn(&mut App) -> Toast }
```

### State ownership and invariants

| State | Authority | Writers / readers | Rule |
| --- | --- | --- | --- |
| Sessions, posts, versions, comments, traces, assets | `~/Desktop/Stage/*.json` (+ `.assets/`) | notesd only writes; everyone else goes through verbs | Every write is temp + fsync + rename; a reader never sees a half-written file. |
| Feedback cursor `agent_seq` | the session's JSON file | post writes and `wait` advance it | stage never calls either. |
| Event log, render cache, settings | SQLite | notesd | Derived or app config; deleting the event log only costs watch resume. |
| Cards per (post, version) | stage memory | stage | Immutable once built; a new version builds new cards. |
| Folds, scroll, selection, input | stage memory | stage | Per pane; lost on exit. |
| Image cache files | `~/Library/Caches/stage/` | stage | Keyed by content hash + width px; deletable anytime. |
| Terminal output | stage | — | Nothing from post data reaches the terminal without `sanitize`. |

### Delivery slices

| Slice | Observable outcome | Acceptance check | Depends on |
| --- | --- | --- | --- |
| **S0** | session data lives only in `~/Desktop/Stage/`; proto contracts are strict | TCC write probe passes under launchd; contract fixtures for every verb pass; Notes.app decodes every fixture; existing `posts`/`daemon` tests pass unchanged; migration round-trips every session; a killed write leaves the previous file intact; Notes.app still shows posts | — |
| **S1** | TUI with sessions sidebar, cards for markdown/code/diff/terminal/json/trace, folding, live updates, `/` session search | tests below; 100 scripted posts p95 publish→painted < 250 ms; manual Rex check (mouse, resize, zoom) | S0 |
| S2 | comments (`c`, line anchors) + `:` commands + `v` versions | comment reaches the agent via its next write and via `wait`, both orders; anchors round-trip | S1 |
| S3 | images in cards (asset images) | placement + crop while scrolling; resize redraws; 20 images in a post | S1 |
| S4 | HTML snapshot + `o` | sideshow `issues` example and the teardown page snapshot in their cards; `o` opens the browser | S3 |
| S5 | mermaid | design pass first, then pele or merman into the S3 image path | S3 |
| S6 | sideshow-compatible API | matrix agreed first | S1 |
| S8 | animated visuals | a card animates at 60 fps in Rex; idle wakeups stay 0 when nothing animates; animation pauses on focus loss and when scrolled off | S3 |

S7 (JSON history + search) is folded into S0 and S1.

## Exhaustive case coverage (S0 + S1)

| Case | Behavior |
| --- | --- |
| `~/Desktop/Stage` missing | notesd creates it on start and before each write |
| TCC denies `~/Desktop` | notesd refuses session writes with an error naming the grant; `notes doctor` reports it |
| Session created | new `<date>-<slug>-<id6>.json`; sidebar gets a row |
| Session renamed | title changes inside the file; file name stays |
| File deleted in Finder | `session-deleted`; sidebar drops it; open session shows a "session ended" banner |
| File dropped in from elsewhere | parsed and indexed like any session |
| File fails to parse | skipped, listed in `notes doctor`, never overwritten |
| notesd killed mid-write | the old file survives (rename is atomic); stray temp files are removed on start |
| Migration fails | SQLite tables stay; notesd keeps serving from SQLite and `notes doctor` reports it |
| Part `markdown` | comrak → styled lines (headings, lists, code blocks, tables aligned, links as text + url) |
| Part `code` | `PartRender::Code` spans, `lineStart` gutter |
| Part `diff` unified / split | spans with +/- backgrounds; split renders as unified with a "split" label until S3+ |
| Part `terminal` | spans from `class`, re-sanitized |
| Part `json` | fold tree; depth ≤ 1 open by default; `enter`/click toggles |
| Part `trace` inline / asset | fold tree of steps; asset form shows step count + `:copy` |
| Part `mermaid` / `image` / `html` | placeholder line in S1 (source / alt / "snapshot in S4") |
| Unknown kind | "unsupported part" card |
| Oversized part / post | capped with a "… N more lines" row |
| Escapes in any text | dropped by `sanitize` |
| `post-created` | add under its session (newest last), auto-select if the user is on the latest post of that session |
| `post-updated` | rebuild that post's cards; keep scroll if it's the open post |
| `post-deleted` | remove; if open, select the neighbor and toast "deleted" |
| Event for a session that isn't open | unread dot on that session row, nothing rebuilt |
| Duplicate `(id, version)` | ignored |
| `comment-created` / `comment-deleted` | refresh that post's comments |
| `/` with no hits | "no sessions match" row; `esc` returns to the sidebar |
| `--session` unknown | error message, exit 2 |
| notesd absent / watch EOF | banner "reconnecting", reconnect with `since = last_seq` |
| Resize / Rex zoom | flag set in SIGWINCH, redraw in the loop (never draw in the handler) |
| Fresh pane 80x24 | first layout after the first real size |
| Probe replies late or missing | first frame already drawn; after 100 ms keep 8x16 cell px and treat kitty as absent |
| No kitty | S1 unaffected; S3+ fall back to placeholders |
| `q` / Ctrl-C | restore terminal (leave alt screen, mouse off, delete placements) |

## Test matrix

| File | Case | Asserts |
| --- | --- | --- |
| `posts` existing tests | every store verb | pass unchanged against the JSON store (temp dir as the stage folder) |
| `proto/tests/contract.rs` | every verb's fixture pair | parse → serialize is byte-identical; each fixture parses into the expected variant |
| `proto/tests/contract.rs` | unknown field on any request | rejected with `invalid`, field named in the message |
| `proto/tests/contract.rs` | error codes per verb | each verb returns only its documented `ErrorCode`s |
| `posts/tests/stage.rs` | layout, rollback, temp files, unreadable files, refresh, migration, search | see the file |
| `json_store.rs` | `schema` | unknown schema or unknown field → skipped and reported, file untouched |
| `json_store.rs` | atomic write | a crash between temp write and rename leaves the old file; stray temps cleaned |
| `json_store.rs` | index | scan finds every session; post id → session lookup; duplicate ids rejected |
| `json_store.rs` | foreign files | unparseable file skipped and reported, never rewritten |
| `migrate_json.rs` | SQLite → JSON | every session, version, comment, trace and asset round-trips; tables dropped only after verify |
| `daemon` (temp dir) | folder watch | `rm` of a session file emits `session-deleted`; a copied-in file emits `session-created` |
| `daemon` (temp dir) | `session-search` | title fuzzy hit and content hit both return the session |
| `sanitize.rs` | ESC, OSC 8/52, APC, DCS, C1 in every text source | no ESC or C1 survives |
| `sanitize.rs` | caps | per-part and per-post caps, dropped counts |
| `cards.rs` | one ratatui `TestBackend` golden per part kind | buffer matches golden |
| `cards.rs` | json/trace folding | toggling a node changes only that subtree |
| `app.rs` | `App::update` (no socket, no terminal) | j/k across sessions and posts, `/` search flow, wheel, click-to-fold, resize → exactly the expected `Effect`s; stage never emits a write other than `CommentNew` |
| `main.rs` (temp notesd) | bootstrap race, dedupe | each (id, v) built once |
| `main.rs` (temp notesd) | reconnect | resumes at `last_seq`, no gap, no duplicate |
| `main.rs` (temp notesd) | feedback cursor | `agent_seq` in the session file unchanged after running stage |
| `scripts/stage-latency.sh` | 100 posts | p50/p95 publish→painted, JSON write included |
| `stage/benches/frames.rs` (pty harness, port of `scratchpad/mock/bench.py`) | fixed 106-input scenario | time to first draw (excludes the probe timeout), KiB per input by kind, draw p50/p95, image ops, idle wakeups = 0, resize repaint < 50 ms; fails if any regress past the v6 mock numbers |

## Verification

1. `cargo test -p notes-posts -p notesd -p stage` · `cargo clippy -p stage -- -D warnings`
2. `scripts/stage-latency.sh`
3. Manual: `scripts/install.sh` then confirm `~/Desktop/Stage/` fills as an agent posts; delete a
   session file in Finder and watch it leave the sidebar.
4. Manual in Rex: `rex split --split=right -- stage`; publish one post of each kind; fold
   JSON; `/` find an older session; scroll with the wheel; zoom and unzoom the pane; quit and
   confirm the shell is clean.

## Risks / notes

- **TCC on `~/Desktop`.** A launchd agent writing to Desktop may be denied without a Files &
  Folders grant, and a non-GUI process may never see the prompt. S0 starts with a write probe
  under launchd; if it's denied, the installer has to walk the user through granting notesd
  access.
- **Whole-file rewrites.** Each post or comment rewrites the session file. Fine for normal
  sessions (tens to hundreds of KB); if a session grows past a few MB, split posts into their
  own files under a session folder.
- **Hand edits.** Files are readable and deletable, but editing one by hand while notesd runs
  can be overwritten by the next write; notesd re-reads on mtime change but doesn't merge.
- **Repo CLAUDE.md** still says SQLite holds sideshow's tables; update that line when S0 lands,
  and add the engineering rules above so other agents follow them.
- **`deny_unknown_fields` can break callers.** Notes.app or an older `notes` CLI that sends an
  extra field starts getting `invalid`. Notes.app's requests are checked against the fixtures
  before the flag lands. serde doesn't support `deny_unknown_fields` with `#[serde(flatten)]`, which is why the
  envelope uses a key allowlist instead.
- **Over-abstraction.** The rules allow a trait only with two real implementations or a
  test that swaps one; review rejects single-implementation traits.
- **No Rex scrollback in the TUI.** History lives in the sidebar instead; `:copy` gives raw text.
- **crossterm vs APC replies.** crossterm misparses kitty APC replies (gpuix notes), so probes
  run before crossterm owns stdin.
- **HTML snapshots** show the page's default state only; anything interactive needs `o`.
- **WKWebView helper** is a small Swift app (warm pool, cold start 1.6-2.8 s); headless Chrome
  is the fallback the mock already used.
- **Mermaid metrics** (pele measurer vs resvg font) are unverified; S5 starts with a design pass.
- **Feedback semantics.** notesd's shared `agent_seq` differs from sideshow's `afterSeq`; S6 must
  reconcile it.
