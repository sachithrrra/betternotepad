#!/bin/sh
# Builds, signs, and packages a distributable .dmg: cargo bundle makes the
# .app, we sign it, then dmgbuild wraps it in a styled drag-to-install .dmg
# (needs uv, for `uvx dmgbuild`).
# Does not notarize/staple.
set -eu

: "${SIGN_IDENTITY:?set SIGN_IDENTITY, e.g. \"Developer ID Application: Your Name (TEAMID)\"}"
APP_NAME="Better Notepad"

cd "$(dirname "$0")/.."
cargo bundle --release --format osx

BUNDLE_DIR="target/release/bundle"
APP="$BUNDLE_DIR/osx/$APP_NAME.app"

codesign --deep --force --options runtime --timestamp \
  --sign "$SIGN_IDENTITY" "$APP"
codesign --verify --deep --strict "$APP"

# Drag-to-install window: layout and background live in scripts/dmg-settings.py.
DMG="$BUNDLE_DIR/dmg/$APP_NAME.dmg"
rm -f "$DMG"
uvx dmgbuild -s scripts/dmg-settings.py -D app="$APP" "$APP_NAME" "$DMG"

echo "Signed app:  $APP"
echo "Installable: $DMG"
