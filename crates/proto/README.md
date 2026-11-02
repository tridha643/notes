# notes-proto

The wire protocol between `notesd` and its clients (`notes`, `stage`), as Rust
types. The full spec, including verbs, errors and exit codes, is
[docs/cli-spec.md](../../docs/cli-spec.md).

## Shape

One JSON object per line, UTF-8, in both directions:

```text
→ {"v":1,"id":7,"verb":"post-list","args":{}}
← {"id":7,"ok":true,"result":[…]}
← {"id":7,"ok":false,"error":{"code":"not_found","message":"…","hint":"…"}}
← {"id":7,"event":{"seq":12,"kind":"post-updated","id":"p1","session":"s1"}}
```

- `Request` carries the verb, plus the caller's `cwd` (the caller's working directory, which picks
  its implicit session) and `agent` (the author recorded on posts and comments).
- `Verb` is one tagged enum with every verb. Args structs use `deny_unknown_fields`, so a typo'd
  field fails loudly instead of being ignored.
- Events carry ids only; clients fetch whatever they need for what changed.
- [`posts.rs`](src/posts.rs) is sideshow's data model (sessions, posts, parts, comments, assets,
  traces, settings), mirroring sideshow's `types.ts` with snake_case names.

The Swift stage-helper mirrors the helper types by hand in
[`apps/Notes/Sources/NotesCore`](../../apps/Notes/Sources/NotesCore), and both sides are checked
against the same fixtures.

## Paths

`default_socket_path()`, `default_stage_path()` and `app_support_dir()`
are the single source of truth for where things live. `NOTES_SOCK` overrides
the first; `NOTES_STAGE_DIR` overrides the stage session directory (default `~/Desktop/Stage`).
Use an absolute path. An empty override keeps the default. Choosing a directory does not move old sessions.
Stage reads the daemon's directory from `doctor`, so its environment need not match the daemon's.

## Contract tests

[`tests/fixtures/`](tests/fixtures) holds one request (and response, where it has a shape
worth pinning) per verb. [`tests/contract.rs`](tests/contract.rs) round-trips each one, so
changing a field means updating a fixture, which then shows up in review.

```sh
cargo test -p notes-proto
```
