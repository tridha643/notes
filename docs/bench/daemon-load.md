# notesd load: stage's publish and read path

Measured 2026-10-30 on an 18-core, 48 GiB Apple Silicon Mac, release builds made the way
`scripts/install.sh` makes them (`-p notesd -p notes-cli`, so serde_json has `preserve_order`).
Other agents were working on the machine (load average 2–6), so each side ran 5 times,
alternating base and final. Cells are the median of the 5 runs; the range is the final side's.

- **base**: 5287fe5. **final**: cf329c4 (branch codex/stage-daemon-load).
- Harness: `scripts/daemon-load.py --bin <notesd>`. Every case starts a fresh notesd with a temp
  HOME (so the session folder is `$HOME/Desktop/Stage`), NOTES_DB, NOTES_VAULT and NOTES_SOCK. It
  seeds one session with 100 or 1000 posts (markdown, code and json in turn; 8 distinct code
  bodies, so renders mostly hit the cache) and runs 4 s per case. Each client is its own process
  with one persistent connection, like stage's client.
- `publish`: N clients `post-new` into the session. `read`: N clients repeat what stage does on
  an event (`post-list` for the session, then `post-show` and `post-render` of a random post).
  `mixed`: N readers, one client doing bursts of 20 `post-update`s with 100 ms gaps, and a
  `watch` stream timing `post-new` sent to `post-created` received (`publish->event`).
- ops/s for read verbs counts read cycles (one of each verb). Errors are non-`ok` responses
  plus socket failures. Peak RSS is sampled from `ps` every 250 ms.

## Results

| scenario | posts | clients | op | ops/s base → final | p50 ms base → final | p95 ms | p99 ms | peak RSS MB | errors |
|---|---:|---:|---|---|---|---|---|---|---|
| mixed | 100 | 4 | post-list | 1563 → 15354 | 0.83 → 0.14 (0.14–0.15) | 1.51 → 0.17 | 2.49 → 0.20 | 23 → 23 | 0 / 0 |
| mixed | 100 | 4 | post-render | 1563 → 15354 | 0.79 → 0.03 (0.03–0.04) | 1.41 → 0.10 | 2.49 → 0.13 | 23 → 23 | 0 / 0 |
| mixed | 100 | 4 | post-show | 1563 → 15354 | 0.71 → 0.03 (0.03–0.04) | 1.51 → 0.06 | 3.00 → 0.63 | 23 → 23 | 0 / 0 |
| mixed | 100 | 4 | post-update | 90 → 110 | 1.64 → 0.81 (0.81–0.88) | 3.13 → 1.08 | 3.70 → 1.68 | 23 → 23 | 0 / 0 |
| mixed | 100 | 4 | publish->event | 11 → 12 | 2.43 → 0.96 (0.87–1.07) | 5.64 → 2.24 | 6.22 → 3.88 | 23 → 23 | 0 / 0 |
| mixed | 100 | 16 | post-list | 1622 → 17226 | 3.08 → 0.32 (0.31–0.33) | 5.54 → 0.79 | 7.01 → 1.48 | 26 → 24 | 0 / 0 |
| mixed | 100 | 16 | post-render | 1622 → 17226 | 3.04 → 0.22 (0.22–0.22) | 5.42 → 0.68 | 6.54 → 1.38 | 26 → 24 | 0 / 0 |
| mixed | 100 | 16 | post-show | 1622 → 17226 | 2.97 → 0.21 (0.21–0.22) | 5.35 → 0.69 | 6.58 → 1.51 | 26 → 24 | 0 / 0 |
| mixed | 100 | 16 | post-update | 80 → 105 | 4.31 → 1.05 (1.05–1.09) | 9.16 → 1.73 | 10.92 → 2.24 | 26 → 24 | 0 / 0 |
| mixed | 100 | 16 | publish->event | 11 → 12 | 5.50 → 1.27 (1.23–1.46) | 16.90 → 2.69 | 18.89 → 6.97 | 26 → 24 | 0 / 0 |
| mixed | 1000 | 4 | post-list | 340 → 3044 | 4.07 → 1.00 (0.99–1.01) | 7.12 → 1.25 | 12.28 → 2.42 | 38 → 32 | 0 / 0 |
| mixed | 1000 | 4 | post-render | 340 → 3044 | 3.22 → 0.03 (0.03–0.03) | 5.96 → 0.15 | 10.38 → 1.44 | 38 → 32 | 0 / 0 |
| mixed | 1000 | 4 | post-show | 340 → 3044 | 2.71 → 0.03 (0.03–0.04) | 5.85 → 0.28 | 10.91 → 1.84 | 38 → 32 | 0 / 0 |
| mixed | 1000 | 4 | post-update | 65 → 105 | 5.87 → 1.34 (1.33–1.37) | 9.63 → 1.71 | 11.69 → 2.68 | 38 → 32 | 0 / 0 |
| mixed | 1000 | 4 | publish->event | 11 → 11 | 10.41 → 1.50 (1.34–1.60) | 19.88 → 3.30 | 23.03 → 3.98 | 38 → 32 | 0 / 0 |
| mixed | 1000 | 16 | post-list | 379 → 4264 | 13.90 → 1.90 (1.89–1.93) | 17.76 → 3.78 | 37.86 → 5.08 | 41 → 36 | 0 / 0 |
| mixed | 1000 | 16 | post-render | 379 → 4264 | 13.02 → 0.57 (0.55–0.57) | 16.81 → 2.36 | 36.91 → 3.56 | 41 → 36 | 0 / 0 |
| mixed | 1000 | 16 | post-show | 379 → 4264 | 12.70 → 0.60 (0.59–0.61) | 16.81 → 2.24 | 37.74 → 4.15 | 41 → 36 | 0 / 0 |
| mixed | 1000 | 16 | post-update | 40 → 100 | 15.03 → 2.58 (2.54–2.62) | 28.70 → 3.62 | 34.59 → 4.64 | 41 → 36 | 0 / 0 |
| mixed | 1000 | 16 | publish->event | 10 → 12 | 25.43 → 2.57 (2.42–2.85) | 46.35 → 5.26 | 52.70 → 6.68 | 41 → 36 | 0 / 0 |
| publish | 1000 | 1 | post-new | 303 → 732 | 3.20 → 1.31 (1.30–1.34) | 4.31 → 1.93 | 4.50 → 2.04 | 214 → 44 | 0 / 0 |
| publish | 1000 | 16 | post-new | 309 → 747 | 48.34 → 20.16 (19.91–21.16) | 78.42 → 32.88 | 125.28 → 54.23 | 225 → 46 | 0 / 0 |
| read | 1000 | 16 | post-list | 473 → 5296 | 11.46 → 1.83 (1.75–1.91) | 13.41 → 2.54 | 14.33 → 3.19 | 29 → 27 | 0 / 0 |
| read | 1000 | 16 | post-render | 473 → 5296 | 11.08 → 0.51 (0.48–0.53) | 12.97 → 0.98 | 13.72 → 1.29 | 29 → 27 | 0 / 0 |
| read | 1000 | 16 | post-show | 473 → 5296 | 10.23 → 0.53 (0.50–0.56) | 12.18 → 1.01 | 12.99 → 1.34 | 29 → 27 | 0 / 0 |

## What changed and why it was slow

1. **Reads copied the whole session** (d55c6cd). `post-show`, `post-render`, `post-list
   --session`, `session-get` and `trace-list` cloned the session to read one post. They borrow
   it now.
2. **Writes copied every post** (b2dbad1). A write clones the session, changes it and saves the
   clone. Posts are behind `Arc` now, so the clone is one pointer per post.
3. **Every save pretty-printed every post** (746be24). Each session entry keeps each post's
   JSON as it sits in the file, keyed by the post's `Arc`, so a save serializes only new or
   changed posts. The bytes are identical to `to_vec_pretty` (unit test, and an end-to-end
   check of a written file).
4. **The rollback journal read the old file** (bb52e8f). It hard-links the old file aside now.
   Commit unlinks the link, and rollback renames it back. Folders that can't hard-link fall
   back to reading the file.
5. **Saves left freed buffers resident** (23d0367). Each save built a new multi-MB `Vec`, and
   macOS malloc kept the freed blocks (`MALLOC_LARGE (empty)` in `vmmap`). After 3300 publishes
   the footprint was 294 MB on base and 429 MB before this fix, and it is 38 MB now.
6. **Hot reads built a `serde_json::Value`** (cf329c4). `post-list`, `post-show` and
   `post-render` serialize straight to text (`post-list` and `post-show` after the lock is
   released) and splice it into the same response line.

Unchanged: fsync of every file and its folder, the session file format, event order, and
unit atomicity (SQLite and files commit or roll back together). The daemon still serializes
every verb behind one mutex. That mutex and the two fsyncs per publish now cap a publish at
about 750/s on a 1000-post session.

Tried and dropped: swapping the new file in with `renamex_np(RENAME_SWAP)` to skip the hard
link. It was about 6% faster, but a failed folder fsync after the swap would have deleted the
only copy of the old file. An in-memory event ring for `watch` wasn't tried, because delivery
adds only ~0.4 ms on top of the publish itself.
