# notes-gpui — agent guide (CLAUDE.md is a symlink to this file)

Read docs/fast-notes-research.md, docs/more-optimizations.md, docs/obsidian-internals.md, docs/mock/index.html (UI target). docs/ref/sideshow is a gitignored reference clone of modem-dev/sideshow. Plan: https://claude.ai/artifact/J51oZ9YXHap6Hc76HeJr5T

## Scope
- A simple, fast note-taking app that also carries all of sideshow's internals and parts (markdown, code, diff, terminal, json, trace, image, mermaid, html). No agent status list, no status bar.
- macOS-only, Apple Silicon only.
- Rust daemon (`notesd`, launchd socket activation, kqueue, no timers) owns all data: SQLite (WAL) + FTS5 trigram + Loro CRDT, sideshow's tables (sessions, posts, comments, assets, trace_steps, settings), and an append-only event log clients resume from by sequence number.
- Rust CLI (`notes`) and the app both talk to the daemon over a unix socket, one JSON line per request (not MCP, not XPC). Every verb supports JSON output.
- UI: Swift + AppKit (TextKit 2, NSGlassEffectView, WKWebView for html parts, pele in JavaScriptCore for mermaid). GPUI only for surfaces Core Animation can't carry (big canvas, graph).
- Renders: heavy work (highlighting, parsing, diff hunking) in the daemon, output cached by content hash, only visible parts laid out, only changed parts re-rendered.
- Look: Apple-style glass. Sidebar 200px, small title-only rows 28px, reading pane; match docs/mock.

## Performance rules
- No bounce/slide animations on frequent actions.
- Zero idle wakeups: no timers/polling when nothing changes.
- Redraw only on change (notify only when state changed).
- Near-zero allocations per frame; reuse buffers, cache shaped text.
- Apply Brendan Gregg's USE method (utilization, saturation, errors) to every resource.
- Budgets are measured, not guessed: user's Studio Display is 60 Hz, MacBook screen is 120 Hz; build for 120 Hz (8.3 ms frame).

## Workflow
- Cargo workspace: crates/core (store + CLI lib, bin `notes`); crates/ui is the GPUI prototype from milestone 1 and will be replaced by the Swift app.
- Work on your own branch/worktree (claude/ui, codex/core). Commit locally; NEVER push or create remotes.
- `cargo build` must pass before you finish. Keep diffs small and in your own crate.
- Orchestrate agents with the rex skill (`rex skill show`): each agent runs interactively in a Rex session in its own worktree.
