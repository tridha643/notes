# `notes` CLI spec

The CLI is how agents post to stage: diagrams, diffs, code, terminal output
and html as cards in the user's terminal pane. See `docs/stage-plan.md`.

References: sideshow's CLI verbs (`docs/ref/sideshow/bin/sideshow.js`).

## Shape

- Binary `notes` (Rust). It never touches the session folder or SQLite itself;
  every call is one request to `notesd` over a unix socket and one (or a stream
  of) responses.
- Sessions live in `~/Desktop/Stage` (override with `NOTES_STAGE_DIR`), one JSON
  file per session. SQLite (WAL) holds the event log, settings and the render
  cache. `notesd` is the only writer.
- Socket: `~/Library/Application Support/Notes/notesd.sock`, override with
  `NOTES_SOCK`. launchd owns the socket and starts `notesd` on first connect.

## Wire protocol

One JSON object per line, UTF-8, both directions.

```
→ {"v":1,"id":7,"verb":"post-show","args":{"id":"p1"},"cwd":"/path"}
← {"id":7,"ok":true,"result":{…}}
← {"id":7,"ok":false,"error":{"code":"not_found","message":"no post p1","hint":"run `notes post list`"}}
```

Streaming verbs (`watch`, `wait`) send `{"id":7,"event":{...}}` lines until
closed. The CLI turns `result` into text, or prints it as-is with `--json`.
Other tools may speak the protocol directly (e.g. `socat`). The Rust types live in `crates/proto` (`notes-proto`); they are the source of truth for field names.

## Global flags

| Flag | Meaning |
|---|---|
| `--json` | Print the `result` object; lists print one JSON object per line (NDJSON) |
| `--sock PATH` | Socket path (same as `NOTES_SOCK`) |
| `-q`, `--quiet` | Print nothing on success; rely on the exit code |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Other error (message on stderr) |
| 2 | Usage error |
| 3 | Not found |
| 6 | Daemon unreachable |

Errors go to stderr as one line plus a `hint:` line saying how to fix it.
With `--json`, the error object is printed to stdout as well.

## Verbs

### Posts

| Verb | What it does |
|---|---|
| `notes post <kind> <file\|-> [--title T] [--session S]` | Publish a post; `kind` is `markdown`, `code`, `diff`, `terminal`, `json`, `mermaid`, `image`, `trace` or `html` |
| `notes post update <id> <file\|->` | New version of a post, same card |
| `notes post part <id> add\|set\|rm ...` | Edit individual parts of a post |
| `notes post list [--session S]` / `notes post show <id>` | List posts, show one with its parts |
| `notes upload <file>` | Store an asset by content hash; prints its id |
| `notes comment <post> TEXT` | Reply on a post |
| `notes sessions` | List agent sessions |
| `notes session search <query> [--limit N]` | Sessions whose title, agent or cwd match, then those whose posts or comments contain the query |
| `notes session delete <id> [--purge]` | Move a session to the trash (kept 30 days, assets kept), or delete it and its unreferenced assets for good. Result `{id, trashed, purged, already}`; deleting a trashed session succeeds with `already: true` |
| `notes session restore <id>` | Bring a session back from the trash with posts, comments and assets. Result `{id, restored, already}`; restoring a live session succeeds with `already: true` |

An id that is neither live nor trashed is `not_found` for both verbs, so `--purge` on something already purged (or never there) is `not_found`: no tombstone is kept. Delete emits `session-deleted` (stage drops it from the sidebar), restore emits `session-restored`; repeats emit nothing.

Session data lives in `~/Desktop/Stage/`: one JSON file per session (`<date>-<slug>-<id>.json`, `"schema": 1`) plus `assets/`. notesd is the only writer; deleting a file in Finder deletes the session. Trashed sessions sit in `.trash/` as `<deleted_at>--<file>`, which the folder loader skips; notesd purges those older than 30 days (and their unreferenced assets) at startup.

### Events

| Verb | What it does |
|---|---|
| `notes watch [--since SEQ] [--kind session,post,comment]` | Stream events as NDJSON from the event log, resuming after `SEQ` |
| `notes wait --comment [--post ID] [--timeout S]` | Block until the user comments, then print it and exit |

### Batch

`notes batch [--atomic] < ops.ndjson` runs one `{"verb":…,"args":…}` per line
in a single process and connection. `--atomic` runs them in one transaction and
rolls back on the first failure. The exit code is the first failed op's, or 0.
Output is one line per op, in order:
`{"ok":true,"result":…}` or `{"ok":false,"error":…}`; an atomic batch stops
at its first error.

### Files and meta

| Verb | What it does |
|---|---|
| `notes manifest` | Machine-readable JSON of every verb, its args, output schema and exit codes |
| `notes agent-howto` | How agents post to stage (sessions, updates, reading comments) |
| `notes guide` | The stage design guide for posts, with the space the user's card gives each part; `notes post` refuses a post that can't fit it and warns about the rest |
| `notes setup` | A block to paste into AGENTS.md or CLAUDE.md that points agents at stage |
| `notes doctor` | Socket and database paths, daemon reachable, versions |
| `notes completions zsh\|bash\|fish` | Shell completion script |
| `notes help [verb]` | Short help |

## Non-goals

- No MCP server, no HTTP API, no XPC.
- No in-process plugins; extending means another client on the socket.
- No direct SQLite access from the CLI.

## Decided

- `batch` exits with the first failed op's exit code, after printing every op's result.
