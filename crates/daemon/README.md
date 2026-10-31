# notesd

The daemon. It owns the vault (`~/Notes`), the stage session folder (`~/Desktop/Stage`), the
SQLite database and fff search, and serves the line protocol from [`notes-proto`](../proto) on
a unix socket. Nothing else writes any of that data.

## Threads and wakeups

- **One thread per connection.** Each reads request lines, runs the verb and writes one
  response line, except `watch`, which streams events.
- **One kqueue watcher thread** ([`watcher.rs`](src/watcher.rs)) registers vnode events on every
  folder and note and blocks in `kevent` with a NULL timeout. When something changes it diffs
  a metadata snapshot and calls `Daemon::sync_paths`, so edits made outside `notes` (an
  editor, `git pull`) still get versioned (author `external`) and indexed. A second kqueue
  thread watches `~/Desktop/Stage` the same way, so session files edited by hand reload.
- **No timers.** An idle daemon with a still vault never wakes up. fff runs with its own
  watcher off; notesd tells it about every change ([`search.rs`](src/search.rs)).

## Writes are transactions

Every verb handler in [`ops.rs`](src/ops.rs) runs inside a `Tx` ([`store.rs`](src/store.rs)):
one SQLite transaction plus a journal of file changes. If the verb fails (or one op in
`batch --atomic` fails), both the files and the database roll back. Note writes go through
temp file → fsync → rename, and each one records a version with its author (`NOTES_AGENT`).
The pure text operations behind `edit`, `append` and `prepend` live in
[`text.rs`](src/text.rs) and do no I/O.

Each change appends to an event log with a sequence number, so `watch --since N` resumes
without gaps after a reconnect.

## Startup

1. Under launchd, take the `Listeners` socket from `launch_activate_socket`; otherwise bind
   `NOTES_SOCK` itself (refusing if another notesd already answers there).
2. Open SQLite (`NOTES_DB`, default `~/Library/Application Support/Notes/notes.db`) and the
   session folder, moving pre-JSON sessions out of SQLite once
   ([`migrate_json.rs`](../posts/src/migrate_json.rs)).
3. Start both watchers, then reconcile the vault against the database (watching first, so
   nothing that changes in between goes unseen), then accept connections.

The LaunchAgent plist is written by [`scripts/install.sh`](../../scripts/install.sh);
[`launchd/dev.notes.notesd.plist`](launchd/dev.notes.notesd.plist) is a reference copy.
Logs go to `~/Library/Logs/notesd.log`.

## Environment

| Variable | Default |
|---|---|
| `NOTES_SOCK` | `~/Library/Application Support/Notes/notesd.sock` |
| `NOTES_VAULT` | `~/Notes` |
| `NOTES_DB` | `~/Library/Application Support/Notes/notes.db` |
| `NOTES_STAGE_DIR` | `~/Desktop/Stage`; set an absolute path for a different session store |
| `NOTES_RENDER_CACHE` | `off` disables the render cache |

## Test and bench

```sh
cargo test -p notesd                         # spins up a real notesd on temp dirs
cargo run --release -p notesd --example bench
```

Numbers and method: [docs/bench/daemon-bench.md](../../docs/bench/daemon-bench.md). Why fff
and how it was configured: [docs/research/fff-check.md](../../docs/research/fff-check.md).
