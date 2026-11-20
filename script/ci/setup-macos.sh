#!/bin/bash
# Install unsigned tools shared by releases, Nightlies and macOS preflight.
set -euo pipefail

: "${RUNNER_TEMP:?Missing Actions temporary directory}"
: "${GALAXY_JOB_ID:?Missing Actions job identity}"
: "${GITHUB_PATH:?Missing Actions path output}"
: "${GITHUB_ENV:?Missing Actions environment output}"
[[ "$RUNNER_TEMP" == /* && "$GALAXY_JOB_ID" =~ ^[0-9]+-[0-9]*$ ]]
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
  for binary in cargo-bundle cargo-about; do
    [[ ! -L "$cached_tools/bin/$binary" && ( ! -e "$cached_tools/bin/$binary" || -f "$cached_tools/bin/$binary" ) ]] || {
      echo 'Cached tools must be regular files, never symbolic links.' >&2; exit 1;
    }
  done
fi
valid_tool() {
  local actual
  [[ -f $1 && ! -L $1 && -x $1 ]] || return 1
  actual=$("$1" --version 2>/dev/null) || return 1
  [[ $actual == "$2" ]]
}
test "$(uname -s)" = Darwin
test "$(uname -m)" = arm64
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
command -v rustup >/dev/null || { echo 'Install rustup for the runner user first.' >&2; exit 1; }
command -v brew >/dev/null || { echo 'Install Homebrew for the runner user first.' >&2; exit 1; }
# Bindgen must parse Xcode's SIMD headers with the matching libclang. Selecting
# only a Homebrew library can mix different Clang versions in one invocation.
export CLANG_PATH=$(xcrun --sdk macosx --find clang)
export CC="$CLANG_PATH" CXX=$(xcrun --sdk macosx --find clang++)
xcode_bin=$(dirname "$CLANG_PATH")
export LIBCLANG_PATH=$(cd "$xcode_bin/../lib" && pwd)
export SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
xcode_resource=$("$CLANG_PATH" -print-resource-dir)
[[ -x "$CC" && -x "$CXX" && -f "$LIBCLANG_PATH/libclang.dylib" && -f "$xcode_resource/include/arm_neon.h" && -d "$SDKROOT" ]] || { echo 'A complete matching Xcode compiler, libclang, headers, and macOS SDK are required.' >&2; exit 1; }
export PATH="$xcode_bin:$PATH"
"$CLANG_PATH" --version
protobuf_prefix=$(brew --prefix protobuf 2>/dev/null || true)
protobuf_complete() {
  [[ -n $protobuf_prefix && -x $protobuf_prefix/bin/protoc ]] || return 1
  local proto
  for proto in descriptor any api duration empty field_mask source_context struct timestamp type wrappers; do
    [[ -f $protobuf_prefix/include/google/protobuf/$proto.proto ]] || return 1
  done
}
if ! protobuf_complete; then
  if brew list --versions protobuf >/dev/null 2>&1; then
    brew reinstall protobuf
  else
    brew install protobuf
  fi
  protobuf_prefix=$(brew --prefix protobuf)
fi
protobuf_complete || { echo 'A complete Protobuf compiler and well-known schema SDK is required.' >&2; exit 1; }
export PROTOC="$protobuf_prefix/bin/protoc" PROTOC_INCLUDE="$protobuf_prefix/include"
"$PROTOC" --version
rustup toolchain install 1.97.1 --profile minimal --component rustfmt --target aarch64-apple-darwin
if ! xcrun -sdk macosx metal --version >/dev/null 2>&1; then
  xcodebuild -downloadComponent MetalToolchain
fi
xcrun -sdk macosx metal --version
xcrun -sdk macosx --find metallib
for package in cmake pkgconf; do
  brew list --versions "$package" >/dev/null 2>&1 || brew install "$package"
done
[[ "$RUNNER_TEMP" == /* && "$GALAXY_JOB_ID" =~ ^[0-9]+-[0-9]*$ ]]
tools="$RUNNER_TEMP/galaxy-tools-$GALAXY_JOB_ID"
[[ ! -e "$tools" && ! -L "$tools" ]]
mkdir "$tools"
mkdir "$tools/bin"
export RUSTUP_TOOLCHAIN=1.97.1
# Keep compiler intermediates inside the same job-owned cleanup boundary as tools.
export CARGO_TARGET_DIR="$tools/build"
# 0.11.0 has the required Cargo target-directory and named-binary support without
# 0.12.0's mandatory Sigstore dependency, which fails to compile on this toolchain.
# Apple signing is performed separately by package-macos.sh.
if [[ -n $cached_tools ]] && valid_tool "$cached_tools/bin/cargo-bundle" 'cargo-bundle v0.11.0'; then
  cp "$cached_tools/bin/cargo-bundle" "$tools/bin/cargo-bundle"
else
  RUSTC_WRAPPER='' cargo install --locked --root "$tools" --git https://github.com/burtonageo/cargo-bundle --rev 739f92c37c789b5511a448a389cbc76fcebd99df cargo-bundle
fi
if [[ -n $cached_tools ]] && valid_tool "$cached_tools/bin/cargo-about" 'cargo-about 0.8.4'; then
  cp "$cached_tools/bin/cargo-about" "$tools/bin/cargo-about"
else
  RUSTC_WRAPPER='' cargo install --locked --root "$tools" cargo-about@0.8.4
fi
valid_tool "$tools/bin/cargo-bundle" 'cargo-bundle v0.11.0' || { echo 'Expected cargo-bundle 0.11.0 from the pinned revision.' >&2; exit 1; }
valid_tool "$tools/bin/cargo-about" 'cargo-about 0.8.4' || { echo 'Expected cargo-about 0.8.4.' >&2; exit 1; }
if [[ -n $cached_tools ]]; then
  for binary in cargo-bundle cargo-about; do
    [[ ! -L "$cached_tools/bin/$binary" ]] || { echo 'Cached tools must not be symbolic links.' >&2; exit 1; }
    cp "$tools/bin/$binary" "$cached_tools/bin/$binary"
  done
fi
# Cargo install intermediates are not runtime tools. Hosted macOS runners have
# a small ephemeral disk; reclaim only this job's exact installation directory.
[[ "$tools" == "$RUNNER_TEMP/galaxy-tools-$GALAXY_JOB_ID" && ! -L "$tools/build" ]]
rm -rf -- "$tools/build"
echo "$tools/bin" >> "$GITHUB_PATH"
echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
echo '/opt/homebrew/bin' >> "$GITHUB_PATH"
echo "$xcode_bin" >> "$GITHUB_PATH"
echo 'RUSTUP_TOOLCHAIN=1.97.1' >> "$GITHUB_ENV"
echo "CLANG_PATH=$CLANG_PATH" >> "$GITHUB_ENV"
echo "LIBCLANG_PATH=$LIBCLANG_PATH" >> "$GITHUB_ENV"
echo "CC=$CC" >> "$GITHUB_ENV"
echo "CXX=$CXX" >> "$GITHUB_ENV"
echo "SDKROOT=$SDKROOT" >> "$GITHUB_ENV"
echo "PROTOC=$PROTOC" >> "$GITHUB_ENV"
echo "PROTOC_INCLUDE=$PROTOC_INCLUDE" >> "$GITHUB_ENV"
echo "GALAXY_TOOLS_DIR=$tools" >> "$GITHUB_ENV"
echo "CARGO_TARGET_DIR=$build_target" >> "$GITHUB_ENV"
echo "GALAXY_RELEASE_DIR=$RUNNER_TEMP/galaxy-artifacts-$GALAXY_JOB_ID" >> "$GITHUB_ENV"
