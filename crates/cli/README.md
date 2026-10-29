# notes (CLI)

A thin client for `notesd`. Every call sends one request line over the unix socket and reads
one response line back, or a stream of event lines for `watch`. It never touches the vault or
SQLite directly, so it's safe to run alongside the app, stage and other agents.

## Verbs

`notes --help` lists them all; `notes manifest` prints every verb with its args, output
schema and exit codes as JSON. The groups are:

- **Notes:** `list find search read new edit append prepend write move mkdir rmdir delete
  restore history diff revert`. A ref is `folder/title`, a unique bare title, or `@last`.
  Writes take `--if-version N` and fail with exit 5 on a conflict.
- **Stage:** `post` (publish, `post update`, list, show), `upload`, `comment`, `comments`,
  `wait`, `sessions`, `session`, `trace`, `kits`.
- **Plumbing:** `watch` (NDJSON events, `--since` to resume), `batch` (NDJSON ops over one
  connection, `--atomic`), `doctor`, `completions`.
- **For agents:** `agent-howto`, `guide`, `setup`.

`--json` prints raw results (lists as one object per line) and `-q` prints nothing, relying on
the exit code.

## How agents get their own session

[`owner.rs`](src/owner.rs) walks up from `notes` to the first ancestor that isn't a shell or
wrapper (the agent process) and identifies it by pid and start time. notesd keys the session
on that, so two conversations in the same folder never share a thread. `NOTES_OWNER` and
`NOTES_SESSION` override it, and `NOTES_AGENT` sets the author name in history.

## Feedback and lint

After `notes post`, comments the user left since the last call print on stderr as
`feedback:` lines, so the agent sees them without polling. [`lint.rs`](src/lint.rs) checks a post
against [`guide.md`](src/guide.md) and the space stage last reported for its cards (the
`stage.space` setting). A post that can't fit the card (a table wider than it, an html page wider
or taller than its viewport) is refused with `error:` lines and exit 2; style slips only print
`warning:` lines. Until stage has reported a space, overflow only warns too.

`agent-howto` and `guide` are compiled into the binary, so the instructions an agent reads
always match the CLI it's running.

## Test

```sh
cargo test -p notes-cli     # uses tests/fake_daemon.rs, no real notesd needed
```
