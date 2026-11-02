# Agent guide: `notes guide`, `notes agent-howto`, and the post checks

Agents learn stage from the installed CLI, never from a copy: `notes agent-howto` says how to
post and read comments, and `notes guide` is sideshow's design guide adapted to stage
(crates/cli/src/guide.md) plus a "Right now" section with the live card size that stage last
reported. `notes post` holds every post to that guide (crates/cli/src/lint.rs): it refuses a
post that can't fit and warns about the rest.

## Sub-features

- `notes agent-howto` and `notes setup` print without a daemon.
- `notes guide` ends with "Right now": stage's reported space, or the default when stage
  hasn't run.
- A post that can't fit (a markdown table wider than the card, an SVG taller than the html
  viewport) is refused with an `error:` line and exit 2, and nothing reaches notesd.
- A post that only reads worse (Title Case title, emoji) publishes with `warning:` lines.

## How to get to it (user POV)

An agent runs these as the stage skill (skills/stage/SKILL.md) tells it to, before its first post.

## Driving it with the CLI

No PTY needed. Use the binary you built (`target/release/notes`), not `~/.local/bin/notes`.

```sh
n=target/release/notes
$n agent-howto --sock /nonexistent/notesd.sock | head -3        # works with no daemon
$n guide | grep -E '^## '                                      # section list
$n guide | sed -n '/^## Right now/,$p'                          # live card size
printf '<svg width="100%%" viewBox="0 0 100 5000"></svg>' > /tmp/tall.html
$n post html /tmp/tall.html --title "Tall svg" --session-title "stage verify guide"; echo "exit $?"
```

Proof: the section list matches guide.md's H2s (Posts and parts, Fit the card, HTML contract,
Built-in kit, Kits, Theming, Style, Feedback, Right now); the tall SVG prints an `error:` line
and `exit 2`; `notes sessions` shows no new "stage verify guide" session.

## Gotchas

- With stage never run on this machine, "Right now" reports the default space and the post
  checks only warn, so the refusal step needs a stage that has reported its size.
- A post that passes the checks is real: it creates a session in the user's stage. Delete it
  with `notes session delete <id> --purge`.
