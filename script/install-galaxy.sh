#!/usr/bin/env bash
#
# install-galaxy.sh — Download and install Galaxy.app on macOS.
#
# Usage:
#   curl -fsSL https://mng-web-sharing.mini-games.tv/wst-data/ryan-share/galaxy/install-galaxy.sh | bash
#   — or —
#   ./script/install-galaxy.sh
#
#

set -euo pipefail

DOWNLOAD_URL="https://mng-web-sharing.mini-games.tv/wst-data/ryan-share/galaxy/Galaxy.zip"
APP_NAME="Galaxy.app"
INSTALL_DIR="/Applications"

# ---------- helpers ----------
info()  { printf "\033[1;34m==>\033[0m %s\n" "$1"; }
warn()  { printf "\033[1;33m==> WARNING:\033[0m %s\n" "$1"; }
fail()  { printf "\033[1;31m==> ERROR:\033[0m %s\n" "$1"; exit 1; }

if [[ "$(uname -s)" != "Darwin" ]]; then
    fail "This installer only supports macOS."
fi

# ---------- 1. Download ----------
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

info "Downloading Galaxy.zip..."
curl -fSL -o "$TMP_DIR/Galaxy.zip" "$DOWNLOAD_URL" || fail "Failed to download Galaxy.zip"

# ---------- 2. Extract ----------
info "Extracting Galaxy.app..."
unzip -q "$TMP_DIR/Galaxy.zip" -d "$TMP_DIR" || fail "Failed to extract Galaxy.zip"

# Remove macOS zip artifacts that cause "unsealed contents" errors during signing
rm -rf "$TMP_DIR/__MACOSX"
find "$TMP_DIR/$APP_NAME" -name '.DS_Store' -delete 2>/dev/null || true
find "$TMP_DIR/$APP_NAME" -name '._*' -delete 2>/dev/null || true

if [[ ! -d "$TMP_DIR/$APP_NAME" ]]; then
    fail "$APP_NAME not found after extraction."
fi

# ---------- 3. Clear quarantine ----------
info "Removing the macOS quarantine attribute..."
/usr/bin/xattr -dr com.apple.quarantine "$TMP_DIR/$APP_NAME" 2>/dev/null || true

# ---------- 4. Kill, remove, install, launch ----------
info "Stopping any running Galaxy processes..."
pkill -x "Galaxy" 2>/dev/null && sleep 1 || true
pkill -9 -x "Galaxy" 2>/dev/null || true

INSTALLED_APP="$INSTALL_DIR/$APP_NAME"
STAGED_APP="$INSTALL_DIR/.$APP_NAME.new"

info "Staging $APP_NAME in $INSTALL_DIR..."
rm -rf "$STAGED_APP"
cp -R "$TMP_DIR/$APP_NAME" "$STAGED_APP" || fail "Failed to copy $APP_NAME to $INSTALL_DIR."

if [[ -d "$INSTALLED_APP" ]]; then
    info "Removing existing $INSTALLED_APP..."
    rm -rf "$INSTALLED_APP"
fi

info "Installing $APP_NAME to $INSTALL_DIR..."
mv "$STAGED_APP" "$INSTALLED_APP" || fail "Failed to install $APP_NAME."

info "Launching Galaxy..."
open "$INSTALLED_APP"

info "Done! Galaxy is running."
