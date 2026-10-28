# notes + stage — agent guide (CLAUDE.md is a symlink to this file)

Start with README.md (architecture and layout), then the README in the crate you're changing.
The protocol spec is docs/cli-spec.md; stage's design is docs/stage-plan.md. Background research
(UI target in docs/research/mock/index.html) is in docs/research/. docs/ref/sideshow is a
gitignored reference clone of modem-dev/sideshow. Plan: https://claude.ai/artifact/J51oZ9YXHap6Hc76HeJr5T

## Scope
- A simple, fast note-taking app that also carries all of sideshow's internals and parts (markdown, code, diff, terminal, json, trace, image, mermaid, html), shown in stage (TUI) and Notes.app. No agent status list, no status bar.
- macOS-only, Apple Silicon only.
- Notes are plain `.md` files in folders under one vault (`~/Notes`, `NOTES_VAULT`). Folders only, no tags or properties. Stage sessions are one JSON file each in `~/Desktop/Stage`.
- Rust daemon (`notesd`, launchd socket activation, kqueue, no timers) is the only writer: vault, session folder, and SQLite (WAL) for note versions and history, trash (30 days), settings and the render cache, plus an append-only event log clients resume from by sequence number. Search is the `fff-search` crate embedded in `notesd`.
- Clients (`notes` CLI, `stage`, Notes.app) talk to the daemon over a unix socket, one JSON line per request (not MCP, not XPC). Every verb supports JSON output.
- UI: Swift + AppKit (TextKit 2, NSGlassEffectView, WKWebView for html parts, pele in JavaScriptCore for mermaid). stage is ratatui; its visuals come from `stage-helper` (Swift) over kitty graphics.
- Renders: heavy work (highlighting, parsing, diff hunking) in the daemon, output cached by content hash, only visible parts laid out, only changed parts re-rendered.
- Look: Apple-style glass. Sidebar 200px, small title-only rows 28px, reading pane; match docs/research/mock.

## Performance rules
- No bounce/slide animations on frequent actions.
- Zero idle wakeups: no timers/polling when nothing changes.
- Redraw only on change (notify only when state changed).
- Near-zero allocations per frame; reuse buffers, cache shaped text.
- Apply Brendan Gregg's USE method (utilization, saturation, errors) to every resource.
- Budgets are measured, not guessed: user's Studio Display is 60 Hz, MacBook screen is 120 Hz; build for 120 Hz (8.3 ms frame).

## Workflow
- Cargo workspace: crates/{daemon,posts,proto,cli,stage}; the Swift package is apps/Notes. spikes/ are kept for their measurements and aren't built by default.
- Work on your own branch/worktree (claude/…, codex/…). Commit locally; don't push unless the user asks.
- `cargo build && cargo test` must pass before you finish (plus `swift test` in apps/Notes if you touched Swift). Keep diffs small and in your own crate.
- A wire change touches crates/proto, its fixtures, and the Swift mirror in apps/Notes/Sources/NotesCore together.
- `scripts/install.sh` reinstalls the binaries and restarts notesd; run it to try changes against your real stage.
- Orchestrate agents with the rex skill (`rex skill show`): each agent runs interactively in a Rex session in its own worktree.
