#!/usr/bin/env bash
#
# install-galaxy.sh — Clone, build, and install Galaxy.app on macOS.
#
# Usage:
#   curl -fsSL https://mng-web-sharing.mini-games.tv/wst-data/ryan-share/galaxy/install-galaxy.sh | bash
#   — or —
#   ./script/install-galaxy.sh
#

set -euo pipefail

REPO_URL="git@gitlab.com:samnasbo/shared/galaxy.git"
CLONE_DIR="$HOME/.galaxy/source"
APP_NAME="Galaxy.app"
INSTALL_DIR="/Applications"
BUNDLE_BIN="galaxy-oss"
BUNDLE_PKG="galaxy"

# ---------- helpers ----------
info()  { printf "\033[1;34m==>\033[0m %s\n" "$1"; }
warn()  { printf "\033[1;33m==> WARNING:\033[0m %s\n" "$1"; }
fail()  { printf "\033[1;31m==> ERROR:\033[0m %s\n" "$1"; exit 1; }

# ---------- 1. Xcode / CLI tools ----------
info "Checking Xcode and Command Line Tools..."

if ! xcode-select -p &>/dev/null; then
    fail "Xcode Command Line Tools are not installed. Run: xcode-select --install"
fi

if ! xcrun --show-sdk-path &>/dev/null; then
    fail "Xcode SDK not found. Ensure Xcode or Command Line Tools are properly installed."
fi

if ! xcrun -f metal &>/dev/null; then
    warn "Metal compiler not found. Attempting to download Metal toolchain..."
    xcodebuild -downloadComponent MetalToolchain || warn "Could not download Metal toolchain — build may fail."
fi

info "Xcode prerequisites look good."

# ---------- 2. Homebrew ----------
if ! command -v brew &>/dev/null; then
    info "Installing Homebrew..."
    /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

    # Add brew to PATH for Apple Silicon
    if [[ -f /opt/homebrew/bin/brew ]]; then
        eval "$(/opt/homebrew/bin/brew shellenv)"
    fi
fi

if ! command -v pkgconf &>/dev/null && ! command -v pkg-config &>/dev/null; then
    info "Installing pkgconf via Homebrew..."
    brew install pkgconf
fi

# ---------- 3. Rust toolchain ----------
if ! command -v rustup &>/dev/null; then
    info "Installing Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
fi

info "Syncing Rust toolchain (rust-toolchain.toml will pin the exact version)..."
rustup show active-toolchain &>/dev/null || rustup default stable

# aarch64-apple-darwin target for Apple Silicon
rustup target add aarch64-apple-darwin 2>/dev/null || true

# cargo-bundle for producing the .app
if ! cargo bundle --help &>/dev/null 2>&1; then
    info "Installing cargo-bundle..."
    cargo install cargo-bundle \
        --git=https://github.com/burtonageo/cargo-bundle \
        --rev ae4c76e92c08774bf54ff077b1c52e3d1cd6c16d
fi

# ---------- 4. Clone the repo ----------
if [[ -d "$CLONE_DIR/.git" ]]; then
    info "Repository already exists at $CLONE_DIR — pulling latest..."
    git -C "$CLONE_DIR" pull --ff-only || warn "Pull failed; building with current checkout."
else
    info "Cloning Galaxy into $CLONE_DIR..."
    mkdir -p "$(dirname "$CLONE_DIR")"
    git clone "$REPO_URL" "$CLONE_DIR"
fi

# ---------- 5. Build ----------
info "Building $BUNDLE_BIN (release)..."
cargo build --release --bin "$BUNDLE_BIN" --package "$BUNDLE_PKG" \
    --manifest-path "$CLONE_DIR/Cargo.toml"

info "Bundling $APP_NAME..."
# cargo-bundle does not support --manifest-path; run from repo root
pushd "$CLONE_DIR" > /dev/null
cargo bundle --release --bin "$BUNDLE_BIN" --package "$BUNDLE_PKG"
popd > /dev/null

BUILT_APP="$CLONE_DIR/target/release/bundle/osx/$APP_NAME"
if [[ ! -d "$BUILT_APP" ]]; then
    fail "Bundle not found at $BUILT_APP — build may have failed."
fi

# ---------- 6. Kill, remove, install, launch ----------
info "Stopping any running Galaxy processes..."
pkill -x "Galaxy" 2>/dev/null && sleep 1 || true
pkill -9 -x "Galaxy" 2>/dev/null || true

INSTALLED_APP="$INSTALL_DIR/$APP_NAME"
if [[ -d "$INSTALLED_APP" ]]; then
    info "Removing existing $INSTALLED_APP..."
    rm -rf "$INSTALLED_APP"
fi

info "Copying $APP_NAME to $INSTALL_DIR..."
cp -R "$BUILT_APP" "$INSTALL_DIR/"

info "Launching Galaxy..."
open "$INSTALLED_APP"

info "Done! Galaxy is running."
