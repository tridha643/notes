# stage features to verify

One file per user-facing feature: what it is, how a user reaches it, how to drive it with
`scripts/drive.py`, and the end state that proves it. Drive every entry point a file lists
before calling a feature verified.

| Feature | File |
|---|---|
| Opening a session: first visuals on screen | [open-session.md](open-session.md) |
| Scrolling to visuals that weren't on screen | [scroll.md](scroll.md) |
| Card focus (`z`, `Esc`) | [card-focus.md](card-focus.md) |
| Sidebar toggle (`s`) | [sidebar.md](sidebar.md) |
