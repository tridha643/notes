# Notes.app

Native macOS client for `notesd`, in Swift + AppKit (TextKit 2, `NSGlassEffectView`).
It never touches the vault: every read and write is one JSON line over the daemon's
unix socket (`$NOTES_SOCK`, else `~/Library/Application Support/Notes/notesd.sock`).
Field names follow `crates/proto/src/lib.rs`.

## Layout

- `Sources/NotesCore`: wire types (`Protocol.swift`, sideshow's in `Posts.swift`), the
  blocking socket client and `watch` stream (`DaemonClient.swift`), per-line markdown spans
  (`Markdown.swift`), ordered JSON and the lazy json tree (`JSON.swift`), trace parsing and
  timeline folding (`Trace.swift`), mermaid via pele in one JavaScriptCore context
  (`Pele.swift`, vendored `Resources/pele.global.js` 1.6.0, MIT). No AppKit.
- `Sources/NotesUI`: window and menus (`App.swift`), sidebar outline with the Agents group
  (`Sidebar.swift`), editor with idle save, conflict banner and incremental styling
  (`Editor.swift`), the serial request queue (`Daemon.swift`), and the agent session view:
  timeline table and composer (`PostsView.swift`), cells (`PostCells.swift`), the
  background loader (`PostModel.swift`), post-render spans → text (`CodeText.swift`),
  html parts in pooled WKWebViews (`WebParts.swift`), colors (`Theme.swift`).
- `Sources/Notes/main.swift`: calls `runApp()`.
- `scripts/fake_notesd.py`: an in-memory stand-in daemon for running the app by hand, with
  every sideshow verb and a seeded session holding a post of every part kind.

## Build, test, run

```sh
swift build && swift test            # in apps/Notes
../../scripts/build-app.sh           # release build → build/Notes.app, ad-hoc signed

# Without the Rust daemon:
python3 scripts/fake_notesd.py /tmp/notes.sock &
open --env NOTES_SOCK=/tmp/notes.sock ../../build/Notes.app
kill -USR1 %1                        # simulate another client editing welcome.md
kill -USR2 %1                        # the agent updates a post, adds one, comments

# Dev switches (run the binary directly so the environment reaches it):
NOTES_SOCK=/tmp/notes.sock NOTES_OPEN_SESSION=s1 ../../build/Notes.app/Contents/MacOS/Notes
#   NOTES_OPEN_SESSION=<id>  open that agent session at launch
#   NOTES_SCROLL_BENCH=1     scroll it top→bottom→top in 6 pt steps, log per-step ms
#   NOTES_SNAPSHOT=<dir>     page through it writing what it draws to dir/shot-N.png
```

`fake_notesd.py --autosend` makes its html part call `sendPrompt()` on load, so the request
log shows the bridge posting a `user` comment.

## Behaviour

- Launch: start `watch`, `list` recursively, open `@last`. If the socket is unreachable the
  window shows how to start notesd; it retries on Retry or when the window becomes key.
- Typing saves 400 ms after the last keystroke with `write` + `if_version`. A
  `version_conflict` keeps your text and shows a banner: Keep Mine (writes over the current
  version) or Use Theirs. Switching notes mid-conflict keeps yours; theirs stays in history.
- ⌘N asks for a name in the sidebar and calls `new` in the selected folder. ⌘⌫ deletes
  the selected note (to the daemon's trash), only while the sidebar has focus.
- Note events refresh the sidebar and reload the open note when it has no unsaved changes.
- No timers or polling: the save timer exists only while there are unsaved changes, and
  the watch thread sleeps in `read()`.

### Agent sessions (sideshow)

- The sidebar's Agents group lists sessions (agent, title), newest activity first; right-click
  renames or deletes. Selecting one shows its timeline in the main pane: post cards oldest
  first, with the session trace folded between them ("··· N steps ···"), opening at the end.
- Each post is several table rows (header, one per part, one per trace step, one per
  comment, footer), so NSTableView lays out and reuses only what's on screen and a live
  update replaces only that post's rows (a row diff; no reloads, no animation).
- Loading happens on its own queue: `post-show`, then `post-render` once per post version
  (cached), `asset-get` once per asset, `kits` once per launch; markdown styling, span →
  attributed text and pele layout happen there too. The main thread only lays out.
- Parts: code/diff/terminal are fixed 18 pt lines of daemon spans in a non-editable TextKit 2
  view with a gutter (diff: file header, +/− counts, old/new numbers, line backgrounds,
  unified or split); markdown uses the editor's styling, read-only; mermaid is pele →
  SVG with pele's CSS variables resolved to the theme → rasterized off the main thread and
  cached by sha256(source + theme + width bucket) under 48 MB (double-click: zoomable
  window); html runs in a WKWebView with sideshow's CSP, tokens, kit CSS/JS and bridge
  (`sendPrompt` → `user` comment on the post, `openLink` → browser for http(s), copy),
  sized by a ResizeObserver, non-persistent data store, pooled (3 idle) and attached only
  while on screen; image is decoded once per asset (double-click opens it); json is an
  NSOutlineView with key order kept and big containers paged in 100s; trace steps expand on
  click and inline steps or a JSON/JSONL trace asset both work.
- Comments sit under each post. The floating composer posts `comment-new` as `user` on the
  last post, or the one whose comment button you pressed; selecting lines in a code or diff
  part anchors it to that line range, option-clicking any part anchors it to that point
  (shown as a pin). Esc drops the anchor. The header has the version picker (shows that
  version in place; a new version resets it to the latest) and delete. New posts and comments arrive over `watch`; at the bottom
  the timeline stays pinned to the newest post.
