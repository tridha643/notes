#!/bin/sh
# Install stage on macOS (Apple Silicon):
#   curl -fsSL https://raw.githubusercontent.com/tridha643/stage/main/install.sh | sh
# Installs notesd, notes, stage and stage-helper into ~/.local/bin, loads the notesd
# LaunchAgent (launchd owns the socket and starts notesd on first connect), and copies
# the stage skill into every agent skills folder it finds. Run it again to upgrade.
# STAGE_VERSION=v0.1.0 pins a release; `sh install.sh DIR` installs an unpacked build
# (scripts/install.sh uses that for source builds).
set -eu
repo="tridha643/stage"
asset="stage-macos-arm64.tar.gz"
[ "$(uname -s)" = Darwin ] || { echo "stage runs on macOS only" >&2; exit 1; }
[ "$(uname -m)" = arm64 ] || { echo "stage needs an Apple Silicon Mac" >&2; exit 1; }
[ "$(sw_vers -productVersion | cut -d. -f1)" -ge 26 ] || { echo "stage needs macOS 26 or later" >&2; exit 1; }

dist="${1:-}"
if [ -z "$dist" ]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  case "${STAGE_VERSION:-latest}" in
    latest) url="https://github.com/$repo/releases/latest/download/$asset" ;;
    *) url="https://github.com/$repo/releases/download/$STAGE_VERSION/$asset" ;;
  esac
  echo "downloading $url"
  curl -fsSL "$url" | tar -xz -C "$tmp"
  dist="$tmp"
fi

bin="$HOME/.local/bin"
support="$HOME/Library/Application Support/Notes"
plist="$HOME/Library/LaunchAgents/dev.notes.notesd.plist"
# Keep a host's session directory across upgrades; an explicit value wins.
stage_dir="${NOTES_STAGE_DIR:-$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:NOTES_STAGE_DIR' "$plist" 2>/dev/null || true)}"
case "$stage_dir" in
  ""|/*) ;;
  *) echo "NOTES_STAGE_DIR must be an absolute path" >&2; exit 1 ;;
esac
mkdir -p "$bin" "$support" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
install -m 755 "$dist/bin/notesd" "$dist/bin/notes" "$dist/bin/stage" "$dist/bin/stage-helper" "$bin/"
# stage-helper loads pele (mermaid) from its resource bundle, which must sit next to it.
for bundle in "$dist"/bin/*.bundle; do
  rm -rf "$bin/$(basename "$bundle")" && cp -R "$bundle" "$bin/"
done
for skills in "$HOME/.claude/skills" "$HOME/.codex/skills" "$HOME/.pi/agent/skills" "$HOME/.agents/skills"; do
  if [ -d "$skills" ]; then rm -rf "$skills/stage" && cp -R "$dist/skills/stage" "$skills/stage"; fi
done
# Standard, not Background: notesd answers stage and the CLI, so it shouldn't be throttled.
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
if [ -n "$stage_dir" ]; then
  plutil -insert EnvironmentVariables -dictionary "$plist"
  plutil -insert EnvironmentVariables.NOTES_STAGE_DIR -string "$stage_dir" "$plist"
fi
launchctl bootout "gui/$(id -u)/dev.notes.notesd" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$plist"
# stage runs in Herdr panes, and its kitty graphics need a current Herdr. Replace an older
# copy (Homebrew's or a loose binary) with the latest release in $bin. Not installed: skip.
if have="$(herdr --version 2>/dev/null)"; then
  have="${have#herdr }"
  # A failed lookup or download leaves the old herdr in place rather than failing the install.
  latest="$(curl -fsSL -o /dev/null -w '%{url_effective}' https://github.com/herdrdev/herdr/releases/latest || true)"
  latest="${latest##*/tag/v}"
  if [ -n "$latest" ] && [ "$have" != "$latest" ] && [ "$(printf '%s\n%s\n' "$have" "$latest" | sort -V | tail -1)" = "$latest" ] &&
    curl -fsSL -o "$bin/herdr.new" "https://github.com/herdrdev/herdr/releases/download/v$latest/herdr-macos-aarch64"; then
    echo "replacing herdr $have with $latest"
    chmod 755 "$bin/herdr.new"
    if brew list herdr >/dev/null 2>&1; then brew uninstall herdr; fi
    old="$(command -v herdr || true)"
    if [ -n "$old" ] && [ "$old" != "$bin/herdr" ]; then rm -f "$old" || echo "remove $old by hand: it shadows $bin/herdr" >&2; fi
    mv "$bin/herdr.new" "$bin/herdr"
    echo "a running herdr keeps $have until you restart it (herdr server stop)"
  fi
fi
case ":$PATH:" in
  *":$bin:"*) ;;
  *) echo "add $bin to your PATH: echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.zshrc" >&2 ;;
esac
# launchd needs a moment after bootstrap before the socket answers.
sock="$support/notesd.sock"
for _ in 1 2 3 4 5 6 7 8 9 10; do "$bin/notes" doctor --sock "$sock" >/dev/null 2>&1 && break; sleep 0.2; done
"$bin/notes" doctor --sock "$sock"
echo "installed. Open a terminal pane and run: stage"
