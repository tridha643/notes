# notesd

The daemon. It owns the stage session folder (`~/Desktop/Stage`) and the SQLite database, and
serves the line protocol from [`notes-proto`](../proto) on a unix socket. Nothing else writes
any of that data.

## Threads and wakeups

- **One thread per connection.** Each reads request lines, runs the verb and writes one
  response line, except `watch`, which streams events.
- **One kqueue watcher thread** ([`watcher.rs`](src/watcher.rs)) watches `~/Desktop/Stage`, so
  session files added, removed or edited by hand reload.
- **No timers.** An idle daemon with a still session folder never wakes up.

## Writes are transactions

Every verb handler in [`ops.rs`](src/ops.rs) runs inside a `Tx` ([`store.rs`](src/store.rs)):
one SQLite transaction plus the session folder's file journal. If the verb fails (or one op in
`batch --atomic` fails), both the files and the database roll back.

Each change appends to an event log with a sequence number, so `watch --since N` resumes
without gaps after a reconnect.

## Startup

1. Under launchd, take the `Listeners` socket from `launch_activate_socket`; otherwise bind
   `NOTES_SOCK` itself (refusing if another notesd already answers there).
2. Open SQLite (`NOTES_DB`, default `~/Library/Application Support/Notes/notes.db`) and the
   session folder, moving pre-JSON sessions out of SQLite once
   ([`migrate_json.rs`](../posts/src/migrate_json.rs)).
3. Start the watcher, purge session trash older than 30 days, then accept connections.

The LaunchAgent plist is written by [`scripts/install.sh`](../../scripts/install.sh);
[`launchd/dev.notes.notesd.plist`](launchd/dev.notes.notesd.plist) is a reference copy.
Logs go to `~/Library/Logs/notesd.log`.

## Environment

| Variable | Default |
|---|---|
| `NOTES_SOCK` | `~/Library/Application Support/Notes/notesd.sock` |
| `NOTES_DB` | `~/Library/Application Support/Notes/notes.db` |
| `NOTES_STAGE_DIR` | `~/Desktop/Stage`; set an absolute path for a different session store |
| `NOTES_RENDER_CACHE` | `off` disables the render cache |

## Test

```sh
cargo test -p notesd                         # spins up a real notesd on temp dirs
```
