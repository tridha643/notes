# stage — agent guide (CLAUDE.md is a symlink to this file)

Start with README.md (architecture and layout), then the README in the crate you're changing.
The protocol spec is docs/cli-spec.md; stage's design is docs/stage-plan.md. docs/ref/sideshow
is a gitignored reference clone of modem-dev/sideshow; `notes guide` (crates/cli/src/guide.md)
tracks its guide/DESIGN_GUIDE.md.

## Scope
- stage: a terminal pane that shows coding agents' posts (sideshow's parts: markdown, code,
  diff, terminal, json, trace, image, mermaid, html) and sends the user's comments back. No
  agent status list, no status bar. There is no note-taking app; don't add one back.
- macOS-only, Apple Silicon only.
- Stage sessions are one JSON file each in `~/Desktop/Stage` (`NOTES_STAGE_DIR`).
- Rust daemon (`notesd`, launchd socket activation, kqueue, no timers) is the only writer: the
  session folder and SQLite (WAL) for session trash (30 days), settings and the render cache,
  plus an append-only event log clients resume from by sequence number.
- Clients (`notes` CLI, `stage`) talk to the daemon over a unix socket, one JSON line per
  request (not MCP, not XPC). Every verb supports JSON output.
- stage is ratatui; its visuals come from `stage-helper` (Swift: pele in JavaScriptCore for
  mermaid, WKWebView for html parts) over kitty graphics.
- Renders: heavy work (highlighting, parsing, diff hunking) in the daemon, output cached by
  content hash, only visible parts laid out, only changed parts re-rendered.
- `notes post` refuses what stage can't show (a card overflow, mermaid pele can't parse)
  before sending, with `error:` lines that name the fix. Add a check there, not a guide line,
  when agents keep making the same mistake.

## Performance rules
- No bounce/slide animations on frequent actions.
- Zero idle wakeups: no timers/polling when nothing changes.
- Redraw only on change (notify only when state changed).
- Near-zero allocations per frame; reuse buffers, cache shaped text.
- Apply Brendan Gregg's USE method (utilization, saturation, errors) to every resource.
- Budgets are measured, not guessed: user's Studio Display is 60 Hz, MacBook screen is 120 Hz;
  build for 120 Hz (8.3 ms frame).

## Workflow
- Cargo workspace: crates/{daemon,posts,proto,cli,stage}; the Swift package is apps/Notes
  (stage-helper and the NotesCore pieces it uses).
- Work on your own branch/worktree (claude/…, codex/…). Commit locally; don't push unless the
  user asks.
- `cargo build && cargo test` must pass before you finish (plus `swift test` in apps/Notes if
  you touched Swift). Keep diffs small and in your own crate.
- A helper wire change touches crates/proto/src/helper.rs, its fixtures, and
  apps/Notes/Sources/NotesCore/HelperProtocol.swift together.
- `scripts/install.sh` builds this checkout and installs it with the release installer
  (`install.sh`), restarting notesd; run it to try changes against your real stage.
  `scripts/release.sh vX.Y.Z` publishes a release the curl installer picks up.
- Drive and prove stage changes with `.claude/skills/verify`. Model-check diagram pixels with
  `.claude/skills/stage-pixels`, and comment delivery to agents (`agent_seq`) with
  `.claude/skills/stage-feedback`.
- Orchestrate agents with the rex skill (`rex skill show`): each agent runs interactively in a
  Rex session in its own worktree.
