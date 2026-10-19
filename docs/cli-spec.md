# `notes` CLI spec (P0 draft, 2026-10-18)

Notes live only in the daemon's SQLite, so the CLI is the only way in for agents.
It must cover what `cat`, `grep` and `sed` give them for plain files, with a
stable output contract and safe concurrent writes.

References: Obsidian's CLI (~100 verbs, `format=json`, one JSON line over a
socket; see `obsidian-internals.md`), Claude Code's Read/Edit/Grep semantics,
sideshow's CLI verbs (`docs/ref/sideshow/bin/sideshow.js`).

## Shape

- Binary `notes` (Rust). It never opens SQLite; every call is one request to
  `notesd` over a unix socket and one (or a stream of) responses.
- Socket: `~/Library/Application Support/Notes/notesd.sock`, override with
  `NOTES_SOCK`. launchd owns the socket and starts `notesd` on first connect.
- Budget (target, unmeasured for the real CLI): ≤10 ms per warm call. The
  measured process floor on the M5 is ~6.5 ms; a socket round trip is ~8 µs.
- Milestone 1 verbs keep working: `new`, `list`, `append`, `show` (alias of `read`).

## Wire protocol

One JSON object per line, UTF-8, both directions.

```
→ {"v":1,"id":7,"verb":"edit","args":{"ref":"12","old":"foo","new":"bar"},"cwd":"/path"}
← {"id":7,"ok":true,"result":{"id":"12","version":43}}
← {"id":7,"ok":false,"error":{"code":"ambiguous_match","message":"\"foo\" matches 3 times (lines 4, 9, 30)","hint":"add surrounding text or pass --all"}}
```

Streaming verbs (`watch`, `wait`) send `{"id":7,"event":{...}}` lines until
closed. The CLI turns `result` into text, or prints it as-is with `--json`.
Other tools may speak the protocol directly (e.g. `socat`).

## Global flags

| Flag | Meaning |
|---|---|
| `--json` | Print the `result` object; lists print one JSON object per line (NDJSON) |
| `--if-version N` | On writes: fail with exit 5 unless the note is at version N |
| `--sock PATH` | Socket path (same as `NOTES_SOCK`) |
| `-q`, `--quiet` | Print nothing on success; rely on the exit code |

## Note references

A `<ref>` is a note id (`12`), an exact title (case-insensitive), or `@last`
(most recently updated). A title that matches more than one note fails with
exit 4 and lists the candidates with their ids.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Other error (message on stderr) |
| 2 | Usage error |
| 3 | Not found |
| 4 | Ambiguous (title ref or edit match) |
| 5 | Version conflict (`--if-version` mismatch); stderr carries the current version |
| 6 | Daemon unreachable |

Errors go to stderr as one line plus a `hint:` line saying how to fix it.
With `--json`, the error object is printed to stdout as well.

## Verbs

### Read

| Verb | What it does |
|---|---|
| `notes list [--tag T] [--sort updated\|created\|title] [--since TIME] [--limit N]` | List notes: id, title, updated time |
| `notes read <ref> [--lines A:B] [-n]` | Print the body; `--lines` slices, `-n` prefixes line numbers like `cat -n` |
| `notes search <query> [--regex] [--title] [--tag T] [-C N] [--limit N]` | grep-style results `id:title:line: text`. FTS5 trigram by default (substring, case-insensitive); queries under 3 characters fall back to a scan; `--regex` filters FTS candidates with a regex; `-C` adds context lines |
| `notes links <ref>` / `notes backlinks <ref>` | Outgoing `[[links]]` and notes linking here |
| `notes prop <ref>` | List properties |

`read --json` returns `{id, title, version, body, line_count, props, created_at, updated_at}`.

### Write

| Verb | What it does |
|---|---|
| `notes new <title> [TEXT\|-] [--prop k=v]... [--tag T]...` | Create a note; prints its id |
| `notes edit <ref> --old S --new S [--all]` | Exact find-and-replace. `--old` must match exactly once unless `--all`; otherwise exit 4 with the match count and line numbers |
| `notes edit <ref> --from FILE\|-` | Several edits from JSON `[{"old":…,"new":…}]`, applied all-or-nothing |
| `notes append <ref> TEXT\|- [--under HEADING]` | Add to the end, or to the end of a section |
| `notes prepend <ref> TEXT\|-` | Add at the top, after any properties |
| `notes write <ref> -` | Replace the whole body from stdin; requires `--if-version` or `--force` |
| `notes rename <ref> <new title>` | Rename; updates `[[links]]` that point to it |
| `notes prop set <ref> k=v...` / `notes prop rm <ref> k...` | Set or remove properties |
| `notes delete <ref> [--purge]` | Move to trash (restorable), or delete for good |
| `notes restore <ref>` | Bring a note back from trash |

`edit`, `append` and `prepend` don't need `--if-version`: the daemon applies them
as Loro operations and re-checks `--old` at apply time, so concurrent agents merge
instead of overwriting. Every write returns the new `version`.

### History

| Verb | What it does |
|---|---|
| `notes history <ref> [--limit N]` | Versions with time and author (human, or the agent name from `NOTES_AGENT`) |
| `notes diff <ref> [--from V] [--to V]` | Unified diff between versions (default: previous to current) |
| `notes revert <ref> --to V` | Restore an earlier version as a new version |

### Sideshow posts

Sideshow's verbs live under `post` so they don't collide with note verbs.

| Verb | What it does |
|---|---|
| `notes post <kind> <file\|-> [--title T] [--session S]` | Publish a post; `kind` is `markdown`, `code`, `diff`, `terminal`, `json`, `mermaid`, `image`, `trace` or `html` |
| `notes post update <id> <file\|->` | New version of a post, same card |
| `notes post part <id> add\|set\|rm ...` | Edit individual parts of a post |
| `notes post list [--session S]` / `notes post show <id>` | List posts, show one with its parts |
| `notes upload <file>` | Store an asset by content hash; prints its id |
| `notes comment <post> TEXT` | Reply on a post |
| `notes sessions` | List agent sessions |

### Events

| Verb | What it does |
|---|---|
| `notes watch [--since SEQ] [--kind note,post,comment]` | Stream events as NDJSON from the event log, resuming after `SEQ` |
| `notes wait --comment [--post ID] [--timeout S]` | Block until the user comments, then print it and exit |

### Batch

`notes batch [--atomic] < ops.ndjson` runs one `{"verb":…,"args":…}` per line
in a single process and connection. `--atomic` runs them in one transaction and
rolls back on the first failure. Output is one result line per op.

### Files and meta

| Verb | What it does |
|---|---|
| `notes export <dir> [<ref>...]` | Write notes as `.md` files with properties as frontmatter, for grep or backup |
| `notes import <dir\|file>` | Import `.md` files (Obsidian vaults included) |
| `notes manifest` | Machine-readable JSON of every verb, its args, output schema and exit codes |
| `notes agent-howto` | Short usage guide to paste into an agent's AGENTS.md |
| `notes doctor` | Socket and database paths, daemon reachable, versions |
| `notes completions zsh\|bash\|fish` | Shell completion script |
| `notes help [verb]` | Short help |

## Non-goals

- No MCP server, no HTTP API, no XPC.
- No in-process plugins; extending means another client on the socket.
- No direct SQLite access from the CLI.

## Open questions

- Folders, or tags only? The sidebar spec is a flat list of titles; this draft assumes tags.
- Trash retention period.
- Whether `search` should rank by BM25 or recency by default.
