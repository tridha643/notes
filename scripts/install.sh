#!/bin/sh
# Build notesd + notes + stage, install them to ~/.local/bin, and load the notesd LaunchAgent.
# launchd owns the socket and starts notesd on the first connection; nothing runs before that.
set -eu
cd "$(dirname "$0")/.."
. "$HOME/.cargo/env"
cargo build --release -p notesd -p notes-cli -p stage
bin="$HOME/.local/bin"
support="$HOME/Library/Application Support/Notes"
plist="$HOME/Library/LaunchAgents/dev.notes.notesd.plist"
mkdir -p "$bin" "$support" "$HOME/Notes" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
install -m 755 target/release/notesd target/release/notes target/release/stage "$bin/"
# stage-helper (Swift): mermaid via pele, image decoding, live HTML frames for stage.
# It loads pele from Notes_NotesCore.bundle, which must sit next to the binary.
swift build -c release --package-path apps/Notes --product stage-helper
swiftbin="$(swift build -c release --package-path apps/Notes --show-bin-path)"
install -m 755 "$swiftbin/stage-helper" "$bin/"
rm -rf "$bin/Notes_NotesCore.bundle" && cp -R "$swiftbin/Notes_NotesCore.bundle" "$bin/"
# The stage skill, for every agent that reads a skills folder, so agents find `notes post`.
for skills in "$HOME/.claude/skills" "$HOME/.codex/skills" "$HOME/.pi/agent/skills" "$HOME/.agents/skills"; do
  if [ -d "$skills" ]; then rm -rf "$skills/stage" && cp -R skills/stage "$skills/stage"; fi
done
# Standard, not Background: notesd answers the UI and the CLI, so it shouldn't be throttled.
cat > "$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.notes.notesd</string>
  <key>ProgramArguments</key><array><string>$bin/notesd</string></array>
  <key>Sockets</key><dict><key>Listeners</key><dict>
    <key>SockPathName</key><string>$support/notesd.sock</string>
    <key>SockPathMode</key><integer>384</integer>
  </dict></dict>
  <key>ProcessType</key><string>Standard</string>
  <key>RunAtLoad</key><false/>
  <key>StandardErrorPath</key><string>$HOME/Library/Logs/notesd.log</string>
</dict>
</plist>
PLIST
launchctl bootout "gui/$(id -u)/dev.notes.notesd" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$plist"
# launchd needs a moment after bootstrap before the socket answers.
for _ in 1 2 3 4 5 6 7 8 9 10; do "$bin/notes" doctor 2>/dev/null && exit 0; sleep 0.2; done
"$bin/notes" doctor
