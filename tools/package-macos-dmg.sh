#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  echo "Usage: $0 APP_PATH OUTPUT_DMG" >&2
  exit 2
fi

app=$1
output=$2
if [ ! -d "$app" ]; then
  echo "App bundle not found: $app" >&2
  exit 1
fi

staging=$(mktemp -d "${TMPDIR:-/tmp}/mcp-commander-dmg.XXXXXX")
trap 'rm -rf "$staging"' EXIT

ditto "$app" "$staging/MCP Commander.app"
ln -s /Applications "$staging/Applications"
codesign --verify --deep --strict "$staging/MCP Commander.app"
hdiutil create -volname 'MCP Commander' -srcfolder "$staging" -ov -format UDZO "$output"
