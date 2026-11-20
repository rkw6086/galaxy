#!/usr/bin/env bash
# Produce native Linux packages; no desktop application is launched.
set -euo pipefail
fail() { echo "Galaxy Linux build: $*" >&2; exit 1; }
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || fail 'Use a native Linux x86_64 runner.'
[[ ${CARGO_TARGET_DIR:-} == /* && ${GALAXY_RELEASE_DIR:-} == /* ]] || fail 'Use absolute job-specific build and artifact directories.'
[[ $(rustc --version) == 'rustc 1.97.1 '* ]] || fail 'Use Rust 1.97.1.'
version=${GALAXY_RELEASE_VERSION:-}
sha=${GALAXY_RELEASE_SHA:-}
channel=${GALAXY_BUILD_CHANNEL:-}
[[ $version =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || fail 'A validated version is required.'
[[ $sha =~ ^[0-9a-f]{40}$ && $(git rev-parse HEAD) == "$sha" ]] || fail 'Checkout differs from the validated source commit.'
[[ $channel == release || $channel == nightly || $channel == ci ]] || fail 'Unknown build channel.'
if [[ $channel == nightly ]]; then
  [[ ${GITHUB_RUN_ID:-} =~ ^[0-9]+$ && ${GITHUB_RUN_ATTEMPT:-} =~ ^[1-9][0-9]*$ && ${GALAXY_JOB_ID:-} == "$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT" ]] || fail 'Nightly build needs its current workflow run and attempt.'
  [[ ${GALAXY_NIGHTLY_ID:-} =~ ^${sha}_([0-9]+)-([1-9][0-9]*)$ && ${BASH_REMATCH[1]} == "$GITHUB_RUN_ID" ]] || fail 'Nightly identity must match the validated source and workflow run.'
  prepared_attempt=${BASH_REMATCH[2]}
  (( 10#$prepared_attempt <= 10#$GITHUB_RUN_ATTEMPT )) || fail 'Nightly identity cannot come from a future workflow attempt.'
fi
verify_source() {
  if [[ $channel == release ]]; then
    [[ ${GALAXY_VERSION_STAGED:-} == 1 ]] || fail 'Tagged releases require controlled version staging.'
    node script/release.mjs verify-staged "$version" "$sha"
  else
    node script/release.mjs check "$version"
    [[ -z $(git status --porcelain) ]] || fail 'Build requires an unchanged source checkout.'
  fi
}
verify_source
export GIT_RELEASE_TAG="$version" GALAXY_GIT_COMMIT="$sha" GALAXY_GIT_DIRTY=false
export GALAXY_BUILD_CHANNEL=$channel
[[ $channel != ci ]] || export GALAXY_BUILD_CHANNEL=release
export CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0
target=x86_64-unknown-linux-gnu
features=release_bundle,gui,nld_classifier_v3,nld_heuristic_v2,galaxy_control_cli
cargo build --locked --release --target "$target" --bin galaxy-oss --bin generate_settings_schema --features "$features"
binary_dir="$CARGO_TARGET_DIR/$target/release"
[[ -x $binary_dir/galaxy-oss && -x $binary_dir/generate_settings_schema ]] || fail 'Built executables are missing.'
strip --strip-unneeded "$binary_dir/galaxy-oss"
schema="$CARGO_TARGET_DIR/settings_schema.json"
"$binary_dir/generate_settings_schema" --channel oss "$schema"
resources="$CARGO_TARGET_DIR/galaxy-resources"
NO_LICENSES= SKIP_SETTINGS_SCHEMA= REQUIRE_WORMHOLE_HELPERS=1 SETTINGS_SCHEMA_CACHE="$schema" \
  bash script/prepare_bundled_resources "$resources" oss release
node --input-type=module - "$root" "$resources/wormhole-helpers" <<'JS'
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const [root, destination] = process.argv.slice(2);
const { inspectWormholeHelpers, verifyWormholeHelpers } = await import(pathToFileURL(path.join(root, 'script/lib/bundled-wormhole-helpers.mjs')));
await verifyWormholeHelpers(await inspectWormholeHelpers(path.join(root, 'resources/wormhole-helpers')), destination);
JS
[[ ! -e $GALAXY_RELEASE_DIR && ! -L $GALAXY_RELEASE_DIR ]] || fail 'Use a fresh artifact directory.'
mkdir -p "$GALAXY_RELEASE_DIR"
if [[ $channel == release ]]; then
  stem="Galaxy_$version"
elif [[ $channel == nightly ]]; then
  stem="Galaxy_nightly_$GALAXY_NIGHTLY_ID"
else
  stem="Galaxy_ci_$sha"
fi
export WORKSPACE_ROOT_DIR="$root" DIST_DIR="$CARGO_TARGET_DIR/galaxy-dist"
export OUT_DIR="$GALAXY_RELEASE_DIR" CARGO_TARGET_OUTPUT_DIR="$binary_dir"
export RELEASE_CHANNEL=oss BUNDLE_ID=com.galaxy.GalaxyOss
export EXECUTABLE_PATH="$binary_dir/galaxy-oss" BINARY_NAME=galaxy-oss ARTIFACT=app
export BUILD_ARCH=x86_64 CARGO_PROFILE=release GALAXY_PREPARED_RESOURCES_DIR="$resources"
bash script/linux/bundle_deb
deb="$GALAXY_RELEASE_DIR/galaxy-oss_${version}_amd64.deb"
[[ -s $deb ]] || fail 'Debian package is missing.'
[[ $(dpkg-deb -f "$deb" Version) == "$version" && $(dpkg-deb -f "$deb" Architecture) == amd64 ]] || fail 'Debian package identity differs from the build.'
mv "$deb" "$GALAXY_RELEASE_DIR/${stem}_amd64.deb"

portable="$CARGO_TARGET_DIR/galaxy-portable/Galaxy"
mkdir -p "$portable"
install -m 755 "$binary_dir/galaxy-oss" "$portable/galaxy-oss"
cp -R "$resources" "$portable/resources"
install -m 644 app/channels/oss/icon/no-padding/512x512.png "$portable/galaxy.png"
tar -czf "$GALAXY_RELEASE_DIR/${stem}_linux_x86_64.tar.gz" -C "$(dirname "$portable")" Galaxy
(
  cd "$GALAXY_RELEASE_DIR"
  sha256sum "${stem}_amd64.deb" "${stem}_linux_x86_64.tar.gz" > SHA256SUMS
  sha256sum --check SHA256SUMS
)
verify_source
echo "Linux packages are ready in $GALAXY_RELEASE_DIR"
