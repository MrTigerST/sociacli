#!/usr/bin/env bash
# Bundle the sociacli binary into a minimal .app and a UDZO-compressed dmg.
#
# Usage:
#   installer/macos/build-dmg.sh <target-triple> <version>
# Example:
#   installer/macos/build-dmg.sh aarch64-apple-darwin 0.1.0
set -euo pipefail

TARGET="${1:?target triple required}"
VERSION="${2:?version required}"

BIN="target/${TARGET}/release/sociacli"
[[ -x "$BIN" ]] || { echo "missing binary: $BIN" >&2; exit 1; }

DIST="dist"
APP="$DIST/sociacli.app"
DMG="$DIST/sociacli-${VERSION}-${TARGET}.dmg"

rm -rf "$DIST"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/sociacli"
chmod +x "$APP/Contents/MacOS/sociacli"

# Stamp the running version into Info.plist
sed -e "s/0\.1\.0/${VERSION}/g" installer/macos/Info.plist > "$APP/Contents/Info.plist"

hdiutil create \
  -volname "sociacli" \
  -srcfolder "$APP" \
  -ov -format UDZO \
  "$DMG"

echo "built: $DMG"
