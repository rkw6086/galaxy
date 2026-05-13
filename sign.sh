#!/bin/bash
set -e
cd "$(dirname "$0")"

SIGNING_IDENTITY="Developer ID Application: SAMSUNG ELECTRONICS AMERICA, INC. (3JU72Z7Y3J)"
ENTITLEMENTS="BuildSupport/Galaxy.entitlements"

if [ "$1" = "--release" ]; then
    APP_PATH="target/release/bundle/osx/Galaxy.app"
else
    APP_PATH="target/debug/bundle/osx/Galaxy.app"
fi

if [ ! -d "$APP_PATH" ]; then
    echo "Error: $APP_PATH not found."
    echo "Run first: cargo bundle --bin galaxy-oss --package galaxy"
    exit 1
fi

if [ ! -f "$ENTITLEMENTS" ]; then
    echo "Error: $ENTITLEMENTS not found."
    exit 1
fi

JUNK=$(find "$APP_PATH" -maxdepth 1 -not -name "Contents" -not -path "$APP_PATH" 2>/dev/null || true)
if [ -n "$JUNK" ]; then
    echo "Removing unsealed items from bundle root: $JUNK"
    echo "$JUNK" | xargs rm -rf
fi

echo "Removing .DS_Store files..."
find "$APP_PATH" -name '.DS_Store' -delete 2>/dev/null || true

echo "Stripping extended attributes..."
xattr -cr "$APP_PATH"

echo "Signing $APP_PATH..."
codesign --force --sign "$SIGNING_IDENTITY" \
    --entitlements "$ENTITLEMENTS" \
    --options runtime \
    --timestamp \
    "$APP_PATH/Contents/MacOS/galaxy-oss"

codesign --force --sign "$SIGNING_IDENTITY" \
    --entitlements "$ENTITLEMENTS" \
    --options runtime \
    --timestamp \
    "$APP_PATH"

echo "Verifying signature..."
codesign --verify --strict "$APP_PATH"
codesign -dvvv "$APP_PATH" 2>&1 | grep -E "Authority|TeamIdentifier|Runtime"
echo "Architecture: $(lipo -archs "$APP_PATH/Contents/MacOS/galaxy-oss")"

echo ""
echo "Done. Galaxy.app is signed and ready for notarization."
