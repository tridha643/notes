# notes-posts

The sideshow engine inside `notesd`: sessions, versioned posts made of typed parts, anchored
comments, content-addressed assets, traces, kits and settings, plus the daemon-side renderer.
It's a library that the daemon calls synchronously; it has no threads and no socket.

## Storage

- **Sessions are JSON files**, one per session, in `~/Desktop/Stage/` with assets under
  `assets/` ([`json_store.rs`](src/json_store.rs)). The store keeps every session in memory and
  rewrites a session's whole file on each change (temp file, fsync, rename), so the folder is
  always readable by hand and by other tools.
- **Settings and the render cache stay in SQLite**, on the daemon's connection.
- **Rollback.** Inside a daemon unit (`Store::begin`) every file write is journaled, so a failed
  op or atomic batch restores the folder and reloads memory (`Store::rollback`).
- **No event log here.** Write functions return the `Event`s they caused with `seq: 0`, and the
  daemon assigns sequence numbers when it appends them to its log.
- [`stage_index.rs`](src/stage_index.rs) maps posts and comments to their session, rebuilt from
  the files on reload. [`sqlite_store.rs`](src/sqlite_store.rs) and
  [`migrate_json.rs`](src/migrate_json.rs) exist only for the one-time move off the old SQLite
  tables.

## Verbs and validation

[`store.rs`](src/store.rs) implements every sideshow verb (sessions, posts, comments, assets,
traces) and mirrors sideshow's `sqlStore.ts` and `app.ts` flows. [`validate.rs`](src/validate.rs)
applies sideshow's strict-mode part checks and size limits on top of what serde already
enforces. [`kits.rs`](src/kits.rs) holds the opt-in CSS/JS bundles for html parts, verbatim
from sideshow.

## Rendering

Clients never parse code or diffs. [`render.rs`](src/render.rs) turns a part into styled spans
once and caches the result by content hash:

- **Code** goes through Pierre's Highlights ([`highlight.rs`](src/highlight.rs)), an 80-language
  lexer compiled to WebAssembly (`pierre/highlights.wasm`, Apache-2.0) and run in wasmi. The
  module compiles once per process and each thread keeps its own instance.
- **Diffs** ([`diff.rs`](src/diff.rs)) accept unified/git patches or before/after text and come
  back as highlighted hunks with old/new line numbers.
- **Terminal output** ([`ansi.rs`](src/ansi.rs)) keeps SGR colours as span classes, drops every
  other escape, and resolves `\r` the way sideshow does.

## Test and bench

```sh
cargo test -p notes-posts
cargo run --release -p notes-posts --example bench
```

Results and the highlighter comparison: [BENCH.md](BENCH.md).
