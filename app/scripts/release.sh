#!/bin/sh
# Build, sign, notarize, staple, repackage as dmg+zip, upload the GitHub
# Release, and add the appcast entry. Does NOT bump the version (edit
# app/Cargo.toml first) or git commit/push — that stays manual.
set -eu

: "${SIGN_IDENTITY:?set SIGN_IDENTITY, e.g. \"Developer ID Application: Your Name (TEAMID)\"}"
NOTARY_PROFILE="${NOTARY_PROFILE:-betternotepad-notary}"
APP_NAME="Better Notepad"

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT/app"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
echo "Releasing v$VERSION"

# 1. Build, sign, produce the initial .app + .dmg
SIGN_IDENTITY="$SIGN_IDENTITY" scripts/build-dmg.sh

BUNDLE_DIR="target/release/bundle"
APP="$BUNDLE_DIR/osx/$APP_NAME.app"
DMG="$BUNDLE_DIR/dmg/$APP_NAME.dmg"

# 2. Verify signature
codesign --verify --deep --strict --verbose=2 "$APP"

# 3-4. Submit for notarization and wait
ditto -c -k --keepParent "$APP" notarize-submission.zip
xcrun notarytool submit notarize-submission.zip \
  --keychain-profile "$NOTARY_PROFILE" --wait
rm -f notarize-submission.zip

# 5-6. Staple and confirm Gatekeeper accepts it
xcrun stapler staple "$APP"
spctl -a -vvv --type exec "$APP"

# 7. Rebuild the dmg from the now-stapled app
rm -f "$DMG"
uvx dmgbuild -s scripts/dmg-settings.py -D app="$APP" "$APP_NAME" "$DMG"

# 8. Zip the stapled app for Sparkle and sign it
ZIP="BetterNotepad-$VERSION.zip"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
SIGN_UPDATE_OUT="$(vendor/bin/sign_update "$ZIP")"

# 9. CFBundleVersion (goes in the appcast's <sparkle:version>)
CFBUNDLE_VERSION="$(/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$APP/Contents/Info.plist")"
MIN_SYS="$(sed -n 's/^minimum_system_version = "\(.*\)"/\1/p' Cargo.toml | head -1)"

# 10. Upload dmg (human download) + zip (Sparkle enclosure) to a GitHub Release
gh release create "v$VERSION" "$DMG" "$ZIP" \
  --title "v$VERSION" --notes "Release v$VERSION." --repo sachithrrra/betternotepad

# 11. Add the appcast entry (newest first) — does not commit or push
APPCAST="$REPO_ROOT/site/public/appcast.xml"
TMP_ITEM="$(mktemp)"
cat > "$TMP_ITEM" <<EOF
    <item>
      <title>Version $VERSION</title>
      <pubDate>$(date -u '+%a, %d %b %Y %H:%M:%S +0000')</pubDate>
      <sparkle:version>$CFBUNDLE_VERSION</sparkle:version>
      <sparkle:shortVersionString>$VERSION</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>$MIN_SYS</sparkle:minimumSystemVersion>
      <enclosure
        url="https://github.com/sachithrrra/betternotepad/releases/download/v$VERSION/$ZIP"
        $SIGN_UPDATE_OUT
        type="application/octet-stream" />
    </item>
EOF
sed -i '' "/<language>en<\/language>/r $TMP_ITEM" "$APPCAST"
rm -f "$TMP_ITEM"

echo
echo "Released v$VERSION. Appcast entry added to $APPCAST — review, commit, and push manually."
