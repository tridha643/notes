# Requests for `notes-proto`

Gaps the CLI hit while implementing against `crates/proto`. Resolved on main
and the CLI's workaround is gone.

1. **Resolved: `batch` result shape is unspecified.** `batch` now returns
   `Vec<BatchOutcome>`, one `{"ok":true,"result":…}` / `{"ok":false,"error":…}`
   per op; the CLI prints one line per outcome.
