#!/bin/bash
set -e
cd "$(dirname "$0")"

echo "=== Galaxy Debug Build + Sign + Notarize + Package ==="
echo ""

cargo bundle --bin galaxy-oss --package galaxy
./sign.sh
./notarize.sh
./package.sh

echo ""
echo "=== Done. Distributable DMG: target/debug/bundle/osx/Galaxy.dmg ==="
