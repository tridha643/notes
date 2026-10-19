#!/bin/bash
# Builds apps/Notes in release and assembles an ad-hoc signed build/Notes.app.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
package="$root/apps/Notes"
app="$root/build/Notes.app"

swift build -c release --package-path "$package"
bin="$(swift build -c release --package-path "$package" --show-bin-path)"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$bin/Notes" "$app/Contents/MacOS/Notes"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key><string>dev.notes.app</string>
	<key>CFBundleName</key><string>Notes</string>
	<key>CFBundleExecutable</key><string>Notes</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>0.1</string>
	<key>CFBundleVersion</key><string>1</string>
	<key>LSMinimumSystemVersion</key><string>26.0</string>
	<key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
	<key>NSHighResolutionCapable</key><true/>
	<key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST

codesign -s - --force "$app"
echo "$app"
