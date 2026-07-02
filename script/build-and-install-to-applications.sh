#!/usr/bin/env bash
# Build and install Galaxy to /Applications

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$SCRIPT_DIR/build-and-install-to-applications"

# Install deps if needed
if [ ! -d "$APP_DIR/node_modules" ]; then
  echo "Installing dependencies..."
  cd "$APP_DIR" && yarn install --frozen-lockfile
fi

cd "$APP_DIR" && yarn --silent start
