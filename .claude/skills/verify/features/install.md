# Install: the curl installer and a source install

Other Macs install from a GitHub release: `curl -fsSL
https://raw.githubusercontent.com/tridha643/stage/main/install.sh | sh` downloads
`stage-macos-arm64.tar.gz` (bin/ with notesd, notes, stage, stage-helper and its
`Notes_NotesCore.bundle`, and skills/stage), installs it into `~/.local/bin`, writes and loads
the `dev.notes.notesd` LaunchAgent, copies the stage skill into every agent skills folder it
finds, and ends with `notes doctor`. `scripts/install.sh` builds the same layout from source
(`scripts/dist.sh`) and runs `install.sh` on it. `scripts/release.sh vX.Y.Z` publishes it.

## Sub-features

- Fresh install from the latest release, or a pinned one (`STAGE_VERSION=v0.1.0`).
- Upgrade in place: an existing `NOTES_STAGE_DIR` in the LaunchAgent survives.
- Source install via `scripts/install.sh`.
- Refuses a non-arm64 or non-macOS host with one line.

## How to get to it (user POV)

The README's Install section.

## Driving it

This replaces the user's installed binaries and restarts their notesd, so run it only when
that's the intent (it is the repo's normal "try my change" step).

```sh
scripts/dist.sh /tmp/stage-dist && sh install.sh /tmp/stage-dist     # what the release installs
curl -fsSL https://raw.githubusercontent.com/tridha643/stage/main/install.sh | sh   # after a release
```

Proof: `notes doctor` succeeds at the end of the run; `launchctl print gui/$(id -u)/dev.notes.notesd`
shows the socket at `~/Library/Application Support/Notes/notesd.sock`; `~/.local/bin` holds the
four binaries and `Notes_NotesCore.bundle`; `~/.claude/skills/stage/SKILL.md` matches
`skills/stage/SKILL.md`; `stage` opens and draws a Mermaid card (drive.py against the fixture).

## Gotchas

- The tarball must carry `Notes_NotesCore.bundle` next to stage-helper, or Mermaid never renders.
- `stage-helper` is built for macOS 26 (`vtool -show-build`); the Rust binaries for macOS 11.
- curl doesn't set the quarantine flag, so Gatekeeper doesn't block the unsigned binaries; a
  tarball downloaded in a browser would need `xattr -dr com.apple.quarantine`.
