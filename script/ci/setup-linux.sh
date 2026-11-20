#!/usr/bin/env bash
set -euo pipefail

[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]]
: "${RUNNER_TEMP:?Missing Actions temporary directory}"
: "${GALAXY_JOB_ID:?Missing Actions job identity}"
: "${GITHUB_ENV:?Missing Actions environment output}"
: "${GITHUB_PATH:?Missing Actions path output}"
[[ $RUNNER_TEMP == /* && $GALAXY_JOB_ID =~ ^[0-9]+-[0-9]+$ ]]

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential cmake pkg-config libssl-dev libfreetype-dev libexpat1-dev \
  libgit2-dev libfontconfig1-dev libasound2-dev libclang-dev clang \
  libx11-dev libxi-dev libxcursor-dev libxrandr-dev libxinerama-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libegl1-mesa-dev \
  libgl1-mesa-dev libdbus-1-dev protobuf-compiler fakeroot dpkg-dev

export PATH="$HOME/.cargo/bin:$PATH"
export RUSTUP_TOOLCHAIN=1.97.1 CARGO_BUILD_JOBS=2
rustup toolchain install 1.97.1 --profile minimal --target x86_64-unknown-linux-gnu
tools="$RUNNER_TEMP/galaxy-tools-$GALAXY_JOB_ID"
[[ ! -e $tools && ! -L $tools ]]
mkdir "$tools"
CARGO_TARGET_DIR="$tools/build" cargo install --locked --root "$tools" cargo-about@0.8.4
rm -rf -- "$tools/build"

echo "$tools/bin" >> "$GITHUB_PATH"
echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
{
  echo 'RUSTUP_TOOLCHAIN=1.97.1'
  echo 'CARGO_BUILD_JOBS=2'
  echo 'CARGO_INCREMENTAL=0'
  echo 'CARGO_PROFILE_RELEASE_DEBUG=0'
  echo 'PKG_CONFIG=/usr/bin/pkg-config'
  echo "GALAXY_TOOLS_DIR=$tools"
  echo "CARGO_TARGET_DIR=$RUNNER_TEMP/galaxy-build-$GALAXY_JOB_ID"
  echo "GALAXY_RELEASE_DIR=$RUNNER_TEMP/galaxy-artifacts-$GALAXY_JOB_ID"
} >> "$GITHUB_ENV"
