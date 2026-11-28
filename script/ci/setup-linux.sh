#!/usr/bin/env bash
set -euo pipefail

[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]]
: "${RUNNER_TEMP:?Missing Actions temporary directory}"
: "${GALAXY_JOB_ID:?Missing Actions job identity}"
: "${GITHUB_ENV:?Missing Actions environment output}"
: "${GITHUB_PATH:?Missing Actions path output}"
[[ $RUNNER_TEMP == /* && $GALAXY_JOB_ID =~ ^[0-9]+-[0-9]+$ ]]
cached_tools=
build_target="$RUNNER_TEMP/galaxy-build-$GALAXY_JOB_ID"
if [[ -n ${GALAXY_BUILD_CACHE_ROOT:-} ]]; then
  [[ $GALAXY_BUILD_CACHE_ROOT == "$RUNNER_TEMP/galaxy-ci-cache-v1" &&
     ${GALAXY_CACHED_TOOLS_DIR:-} == "$GALAXY_BUILD_CACHE_ROOT/tools" &&
     ${GALAXY_CACHED_TARGET_DIR:-} == "$RUNNER_TEMP/galaxy-build" ]] || {
    echo 'Build cache paths must match the hosted runner cache directories.' >&2; exit 1;
  }
  for directory in "$GALAXY_BUILD_CACHE_ROOT" "$GALAXY_CACHED_TOOLS_DIR" "$GALAXY_CACHED_TOOLS_DIR/bin" "$GALAXY_CACHED_TARGET_DIR"; do
    [[ ! -L $directory && ( ! -e $directory || -d $directory ) ]] || {
      echo 'Build cache directories must be real directories, never symbolic links.' >&2; exit 1;
    }
  done
  cached_tools=$GALAXY_CACHED_TOOLS_DIR
  build_target=$GALAXY_CACHED_TARGET_DIR
  mkdir -p "$cached_tools/bin"
  [[ ! -L "$cached_tools/bin/cargo-about" && ( ! -e "$cached_tools/bin/cargo-about" || -f "$cached_tools/bin/cargo-about" ) ]] || {
    echo 'Cached tools must be regular files, never symbolic links.' >&2; exit 1;
  }
fi
valid_license_tool() {
  local actual
  [[ -f $1 && ! -L $1 && -x $1 ]] || return 1
  actual=$("$1" --version 2>/dev/null) || return 1
  [[ $actual == 'cargo-about 0.9.3' ]]
}

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential cmake pkg-config libssl-dev libfreetype-dev libexpat1-dev \
  libgit2-dev libfontconfig1-dev libasound2-dev libclang-dev clang \
  libx11-dev libxi-dev libxcursor-dev libxrandr-dev libxinerama-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libegl1-mesa-dev \
  libgl1-mesa-dev libdbus-1-dev protobuf-compiler libprotobuf-dev fakeroot dpkg-dev

# The compiler package alone omits the well-known .proto SDK on Ubuntu.
# prost-build appends PROTOC_INCLUDE to its schema search path explicitly.
export PROTOC=/usr/bin/protoc PROTOC_INCLUDE=/usr/include
[[ -x $PROTOC ]]
for proto in descriptor any api duration empty field_mask source_context struct timestamp type wrappers; do
  [[ -f $PROTOC_INCLUDE/google/protobuf/$proto.proto ]] || {
    echo "Incomplete Protobuf SDK: google/protobuf/$proto.proto is missing." >&2
    exit 1
  }
done
"$PROTOC" --version

export PATH="$HOME/.cargo/bin:$PATH"
export RUSTUP_TOOLCHAIN=1.97.1 CARGO_BUILD_JOBS=2
rustup toolchain install 1.97.1 --profile minimal --target x86_64-unknown-linux-gnu
tools="$RUNNER_TEMP/galaxy-tools-$GALAXY_JOB_ID"
[[ ! -e $tools && ! -L $tools ]]
mkdir "$tools"
mkdir "$tools/bin"
export PATH="$tools/bin:$PATH"
if [[ -n $cached_tools ]] && valid_license_tool "$cached_tools/bin/cargo-about"; then
  cp "$cached_tools/bin/cargo-about" "$tools/bin/cargo-about"
else
  CARGO_TARGET_DIR="$tools/build" RUSTC_WRAPPER='' cargo +1.97.1 install --locked --root "$tools" cargo-about@0.9.3 --features cli
fi
valid_license_tool "$tools/bin/cargo-about" || { echo 'Expected cargo-about 0.9.3.' >&2; exit 1; }
if [[ -n $cached_tools ]]; then
  [[ ! -L "$cached_tools/bin/cargo-about" ]] || { echo 'Cached tools must not be symbolic links.' >&2; exit 1; }
  cp "$tools/bin/cargo-about" "$cached_tools/bin/cargo-about"
fi
[[ ! -L "$tools/build" ]]
rm -rf -- "$tools/build"

echo "$tools/bin" >> "$GITHUB_PATH"
echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
{
  echo 'RUSTUP_TOOLCHAIN=1.97.1'
  echo 'CARGO_BUILD_JOBS=2'
  echo 'CARGO_INCREMENTAL=0'
  echo 'CARGO_PROFILE_RELEASE_DEBUG=0'
  echo 'PKG_CONFIG=/usr/bin/pkg-config'
  echo "PROTOC=$PROTOC"
  echo "PROTOC_INCLUDE=$PROTOC_INCLUDE"
  echo "GALAXY_TOOLS_DIR=$tools"
  echo "CARGO_TARGET_DIR=$build_target"
  echo "GALAXY_RELEASE_DIR=$RUNNER_TEMP/galaxy-artifacts-$GALAXY_JOB_ID"
} >> "$GITHUB_ENV"
