#!/bin/bash
# Install unsigned tools shared by releases, Nightlies and macOS preflight.
set -euo pipefail

: "${RUNNER_TEMP:?Missing Actions temporary directory}"
: "${GALAXY_JOB_ID:?Missing Actions job identity}"
: "${GITHUB_PATH:?Missing Actions path output}"
: "${GITHUB_ENV:?Missing Actions environment output}"
[[ "$RUNNER_TEMP" == /* && "$GALAXY_JOB_ID" =~ ^[0-9]+-[0-9]*$ ]]
test "$(uname -s)" = Darwin
test "$(uname -m)" = arm64
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
command -v rustup >/dev/null || { echo 'Install rustup for the runner user first.' >&2; exit 1; }
command -v brew >/dev/null || { echo 'Install Homebrew for the runner user first.' >&2; exit 1; }
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
for package in cmake pkgconf llvm; do
  brew list --versions "$package" >/dev/null 2>&1 || brew install "$package"
done
[[ "$RUNNER_TEMP" == /* && "$GALAXY_JOB_ID" =~ ^[0-9]+-[0-9]*$ ]]
tools="$RUNNER_TEMP/galaxy-tools-$GALAXY_JOB_ID"
[[ ! -e "$tools" && ! -L "$tools" ]]
mkdir "$tools"
export RUSTUP_TOOLCHAIN=1.97.1
# Keep compiler intermediates inside the same job-owned cleanup boundary as tools.
export CARGO_TARGET_DIR="$tools/build"
# 0.11.0 has the required Cargo target-directory and named-binary support without
# 0.12.0's mandatory Sigstore dependency, which fails to compile on this toolchain.
# Apple signing is performed separately by package-macos.sh.
cargo install --locked --root "$tools" --git https://github.com/burtonageo/cargo-bundle --rev 739f92c37c789b5511a448a389cbc76fcebd99df cargo-bundle
cargo install --locked --root "$tools" cargo-about@0.8.4
# Cargo install intermediates are not runtime tools. Hosted macOS runners have
# a small ephemeral disk; reclaim only this job's exact installation directory.
[[ "$tools" == "$RUNNER_TEMP/galaxy-tools-$GALAXY_JOB_ID" && -d "$tools/build" && ! -L "$tools/build" ]]
rm -rf -- "$tools/build"
echo "$tools/bin" >> "$GITHUB_PATH"
echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
echo '/opt/homebrew/bin' >> "$GITHUB_PATH"
echo 'RUSTUP_TOOLCHAIN=1.97.1' >> "$GITHUB_ENV"
echo "PROTOC=$PROTOC" >> "$GITHUB_ENV"
echo "PROTOC_INCLUDE=$PROTOC_INCLUDE" >> "$GITHUB_ENV"
echo "GALAXY_TOOLS_DIR=$tools" >> "$GITHUB_ENV"
echo "CARGO_TARGET_DIR=$RUNNER_TEMP/galaxy-build-$GALAXY_JOB_ID" >> "$GITHUB_ENV"
echo "GALAXY_RELEASE_DIR=$RUNNER_TEMP/galaxy-artifacts-$GALAXY_JOB_ID" >> "$GITHUB_ENV"
