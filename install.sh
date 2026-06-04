#!/bin/bash
set -e
cd "$(dirname "$0")"

PROFILE="debug"
if [ "$1" = "--release" ]; then
    PROFILE="release"
    CARGO_FLAGS="--release"
fi

APP_PATH="target/${PROFILE}/bundle/osx/Galaxy.app"
INSTALL_PATH="/Applications/Galaxy.app"

echo "=== Building Galaxy (${PROFILE}) ==="
cargo bundle ${CARGO_FLAGS} --bin galaxy-oss --package galaxy

if [ ! -d "$APP_PATH" ]; then
    echo "Error: Bundle not found at ${APP_PATH}"
    exit 1
fi

echo "=== Installing to /Applications ==="
if [ -d "$INSTALL_PATH" ]; then
    rm -rf "$INSTALL_PATH"
fi

cp -R "$APP_PATH" "$INSTALL_PATH"
echo "=== Installed Galaxy.app to /Applications ==="
