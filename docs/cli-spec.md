# `notes` CLI spec (P0 draft, 2026-10-18)

Notes are plain `.md` files in folders under one vault directory; folders are
real directories and there are no tags. The CLI is the safe, fast way in for
agents (atomic writes, version checks, history), but `cat` and `grep` on the
vault still work. Search is fff (`fff-search` crate) embedded in `notesd`.

References: Obsidian's CLI (~100 verbs, `format=json`, one JSON line over a
socket; see `obsidian-internals.md`), Claude Code's Read/Edit/Grep semantics,
sideshow's CLI verbs (`docs/ref/sideshow/bin/sideshow.js`).

## Shape

- Binary `notes` (Rust). It never touches the vault or SQLite itself; every
  call is one request to `notesd` over a unix socket and one (or a stream of)
  responses.
- Vault: `~/Notes` by default, override with `NOTES_VAULT`. A note is
  `<folder>/<title>.md`. `notesd` writes atomically (temp file + rename) and
  watches the vault, so edits made outside the CLI are picked up and versioned.
- SQLite (WAL) holds what isn't a note: versions and history, trash, the event
  log, and sideshow's tables.
- Socket: `~/Library/Application Support/Notes/notesd.sock`, override with
  `NOTES_SOCK`. launchd owns the socket and starts `notesd` on first connect.
- Budget (target, unmeasured for the real CLI): ≤10 ms per warm call. The
  measured process floor on the M5 is ~6.5 ms; a socket round trip is ~8 µs.
- Milestone 1 verbs keep working: `new`, `list`, `append`, `show` (alias of `read`).

## Wire protocol

One JSON object per line, UTF-8, both directions.

```
→ {"v":1,"id":7,"verb":"edit","args":{"note":"work/plan","edits":[{"old":"foo","new":"bar"}]},"cwd":"/path"}
← {"id":7,"ok":true,"result":{"path":"work/plan.md","version":43}}
← {"id":7,"ok":false,"error":{"code":"ambiguous","message":"\"foo\" matches 3 times (lines 4, 9, 30)","hint":"add surrounding text or pass --all"}}
```

Streaming verbs (`watch`, `wait`) send `{"id":7,"event":{...}}` lines until
closed. The CLI turns `result` into text, or prints it as-is with `--json`.
Other tools may speak the protocol directly (e.g. `socat`). The Rust types live in `crates/proto` (`notes-proto`); they are the source of truth for field names.

## Global flags

| Flag | Meaning |
|---|---|
| `--json` | Print the `result` object; lists print one JSON object per line (NDJSON) |
| `--if-version N` | On writes: fail with exit 5 unless the note is at version N |
| `--sock PATH` | Socket path (same as `NOTES_SOCK`) |
| `-q`, `--quiet` | Print nothing on success; rely on the exit code |

## Note references

A `<ref>` is a vault-relative path with or without `.md` (`work/plan`), a bare
title when only one note has it, or `@last` (most recently updated). A bare
title that matches notes in several folders fails with exit 4 and lists their
paths.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Other error (message on stderr) |
| 2 | Usage error |
| 3 | Not found |
| 4 | Ambiguous (bare title or edit match) |
| 5 | Version conflict (`--if-version` mismatch); the error's `details` carry `{"current_version": N}` |
| 6 | Daemon unreachable |

Errors go to stderr as one line plus a `hint:` line saying how to fix it.
With `--json`, the error object is printed to stdout as well.

## Verbs

### Read

| Verb | What it does |
|---|---|
| `notes list [FOLDER] [-r] [--sort updated\|created\|title] [--limit N]` | List notes and subfolders in a folder (vault root by default); `-r` recurses |
| `notes find <query> [--limit N]` | fff fuzzy path search: typo-tolerant, ranked by how often and how recently you open notes |
| `notes read <ref> [--lines A:B] [-n]` | Print the body; `--lines` slices, `-n` prefixes line numbers like `cat -n` |
| `notes search <query> [FOLDER] [-C N] [--case] [--limit N]` | fff content grep, results `path:line: text`. Smart case, regex detected automatically, fuzzy fallback when nothing matches exactly, ranked by fff's frecency; `-C` adds context lines |
| `notes links <ref>` / `notes backlinks <ref>` | Outgoing `[[links]]` and notes linking here |

`list --json` prints one line per entry, folders first: `{"folder":"work"}` or a note summary `{path, title, version, updated_at}`.

`read --json` returns `{path, title, version, body, line_count, created_at, updated_at}`.

### Write

| Verb | What it does |
|---|---|
| `notes new <path> [TEXT\|-]` | Create a note, making folders as needed; prints its path |
| `notes edit <ref> --old S --new S [--all]` | Exact find-and-replace. `--old` must match exactly once unless `--all`; otherwise exit 4 with the match count and line numbers |
| `notes edit <ref> --from FILE\|-` | Several edits from JSON `[{"old":…,"new":…}]`, applied all-or-nothing |
| `notes append <ref> TEXT\|- [--under HEADING]` | Add to the end, or to the end of a section |
| `notes prepend <ref> TEXT\|-` | Add at the top |
| `notes write <ref> -` | Replace the whole body from stdin; requires `--if-version` or `--force` |
| `notes move <ref> <path>` | Move or rename a note; updates `[[links]]` that point to it |
| `notes mkdir <folder>` / `notes rmdir <folder>` | Create a folder, or remove an empty one |
| `notes delete <ref> [--purge]` | Move to trash (kept 30 days, purged when the daemon is idle), or delete for good |
| `notes restore <ref>` | Bring a note back from trash |

`edit`, `append` and `prepend` don't need `--if-version`: the daemon serializes
writes per note and re-checks `--old` at apply time, so two agents editing
different parts of a note both land. Every write returns the new `version`.

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
rolls back on the first failure. Output is one line per op, in order:
`{"ok":true,"result":…}` or `{"ok":false,"error":…}`; an atomic batch stops
at its first error.

### Files and meta

| Verb | What it does |
|---|---|
| `notes import <dir\|file> [FOLDER]` | Copy `.md` files into the vault, keeping their folder structure (Obsidian vaults included) |
| `notes manifest` | Machine-readable JSON of every verb, its args, output schema and exit codes |
| `notes agent-howto` | Short usage guide to paste into an agent's AGENTS.md |
| `notes doctor` | Socket and database paths, daemon reachable, versions |
| `notes completions zsh\|bash\|fish` | Shell completion script |
| `notes help [verb]` | Short help |

## Non-goals

- No MCP server, no HTTP API, no XPC.
- No in-process plugins; extending means another client on the socket.
- No direct SQLite access from the CLI.
- No tags or properties.

## Decided (2026-10-18)

- Folders only, no tags.
- Trash keeps notes 30 days.
- Search and find use fff's ranking (fuzzy, typo-tolerant, frecency).

## Open questions

- Loro: with plain files as the source of truth, P0 history is per-write snapshots in SQLite. Loro stays for shared notes later, unless we want it from day one.
