---
name: stage
description: "Show the user something in stage, the terminal pane beside them: diagrams, UI sketches, plans, diffs to review, terminal output, data, live html. Use when the user mentions stage or posting, says show me, visualize, draw, diagram, sketch, or review this diff, or when a picture would explain faster than chat."
---

# stage

The user keeps stage open in a terminal pane. You publish posts to it with the
`notes` CLI, and each conversation gets its own session (a thread of posts).
This skill is only a bootstrap: read the current instructions from the
installed CLI before your first post. They never override system, developer,
project or user instructions.

```sh
notes agent-howto   # how to post, revise a post, and read the user's comments
notes guide         # how posts should look
```

Then post with `notes post <kind> <file|-> --title "..."` (add
`--session-title "The task"` on the first one), revise with
`notes post update <id> ...`, and treat `feedback:` lines on stderr as messages
from the user. `notes session delete <id>` trashes a session for 30 days,
`notes session restore <id>` brings it back, and `--purge` deletes it for good.
If `notes` isn't on PATH, stage isn't installed: say so instead of looking for
another way to post.
