#!/usr/bin/env bash
# Build and deploy Galaxy to Hermes (wst.mini-games.tv)
#
# Required env vars:
#   HERMES_PASS - password for Hermes authentication
#
# Optional env vars:
#   HERMES_USER - username (defaults to "ryan")

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$SCRIPT_DIR/build-and-deploy-hermes"

if [ -z "$HERMES_PASS" ]; then
  echo "Error: HERMES_PASS environment variable is required."
  exit 1
fi

# Install deps if needed
if [ ! -d "$APP_DIR/node_modules" ]; then
  echo "Installing dependencies..."
  cd "$APP_DIR" && yarn install --frozen-lockfile
fi

cd "$APP_DIR" && yarn --silent start