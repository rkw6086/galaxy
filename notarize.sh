#!/bin/bash
set -e
cd "$(dirname "$0")"

if [ "$1" = "--release" ]; then
    APP_PATH="target/release/bundle/osx/Galaxy.app"
    ZIP_PATH="target/release/bundle/osx/Galaxy.zip"
else
    APP_PATH="target/debug/bundle/osx/Galaxy.app"
    ZIP_PATH="target/debug/bundle/osx/Galaxy.zip"
fi

trap 'rm -f "$ZIP_PATH"' EXIT

if [ ! -d "$APP_PATH" ]; then
    echo "Error: $APP_PATH not found."
    exit 1
fi

echo "Verifying app is signed before notarization..."
codesign --verify --strict "$APP_PATH" || {
    echo "Error: App is not properly signed. Run ./sign.sh first."
    exit 1
}

echo "Creating zip for notarization submission..."
rm -f "$ZIP_PATH"
ditto -c -k --keepParent "$APP_PATH" "$ZIP_PATH"

echo "Submitting to Apple notary service (this may take a few minutes)..."
RESULT=$(xcrun notarytool submit "$ZIP_PATH" \
    --keychain-profile notarytool-profile \
    --wait 2>&1)
echo "$RESULT"

if ! echo "$RESULT" | grep -q "status: Accepted"; then
    echo ""
    echo "Error: Notarization was not accepted. Check output above."
    exit 1
fi

echo "Stapling notarization ticket to app..."
xcrun stapler staple "$APP_PATH"

echo "Validating stapled ticket..."
xcrun stapler validate "$APP_PATH"

echo ""
echo "Done. Galaxy.app is notarized and stapled."
