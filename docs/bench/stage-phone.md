# Stage phone rendering and scrolling check

Measured 2026-10-30 against Rex and the installed stage-helper.

## Target

The connected iPhone client reported 49 columns × 19 rows, with 23 × 51 physical
pixels per terminal cell. Use these measured terminal dimensions, not a guessed
phone model's CSS viewport. Test sizes also included 39×12 (keyboard), 49×36,
92×19 (landscape), and 120×40 (desktop).

## Findings and fixes

- A resize during helper startup/probing could be lost before crossterm's input
  reader started. The phone then displayed an 80×24 desktop layout, clipped by
  its real terminal. Re-read size and cell pixels after starting input, and do
  not enqueue the stale first-frame dimensions.
- A narrow terminal hid the sidebar permanently, including search results.
  Replace the feed with a session picker/search view; preserve feed geometry and
  reading position behind it, and remove graphics placements while it is open.
- Every wheel tick moved three lines, including in a 16-line phone feed. Narrow
  and short feeds now move one line per tick; bounds do not request redraws.
- Scrolling back in the latest post kept auto-follow enabled. Wheel/page
  scrolling now follows new posts only once the feed reaches its bottom.
- Metadata obscured the scroll counter. Compact title bars reserve its width.
- Long prompts hid the insertion point. Show the tail of the input instead.
- Card labels, hanging words, and wide tables could exceed their width. Clip
  labels cell-wise, wrap hanging words within the remaining space, and stack
  wide table rows into labelled fields.
- The fixed 2x HTML backing scale gave the phone a 506 CSS-pixel page in 44
  usable cells. Cell-relative typography gives it ~317.49 CSS px, while still
  producing exactly 1012 physical pixels. Desktop 14×30 cells keep the 2x scale.
- Disable unused all-pointer-motion reporting; retain clicks, drag selection,
  and wheel events.

## Verification

`cargo build && cargo test` passed, including 84 stage library tests and four
real-daemon stage integration tests. New regressions cover the exact phone
viewport, picker round-trips, tappable search, long input, 2,000 boundary wheel
events, auto-follow, table/list/label fitting, and HTML scale/pixel agreement.

A live Rex smoke test used an isolated temporary HOME/vault/database and real
notesd, installed Stage, and installed Swift helper. It sent SGR wheel/click
sequences via `rex send`, then checked `rex capture` after each action:

1. Resize immediately after launch to 49×19 / 23×51 cells, including while the
   graphics helper starts (this reproduced the stale-layout bug before the fix).
2. Twelve downward wheel events: exactly twelve lines, checked one at a time.
3. 1,000 upward and 1,000 downward events: clamp at the respective ends.
4. Tap the footer's down control: advance 15 lines in a 16-line feed.
5. Tap menu, then the open session: identical viewport before and after.
6. Search and tap the result; type a long unsent reply: insertion point visible.
7. Cycle keyboard, portrait, landscape and desktop sizes, then return to phone.
8. Publish an HTML card: verify helper output and a real kitty placement.

Three consecutive final runs passed. Server-side draw times: p50 0.13–0.14 ms,
p95 0.26–0.35 ms, maximum 0.33–0.41 ms (44 draws/run). HTML's first frame took
40.8–45.7 ms and measured 1012×395 px; the placement was cropped to the visible
44×7 cells as expected. These timings exclude network latency and client rendering.

This verifies terminal input and graphics, not physical touchscreen recognition
or iOS inertia. Those remain Rex client behaviour and need an on-device swipe
check. HTML still has its documented viewport-height cap.

## Installation caveat

The normal install script also rebuilds/restarts notesd. On this machine the
restarted daemon blocked in `Stage::load → read_dir → open` for Desktop access,
consistent with a pending macOS privacy prompt. `notes doctor` timed out. Do not
bypass privacy controls: the user must approve Desktop access on the Mac. The
isolated smoke tests above use only generated fixtures outside protected folders.
