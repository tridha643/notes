# Requests for `notes-proto`

Gaps the CLI hit while implementing against `crates/proto`. All three are
resolved on main (f6359af, 7b6f9d7) and the CLI's workarounds are gone.

1. **Resolved: `list` result isn't a list.** `list` now returns
   `Vec<ListEntry>`, folders first; `notes list --json` prints one line per
   entry.
2. **Resolved: `batch` result shape is unspecified.** `batch` now returns
   `Vec<BatchOutcome>`, one `{"ok":true,"result":…}` / `{"ok":false,"error":…}`
   per op; the CLI prints one line per outcome.
3. **Resolved: version conflicts carry the current version only in prose.**
   `version_conflict` now carries `details.current_version`
   (`ErrorBody::current_version()`); the CLI prints it in the exit-5 hint.
