# Notes.app

Native macOS client for `notesd`, in Swift + AppKit (TextKit 2, `NSGlassEffectView`).
It never touches the vault: every read and write is one JSON line over the daemon's
unix socket (`$NOTES_SOCK`, else `~/Library/Application Support/Notes/notesd.sock`).
Field names follow `crates/proto/src/lib.rs`.

## Layout

- `Sources/NotesCore`: wire types (`Protocol.swift`), the blocking socket client and
  `watch` stream (`DaemonClient.swift`), per-line markdown spans (`Markdown.swift`). No AppKit.
- `Sources/NotesUI`: window and menus (`App.swift`), sidebar outline (`Sidebar.swift`),
  editor with idle save, conflict banner and incremental styling (`Editor.swift`), and the
  serial request queue (`Daemon.swift`).
- `Sources/Notes/main.swift`: calls `runApp()`.
- `scripts/fake_notesd.py`: an in-memory stand-in daemon for running the app by hand.

## Build, test, run

```sh
swift build && swift test            # in apps/Notes
../../scripts/build-app.sh           # release build → build/Notes.app, ad-hoc signed

# Without the Rust daemon:
python3 scripts/fake_notesd.py /tmp/notes.sock &
open --env NOTES_SOCK=/tmp/notes.sock ../../build/Notes.app
kill -USR1 %1                        # simulate another client editing welcome.md
```

## Behaviour

- Launch: start `watch`, `list` recursively, open `@last`. If the socket is unreachable the
  window shows how to start notesd; it retries on Retry or when the window becomes key.
- Typing saves 400 ms after the last keystroke with `write` + `if_version`. A
  `version_conflict` keeps your text and shows a banner: Keep Mine (writes over the current
  version) or Use Theirs. Switching notes mid-conflict keeps yours; theirs stays in history.
- ⌘N asks for a name in the sidebar and calls `new` in the selected folder. ⌘⌫ deletes
  the selected note (to the daemon's trash), only while the sidebar has focus.
- Events refresh the sidebar and reload the open note when it has no unsaved changes.
- No timers or polling: the save timer exists only while there are unsaved changes, and
  the watch thread sleeps in `read()`.
