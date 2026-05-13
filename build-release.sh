#!/bin/bash
set -e
cd "$(dirname "$0")"

echo "=== Galaxy Release Build + Sign + Notarize + Package ==="
echo ""

cargo bundle --release --bin galaxy-oss --package galaxy
./sign.sh --release
./notarize.sh --release
./package.sh --release

echo ""
echo "=== Done. Distributable DMG: target/release/bundle/osx/Galaxy.dmg ==="
