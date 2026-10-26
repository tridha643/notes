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
"$bin/notes" doctor
