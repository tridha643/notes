# Requests for `notes-proto` from `notesd`

`notesd` implements the current contract unchanged. These are the gaps it worked around.

1. **`note-moved` events can't say where the note came from.** `Event` has one `path`, so
   `notesd` sends `{"kind":"note-moved","path":<new path>,"version":N}`. A client keeping a
   sidebar in sync has to re-list to drop the old row. Request: an optional
   `from: Option<String>` on `Event`, set only for `note-moved`.
2. **`Request.cwd` is unused.** Refs are vault-relative and absolute paths are rejected
   (they could escape the vault). If cwd-relative refs are wanted (`cd ~/Notes/work &&
   notes read plan`), the spec should say how they rank against bare titles. Until then
   `notesd` ignores `cwd`.
