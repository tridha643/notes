---
name: stage-feedback
description: Model-check stage's feedback delivery (the per-session agent_seq cursor behind `feedback:` lines on `notes post`, `notes wait`, and `notes comments`) with TLA+/TLC, so no order of user comments, agent posts, concurrent waits, deletes, lost replies or daemon restarts drops a user comment or hands it to the agent twice. Use when changing collect_feedback, post_new, write_version, comment_new, comment_delete, post_delete or next_user_comment in crates/posts/src/store.rs, Store::save or Stage::load in crates/posts/src/json_store.rs, StageIndex in crates/posts/src/stage_index.rs, CommentSignal in crates/posts/src/lib.rs, or Daemon::unit and the Wait arm of call() in crates/daemon.
---

# stage-feedback: every user comment reaches the agent exactly once

## The promise

The user comments on cards in stage (`comment_new`). The agent gets those comments on the
next `notes post` or `notes post update` (as `feedback:` lines, from `collect_feedback`) or
by blocking in `notes wait` (`next_user_comment`). One number per session decides what the
agent has seen: `agent_seq` in the session file. The promise, as two rules:

| Rule | In plain words | Harm when broken |
|---|---|---|
| `Once` | No user comment reaches the agent twice, counting feedback lines and wait answers together. | The agent acts on the same instruction twice (a second edit, a second apology). |
| `NoLoss` | The cursor never passes a user comment the agent has not received: everything at or below `agent_seq` was delivered, everything above is still deliverable. | The user typed a correction and the agent never sees it. |

`notes comments` (`comment_list`) is a plain read with a client-chosen `--after` and never
moves the cursor, so it can neither lose nor consume a comment; it is outside the two rules
by design.

Tests check the orders someone wrote down; this checks every order within the bounds.

## Run

```sh
.claude/skills/stage-feedback/scripts/check.sh
```

Prints `PASS` or the violated rule and a trace. It downloads TLA+ tools 1.8.0 once
(sha256-pinned) to `~/.cache/notes-gpui/` and uses Homebrew's `openjdk`
(`brew install openjdk` if `java` is missing). Under a second. From Claude Code, run it with
the sandbox disabled (the first run needs the network). Pass a spec path to check a mutated
copy: `scripts/check.sh /path/to/StageFeedback.tla`.

The default config models the code as it is, and the code as it is breaks `NoLoss` in four
ways (see Open findings), so **the default run fails today**. Until those are fixed, the
regression gate for a change to the cursor logic is the cursor config, which leaves out the
four behaviours the findings need and passes:

```sh
.claude/skills/stage-feedback/scripts/check.sh .claude/skills/stage-feedback/model/StageFeedback.tla .claude/skills/stage-feedback/model/StageFeedback-cursor.cfg
python3 -I .claude/skills/stage-feedback/scripts/findings.py
```

`findings.py` reproduces each finding on its own and prints `open` or `fixed` per row. Before
a release, run the deep bounds too (`StageFeedback-deep.cfg`, about 20 s).

Read every result, pass or fail, with the pstack `/statechart` skill: on a violation it gives
the shortest path to the bad state in plain words; on a pass it draws how the session moves
between states.

## Scope and bounds

- One session, two posts (`P1`, `P2`) plus session-level comments, two authors (`user` and
  the agent; agent comments move the cursor in `collect_feedback` but are never feedback).
- Events: a user or agent comment, an agent post (post_new, post_update, post_part: all run
  `collect_feedback`), a `notes wait` starting, being answered or timing out, a comment or
  post deleted, and notesd restarting. Every store unit runs under `Daemon::unit`'s lock, so
  a unit is one `Begin*` step (read and compute) and one `Commit` step (file written, lock
  released, reply sent); nothing interleaves between them unless the lock goes.
- The four switchable behaviours (`StageFeedback.cfg` turns all on, `-cursor.cfg` all off):
  `WaitPost` (`notes wait --post`), `Abandon` (the wait's client dies while blocked),
  `ReplyLost` (the reply fails to land after the unit committed), `Deletes` (comment_delete,
  post_delete).
- Bounds (`StageFeedback-cursor.cfg`): 4 comments ever created, 2 wait clients: 77,624
  distinct states at the sync commit. Deep (`-deep.cfg`): 6 comments, 3 clients, 9,329,392.
- Left out: other sessions (the cursor is per session; other sessions only raise the global
  seq, which cannot cause a collision), trashing and restoring a session, a session file
  edited outside notesd (`Store::refresh` reloads whatever `agent_seq` the file says), a unit
  that fails after `save` (the journal puts the old file back and the reply is an error, so
  nothing moved), an explicit `after_seq` to `next_user_comment` (the daemon always passes
  `None`), whether a `--post` wait returns only that post's comments, and the agent reading
  the `feedback:` lines the CLI printed.

## What each part stands for (coverage)

Sync commit: `38767a4` (the model describes the code at this commit). When any file below
changes after it, recheck that the model still describes the code; move the sync commit
forward only after you have.

| Model | Code |
|---|---|
| `comments`, `cursor` | `SessionFile.comments`, `SessionFile.agent_seq` (crates/posts/src/json_store.rs); `Store::save` is the only path that updates memory and disk, so there is one cursor value |
| `maxSeq`, `NewComment` | `StageIndex::next_comment_seq`, `add_session` (crates/posts/src/stage_index.rs); `comment_new` (crates/posts/src/store.rs) |
| `BeginPost` | `collect_feedback`, called by `post_new` and `write_version` (post_update, post_part) in store.rs |
| `StartWait`, `BeginWait`, `WaitTimeout` | `Cmd::Wait` in crates/cli/src/main.rs (`--post`, `--timeout`, default 120 s); the `Verb::Wait` arm of `call` in crates/daemon/src/main.rs; `CommentSignal::wait_for` (crates/posts/src/lib.rs); `next_user_comment` in store.rs |
| `Free`, `Commit` | `Daemon::unit` in crates/daemon/src/store.rs (the `Mutex<Store>`, COMMIT, `store.commit()`, `comments.notify()`), then `serve` in crates/daemon/src/main.rs writing the reply; `Tx::sideshow`'s `CommentNew` arm setting `commented` |
| `Leave` | the `Verb::Wait` arm of `call` has no reader on the socket (compare `watch`, which spawns `notesd-watch-eof`) |
| `DeleteComment` | `comment_delete`, and `post_delete`'s `comments.retain` in store.rs |
| `Restart` | `Stage::load` and `Stage::insert` (json_store.rs) rebuilding `max_comment_seq` from the files; `Store::rollback` for a unit cut short |
| `delivered` | the `feedback` field of `PostWriteResult` and the `wait` result, as the CLI prints them (`feedback:` lines, `comment_line`) |

## When the check fails

1. Read the trace with `/statechart`. Retell it as the steps the user and the agent take
   (comments, posts, waits, a restart) and what the agent received.
2. Decide: real bug or modeling error. Open the code the failing step stands for (table
   above) and walk the same steps through it. If the code does what the trace says, it's a
   real bug: report it with that sequence; fix the code only if asked. If the code doesn't,
   the model is wrong there: fix the model to match the code, never by weakening a rule.
3. Every fix to code that the model covers needs the matching edit in the model, then a
   passing run of the cursor config and `mutations.py`. When a fix closes an open finding,
   `findings.py` prints `fixed` for it: delete its row there and below, and set its constant
   to `TRUE` in `StageFeedback-cursor.cfg` so the behaviour stays checked.

## Open findings

All four break `NoLoss` at the sync commit. `findings.py` prints each trace.

| Finding | Steps (the code's words) |
|---|---|
| `notes wait --post P` skips the user's comments on other posts. | User comments on P1 (seq 1), then on P2 (seq 2). Agent runs `notes wait --post P2`. `next_user_comment` returns seq 2 and sets `agent_seq = 2`. Seq 1 is now below the cursor: no later post or wait returns it. |
| A `notes wait` whose client died consumes the next comment. | Agent runs `notes wait` (default 120 s) and the harness kills it. Nothing in `call`'s Wait arm reads the socket, so the daemon thread stays in `wait_for`. The user comments. The thread's `next_user_comment` sets `agent_seq` and the reply goes to a closed socket. |
| A reply lost after the cursor was written takes its comments with it. | User comments. Agent posts. `post_new` runs `collect_feedback`, `save` writes `agent_seq`, the unit commits. notesd dies, or the socket write in `serve` fails, before the reply lands. No ack exists, so the comment is gone. |
| Deleting a delivered comment and restarting reuses its seq below the cursor. | User comments (seq 1). Agent posts, gets it, `agent_seq = 1`. The post is deleted (`post_delete` drops its comments). notesd restarts: `Stage::load` sets `max_comment_seq` from the files, now 0. The next user comment gets seq 1, below the cursor: never delivered. |

The third needs a protocol change (an acknowledgement from the client) and may be accepted as
is; the other three are local to the functions named.

## Proof that it can fail

`scripts/mutations.py` applies each mutation below to a copy of the model, the way the
regression would look in the code, checks it against the cursor config, and expects TLC to
report the rule shown (all caught at the sync commit, about 4 s). Run it after changing the
model; add a row for each new bug:

| Regression in code | Rule that fails |
|---|---|
| `collect_feedback` takes comments at or past `agent_seq` (`>=`) | `Once` |
| `post_new` replies with the `result` built before `collect_feedback` (empty `feedback`) | `NoLoss` |
| `next_user_comment` returns the comment without moving `agent_seq` | `Once` |
| `next_user_comment` moves `agent_seq` to the newest user comment, not the one it returns | `NoLoss` |
| `next_user_comment` takes comments at or past `agent_seq` (`>=`) | `Once` |
| `StageIndex::add_session` stops tracking `max_comment_seq` (seqs restart at 1 after a restart) | `NoLoss` |
| `Daemon::unit` stops taking the store lock (two threads read one cursor) | `Once` |

## Keeping it in step

`/maintain-formal-verification-skill` brings the model back in line after changes that
didn't recheck it.
