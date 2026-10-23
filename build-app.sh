#!/bin/sh
set -eu

cd "$(dirname "$0")"
cargo build --release

version=$(sed -n 's/^version = "\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)"$/\1/p' Cargo.toml)
if [ -z "$version" ]; then
  echo "Cannot read package version from Cargo.toml" >&2
  exit 1
fi
release_label=${version%.0}

app="dist/MCP Commander.app"
mkdir -p "$app/Contents/MacOS"
cp target/release/mcp-commander "$app/Contents/MacOS/mcp-commander"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>MCP Commander</string>
  <key>CFBundleDisplayName</key><string>MCP Commander $release_label</string>
  <key>CFBundleIdentifier</key><string>local.mcp-commander</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleExecutable</key><string>mcp-commander</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$app"
echo "MCP Commander $release_label: $PWD/$app"
