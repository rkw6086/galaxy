#!/usr/bin/env bash
# Compatibility entrypoint for building and uploading Galaxy to Hermes.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/build-and-deploy-hermes.sh" "$@"
