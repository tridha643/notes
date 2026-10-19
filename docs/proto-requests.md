# Requests for `notes-proto`

Gaps the CLI hit while implementing against `crates/proto`. The CLI works
around each one today; none needs a wire change to ship P0.

1. **`list` result isn't a list.** `ListResult` is `{folders, notes}`, so
   `notes list --json` can't be NDJSON without inventing a line shape. The CLI
   prints it as one JSON line. Option: return `Vec<Entry>` where an entry is a
   note summary or `{"folder": …}`, so it streams like every other list.
2. **`batch` result shape is unspecified.** The spec says "one result line per
   op"; the CLI prints each element when the result is an array, else the
   result as one line. Proposal: document `result` as an array with one
   `{"ok":true,"result":…}` / `{"ok":false,"error":…}` per op, so a
   non-atomic batch can report per-op failures.
3. **Version conflicts carry the current version only in prose.** The spec says
   exit 5 "carries the current version" on stderr; today that relies on the
   daemon putting it in `message`. Proposal: `details: {"current_version": N}`
   on `version_conflict`, so scripts can retry without parsing text.
