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

Heavy diagrams: `scripts/heavy.py` makes a "stage verify heavy" session (real charts from
pstack's /statechart corpus, an 80-state statechart, a 160-node architecture flowchart, a
30-class diagram). Drive any feature above against it too; pele lays these out far wider
than the card (DieHard: 2280 CSS px), so check text size, not only `one_to_one`.
