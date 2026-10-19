# Requests for `notes-proto` from `notes-posts`

Gaps `crates/posts` hit against `crates/proto/src/posts.rs`. The proto is
unchanged; each item says what the crate does today.

1. **`comment-new` can't piggyback feedback.** Sideshow's agent comments are
   writes too, so their response carries unseen user comments, the same as post
   writes. `comment-new` returns a bare `Comment`, so `notes-posts` doesn't
   collect feedback on comments, and the cursor only advances on post writes
   and `wait`. Ask: return `{comment, feedback}` (like `PostWriteResult`).
2. **`DiffFileRender` drops renames and file status.** The patch parser reads
   `rename from/to`, `new file mode` and `deleted file mode`, but the render
   only has `filename`, which is the new path (or the old one for a deletion).
   Ask: `old_filename: Option<String>` and `status: added|deleted|renamed|modified`.
3. **`Kit` has no slot for sideshow's shared `CORE_CSS`.** Sideshow injects it
   once whenever any kit is used. `kits()` prepends it to each kit's `css`,
   which is harmless but duplicated when a part uses both kits. Ask: a
   `core_css` field on the `kits` result, or accept the duplication.
4. **`trace-set` has no append mode.** Sideshow's trace endpoint appends unless
   `reset: true`. `trace-set` replaces the list (the verb says "set"), keeping
   the last 2,000 steps. Ask: `append: bool` if agents should stream steps.
5. **Unspecified result shapes.** Here's what the crate returns, for the daemon
   and CLI to agree on. `trace-set` gives the stored step count (`usize`).
   `setting-get` gives `Option<String>`. `asset-put` gives `AssetInfo`. The
   `*-delete` verbs give no value, only events.
