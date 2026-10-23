#!/bin/sh
set -eu

cd "$(dirname "$0")"
cargo build --release

app="dist/MCP Commander.app"
mkdir -p "$app/Contents/MacOS"
cp target/release/mcp-commander "$app/Contents/MacOS/mcp-commander"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>MCP Commander</string>
  <key>CFBundleDisplayName</key><string>MCP Commander</string>
  <key>CFBundleIdentifier</key><string>local.mcp-commander</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleExecutable</key><string>mcp-commander</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$app"
echo "$PWD/$app"
