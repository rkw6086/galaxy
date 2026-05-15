#!/bin/bash
set -e
cd "$(dirname "$0")"

SIGNING_IDENTITY="Developer ID Application: SAMSUNG ELECTRONICS AMERICA, INC. (3JU72Z7Y3J)"

if [ "$1" = "--release" ]; then
    APP_PATH="target/release/bundle/osx/Galaxy.app"
    DMG_PATH="target/release/bundle/osx/Galaxy.dmg"
    STAGING_DIR="target/release/bundle/osx/dmg-staging"
else
    APP_PATH="target/debug/bundle/osx/Galaxy.app"
    DMG_PATH="target/debug/bundle/osx/Galaxy.dmg"
    STAGING_DIR="target/debug/bundle/osx/dmg-staging"
fi

if [ ! -d "$APP_PATH" ]; then
    echo "Error: $APP_PATH not found."
    exit 1
fi

echo "Verifying app is notarized..."
xcrun stapler validate "$APP_PATH" || {
    echo "Error: App does not have a valid notarization ticket. Run ./sign.sh and ./notarize.sh first."
    exit 1
}

echo "Preparing DMG staging directory..."
rm -rf "$STAGING_DIR"
mkdir -p "$STAGING_DIR"
cp -R "$APP_PATH" "$STAGING_DIR/"
ln -s /Applications "$STAGING_DIR/Applications"

echo "Creating DMG..."
hdiutil detach /Volumes/Galaxy -force 2>/dev/null || true
rm -f "$DMG_PATH"
# Prevent Spotlight/fseventsd from locking the staging dir
touch "$STAGING_DIR/.metadata_never_index"
sleep 1
for attempt in 1 2 3; do
    if hdiutil create -volname "Galaxy" \
        -srcfolder "$STAGING_DIR" \
        -ov -format UDZO \
        "$DMG_PATH"; then
        break
    fi
    if [ "$attempt" -eq 3 ]; then
        echo "Error: hdiutil create failed after 3 attempts"
        exit 1
    fi
    echo "hdiutil failed (attempt $attempt/3), retrying in 3s..."
    sleep 3
done

echo "Signing DMG..."
codesign --force --sign "$SIGNING_IDENTITY" --timestamp "$DMG_PATH"

echo "Submitting DMG to Apple notary service..."
RESULT=$(xcrun notarytool submit "$DMG_PATH" \
    --keychain-profile notarytool-profile \
    --wait 2>&1)
echo "$RESULT"

if ! echo "$RESULT" | grep -q "status: Accepted"; then
    echo ""
    echo "Error: DMG notarization was not accepted. Check output above."
    exit 1
fi

echo "Stapling notarization ticket to DMG..."
xcrun stapler staple "$DMG_PATH"

echo "Cleaning up staging directory..."
rm -rf "$STAGING_DIR"

echo ""
echo "Verifying final DMG..."
xcrun stapler validate "$DMG_PATH"

echo ""
echo "Done. Galaxy.dmg is ready for distribution at:"
echo "  $DMG_PATH"
