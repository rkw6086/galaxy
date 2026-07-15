#!/usr/bin/env bash
#
# install-galaxy.sh — Download, sign, and install Galaxy.app on macOS.
#
# Usage:
#   curl -fsSL https://mng-web-sharing.mini-games.tv/wst-data/ryan-share/galaxy/install-galaxy.sh | bash
#   — or —
#   ./script/install-galaxy.sh
#

set -euo pipefail

DOWNLOAD_URL="https://mng-web-sharing.mini-games.tv/wst-data/ryan-share/galaxy/Galaxy.zip"
APP_NAME="Galaxy.app"
INSTALL_DIR="/Applications"

# ---------- helpers ----------
info()  { printf "\033[1;34m==>\033[0m %s\n" "$1"; }
warn()  { printf "\033[1;33m==> WARNING:\033[0m %s\n" "$1"; }
fail()  { printf "\033[1;31m==> ERROR:\033[0m %s\n" "$1"; exit 1; }

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

# ---------- 3. Clear quarantine & ad-hoc sign ----------
info "Clearing quarantine attributes..."
sudo /usr/bin/xattr -cr "$TMP_DIR/$APP_NAME"

info "Ad-hoc code signing..."
codesign --force --deep --sign - "$TMP_DIR/$APP_NAME" || fail "Code signing failed."

# ---------- 4. Kill, remove, install, launch ----------
info "Stopping any running Galaxy processes..."
pkill -x "Galaxy" 2>/dev/null && sleep 1 || true
pkill -9 -x "Galaxy" 2>/dev/null || true

INSTALLED_APP="$INSTALL_DIR/$APP_NAME"
if [[ -d "$INSTALLED_APP" ]]; then
    info "Removing existing $INSTALLED_APP..."
    rm -rf "$INSTALLED_APP"
fi

info "Copying $APP_NAME to $INSTALL_DIR..."
cp -R "$TMP_DIR/$APP_NAME" "$INSTALL_DIR/"

info "Launching Galaxy..."
open "$INSTALLED_APP"

info "Done! Galaxy is running."
