# `notes` CLI bench

Measured 2026-10-18 on an Apple M5, macOS 26.6.2, rustc 1.99.1, release build
(`cargo build --release -p notes-cli`, 1.7 MB binary). The daemon is a fake: a
Python unix-socket server that answers every connection with one canned
`doctor` result, so these numbers cover the CLI process and one socket round
trip, not real `notesd` work.

```
hyperfine -N --warmup 50 --runs 1000 \
  'notes doctor --sock $SOCK' 'notes --version' /usr/bin/true
```

| Command | Mean | σ | Min | Max | Peak RSS |
|---|---:|---:|---:|---:|---:|
| `notes doctor` (fake daemon) | 5.4 ms | 1.0 ms | 2.6 ms | 18.3 ms | 2.5 MiB |
| `notes --version` (no socket) | 5.2 ms | 1.3 ms | 2.4 ms | 14.2 ms | 2.2 MiB |
| `/usr/bin/true` (process floor) | 1.9 ms | 0.3 ms | 1.5 ms | 6.6 ms | 1.2 MiB |

`doctor` is inside the spec's 10 ms warm-call budget. The connect, request and
response add about 0.2 ms over `--version`; the remaining ~3.3 ms over
`/usr/bin/true` is process startup of the binary itself (dyld, Rust runtime,
clap), which is where to look if the budget gets tighter.

## Re-measured after sideshow verbs, 2026-10-18

Same machine and command, after adding `--fuzzy`, batch exit codes,
`agent-howto`, `completions` (clap_complete) and the sideshow verbs (`post`,
`upload`, `comment(s)`, `wait`, `sessions`, `session`, `trace`, `kits`). The
binary is now 2.6 MB. The old binary (main 69a1f80, 1.7 MB) ran in the same
hyperfine invocation so both saw the same load; the machine was busy (load
average 7 to 9, `/usr/bin/true` at 3.3 ms instead of 1.9 ms), so read the
means as noisy and compare the two binaries to each other.

| Command | Mean | σ | Min | Max | Peak RSS |
|---|---:|---:|---:|---:|---:|
| `notes doctor` (new) | 5.4 ms | 1.2 ms | 2.8 ms | 12.6 ms | 3.1 MiB |
| `notes doctor` (main) | 5.0 ms | 1.2 ms | 2.9 ms | 10.3 ms | 2.5 MiB |
| `notes --version` (new) | 5.0 ms | 1.2 ms | 2.6 ms | 11.5 ms | 2.8 MiB |
| `notes --version` (main) | 5.3 ms | 1.2 ms | 2.7 ms | 10.9 ms | 2.3 MiB |
| `/usr/bin/true` | 3.3 ms | 1.6 ms | 1.5 ms | 10.3 ms | 1.2 MiB |

Wall time didn't move: the minimums match within 0.1 ms, and a second round
flipped which binary had the lower mean. Peak RSS grew about 0.6 MiB, most
likely clap building the larger subcommand tree at startup plus the bigger
binary's pages (not profiled); if that ever matters, that's the place to look.

Reproduce: start any server on a short socket path (macOS caps unix socket
paths at 104 bytes, so long scratch dirs fail with exit 6), then run the
hyperfine line above.
