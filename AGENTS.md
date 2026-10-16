# notes-gpui — agent guide (CLAUDE.md is a symlink to this file)

Read docs/fast-notes-research.md, docs/more-optimizations.md, docs/mock/index.html (UI target). docs/ref/sideshow is a gitignored reference clone of modem-dev/sideshow.

## Scope
- A SIMPLE note-taking app ONLY. NO diff view, terminal, JSON views, agent status list, status bar.
- macOS-only, Metal / Apple Silicon only. Rust + GPUI (Zed's UI framework). rustc 1.88+.
- Used by humans AND agents. Agents use a CLI (`notes new/list/show/append`) over a unix socket (not MCP).
- Storage internals follow sideshow: SQLite (WAL) + Loro CRDT for note bodies.
- Look: Apple-style glass. Sidebar 200px, small title-only rows 28px, reading pane; match docs/mock.

## Performance rules
- No bounce/slide animations on frequent actions.
- Zero idle wakeups: no timers/polling when nothing changes.
- Redraw only on change (notify only when state changed).
- Near-zero allocations per frame; reuse buffers, cache shaped text.
- Apply Brendan Gregg's USE method (utilization, saturation, errors) to every resource.
- Budgets are measured, not guessed: user's Studio Display is 60 Hz, MacBook screen is 120 Hz; build for 120 Hz (8.3 ms frame).

## Workflow
- Crates in a cargo workspace: crates/core (store+CLI lib), crates/ui (GPUI app), bin `notes`.
- Work on your own branch/worktree (claude/ui, codex/core). Commit locally; NEVER push or create remotes.
- `cargo build` must pass before you finish. Keep diffs small and in your own crate.
- Local tool `rex` skill exists (~/.claude/skills/rex) but the rex CLI is not installed here.
