#!/usr/bin/env bash
#
# install-galaxy.sh — Download, sign, and install Galaxy.app on macOS.
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
PROCESS_NAME="galaxy-oss"
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

# ---------- 3. Clear quarantine & ad-hoc sign ----------
info "Clearing quarantine attributes..."
/usr/bin/xattr -cr "$TMP_DIR/$APP_NAME" || fail "Failed to clear quarantine attributes."

info "Applying an ad-hoc code signature..."
/usr/bin/codesign --force --deep --sign - "$TMP_DIR/$APP_NAME" || fail "Code signing failed."

info "Verifying the code signature..."
/usr/bin/codesign --verify --deep --strict --verbose=2 "$TMP_DIR/$APP_NAME" ||
    fail "Code signature verification failed."

# ---------- 4. Kill, remove, install, launch ----------
info "Stopping any running Galaxy processes..."
pkill -x "$PROCESS_NAME" 2>/dev/null && sleep 1 || true
pkill -9 -x "$PROCESS_NAME" 2>/dev/null || true

INSTALLED_APP="$INSTALL_DIR/$APP_NAME"
STAGED_APP="$INSTALL_DIR/.$APP_NAME.new"

info "Staging $APP_NAME in $INSTALL_DIR..."
rm -rf "$STAGED_APP"
cp -R "$TMP_DIR/$APP_NAME" "$STAGED_APP" || fail "Failed to copy $APP_NAME to $INSTALL_DIR."

/usr/bin/codesign --verify --deep --strict --verbose=2 "$STAGED_APP" ||
    fail "Installed application signature verification failed."

if [[ -d "$INSTALLED_APP" ]]; then
    info "Removing existing $INSTALLED_APP..."
    rm -rf "$INSTALLED_APP"
fi

info "Installing $APP_NAME to $INSTALL_DIR..."
mv "$STAGED_APP" "$INSTALLED_APP" || fail "Failed to install $APP_NAME."

info "Launching Galaxy..."
/usr/bin/open "$INSTALLED_APP" || fail "macOS failed to launch Galaxy."

for _ in {1..50}; do
    if pgrep -x "$PROCESS_NAME" >/dev/null; then
        sleep 2
        pgrep -x "$PROCESS_NAME" >/dev/null || fail "Galaxy exited immediately after launch."
        info "Done! Galaxy is running."
        exit 0
    fi
    sleep 0.1
done

fail "Galaxy did not start. Run $INSTALLED_APP/Contents/MacOS/$PROCESS_NAME to see the launch error."
