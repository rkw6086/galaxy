#!/bin/bash
# Build the complete unsigned app before making Apple credentials available.
set -euo pipefail

fail() { echo "Galaxy macOS build: $*" >&2; exit 1; }
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || fail 'Use a native Apple Silicon macOS runner.'
[[ -n ${CARGO_TARGET_DIR:-} && $CARGO_TARGET_DIR == /* && -n ${GALAXY_RELEASE_DIR:-} && $GALAXY_RELEASE_DIR == /* ]] || fail 'Set absolute job-specific CARGO_TARGET_DIR and GALAXY_RELEASE_DIR.'
[[ $(rustc --version) == 'rustc 1.97.1 '* ]] || fail 'Use Rust 1.97.1 from rust-toolchain.toml.'
for tool in node python3 cargo-bundle pkg-config cmake; do command -v "$tool" >/dev/null || fail "Missing build tool: $tool"; done
cargo about --version >/dev/null
xcode-select -p >/dev/null
for tool in clang metal metallib actool notarytool stapler; do xcrun --find "$tool" >/dev/null || fail "Xcode tool unavailable: $tool"; done

version=${GALAXY_RELEASE_VERSION:-}
[[ $version =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || fail 'A validated stable version is required.'
export GALAXY_BUILD_CHANNEL=${GALAXY_BUILD_CHANNEL:-release}
[[ $GALAXY_BUILD_CHANNEL == release || $GALAXY_BUILD_CHANNEL == nightly || $GALAXY_BUILD_CHANNEL == beta ]] || fail 'Build channel must be release or nightly.'
if [[ ${GALAXY_UNSIGNED_PREFLIGHT:-} == 1 ]]; then
  [[ $GALAXY_BUILD_CHANNEL == release ]] || fail 'Unsigned preflight cannot produce a beta build.'
  if [[ ${GALAXY_GITHUB_CI:-} == 1 ]]; then
    [[ ${GITHUB_SERVER_URL:-} == https://github.com && ${GITHUB_REPOSITORY:-} == rkw6086/galaxy ]] || fail 'GitHub CI requires the Galaxy repository.'
    [[ ${GITHUB_EVENT_NAME:-} == pull_request || ( ${GITHUB_EVENT_NAME:-} == push && ${GITHUB_REF:-} == refs/heads/master ) ]] || fail 'Unsigned GitHub CI requires a pull request or master push.'
  else
    [[ ${GITHUB_EVENT_NAME:-} == workflow_dispatch && ${GITHUB_REF:-} == refs/heads/master ]] || fail 'Unsigned preflight is restricted to manual builds of master.'
  fi
elif [[ $GALAXY_BUILD_CHANNEL == nightly ]]; then
  [[ ( ${GITHUB_EVENT_NAME:-} == schedule || ${GITHUB_EVENT_NAME:-} == workflow_dispatch ) && ${GITHUB_REF:-} == refs/heads/master ]] || fail 'Nightly builds require a scheduled or manual master run.'
  [[ ${GALAXY_RELEASE_SHA:-} =~ ^[0-9a-f]{40}$ && ${GITHUB_RUN_ID:-} =~ ^[1-9][0-9]{0,18}$ && ${GITHUB_RUN_ATTEMPT:-} =~ ^[1-9][0-9]{0,8}$ ]] || fail 'Nightly source or run identity is invalid.'
  nightly_pattern="^${GALAXY_RELEASE_SHA}_${GITHUB_RUN_ID}-([1-9][0-9]{0,8})$"
  [[ ${GALAXY_NIGHTLY_ID:-} =~ $nightly_pattern ]] || fail 'Nightly identity differs from the prepared source and run.'
  (( ${BASH_REMATCH[1]} <= GITHUB_RUN_ATTEMPT )) || fail 'Nightly preparation attempt exceeds this run attempt.'
elif [[ $GALAXY_BUILD_CHANNEL == beta ]]; then
  [[ ( ${GITHUB_EVENT_NAME:-} == schedule || ${GITHUB_EVENT_NAME:-} == workflow_dispatch ) && ${GITHUB_REF:-} == refs/heads/master ]] || fail 'Beta builds require a scheduled or manual master run.'
  [[ ${GALAXY_RELEASE_SHA:-} =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ && ${GALAXY_BETA_ID:-} == "$GALAXY_RELEASE_SHA" ]] || fail 'Beta identity must match the validated master commit.'
else
  [[ ${GITHUB_EVENT_NAME:-} == push && ${GITHUB_REF:-} == "refs/tags/$version" ]] || fail "Release builds require a pushed $version tag."
fi
[[ $(git rev-parse HEAD) == "${GALAXY_RELEASE_SHA:-}" ]] || fail 'Checkout does not match the validated release commit.'
verify_source() {
  if [[ $GALAXY_BUILD_CHANNEL == release && ${GALAXY_UNSIGNED_PREFLIGHT:-} != 1 ]]; then
    [[ ${GALAXY_VERSION_STAGED:-} == 1 ]] || fail 'Tagged releases require controlled CI version staging.'
    node script/release.mjs verify-staged "$version" "$GALAXY_RELEASE_SHA"
  else
    node script/release.mjs check "$version"
    [[ -z $(git status --porcelain) ]] || fail 'Build requires an unchanged source checkout.'
  fi
}
verify_source
node script/ci/release-identity.mjs
[[ ${GALAXY_UNSIGNED_PREFLIGHT:-} != 1 ]] || export GALAXY_GITHUB_CI=0
export GIT_RELEASE_TAG="$version"
export GALAXY_GIT_COMMIT="$GALAXY_RELEASE_SHA"
export GALAXY_GIT_DIRTY=false
export MACOSX_DEPLOYMENT_TARGET=10.14
# Keep bindgen's compiler/include discovery and loaded library on the same active
# Xcode toolchain, even if another LLVM installation is present on the runner.
export CLANG_PATH=$(xcrun --sdk macosx --find clang)
export CC="$CLANG_PATH" CXX=$(xcrun --sdk macosx --find clang++)
xcode_bin=$(dirname "$CLANG_PATH")
export LIBCLANG_PATH=$(cd "$xcode_bin/../lib" && pwd)
export SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
xcode_resource=$("$CLANG_PATH" -print-resource-dir)
[[ -x "$CC" && -x "$CXX" && -f "$LIBCLANG_PATH/libclang.dylib" && -f "$xcode_resource/include/arm_neon.h" && -d "$SDKROOT" ]] || fail 'A complete matching Xcode compiler, libclang, headers, and macOS SDK are required.'
export PATH="$xcode_bin:$PATH"

target=aarch64-apple-darwin
features=autoupdate,release_bundle,extern_plist,gui,nld_classifier_v3,nld_heuristic_v2,galaxy_control_cli
app="$CARGO_TARGET_DIR/$target/release/bundle/osx/Galaxy.app"
[[ ! -e "$app" && ! -L "$app" ]] || fail 'Unsigned bundle already exists; use a fresh job target directory.'
# The pinned cargo-bundle supports CARGO_BUNDLE_SKIP_BUILD but not --locked.
# Compile with Cargo's lockfile protection first, then package that exact binary.
cargo build --locked --release --target "$target" --bin galaxy-oss --bin generate_settings_schema --features "$features"
(
  cd app
  CARGO_BUNDLE_SKIP_BUILD=1 cargo bundle --release --format osx --target "$target" --bin galaxy-oss --features "$features"
)
[[ -x "$app/Contents/MacOS/galaxy-oss" ]] || fail 'Galaxy executable missing from app bundle.'
python3 - "$app/Contents/MacOS/galaxy-oss" <<'PY'
import re, subprocess, sys
binary = sys.argv[1]
rpath = '@executable_path/../Frameworks'
commands = subprocess.check_output(['/usr/bin/otool', '-l', binary], text=True)
paths = re.findall(r'cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset', commands)
if rpath not in paths:
    subprocess.run(['/usr/bin/install_name_tool', '-add_rpath', rpath, binary], check=True)
PY

# app/build.rs currently stages this plug-in under the repository target directory,
# independently of CARGO_TARGET_DIR. Copy its freshly built universal bundle.
plugin=GalaxyDockTilePlugin.docktileplugin
[[ -x "$root/target/release/$plugin/Contents/MacOS/GalaxyDockTilePlugin" ]] || fail 'Built DockTile plug-in is missing.'
mkdir -p "$app/Contents/PlugIns"
ditto "$root/target/release/$plugin" "$app/Contents/PlugIns/$plugin"

export WARP_SCHEME_NAME=galaxyoss
export WARP_PLIST_PATH="$app/Contents/Info.plist"
bash script/update_plist
python3 - "$app" "$version" "$GALAXY_BUILD_CHANNEL" "$GALAXY_RELEASE_SHA" "${GALAXY_NIGHTLY_ID:-}" "${GALAXY_ARTIFACT_IDENTITY:-}" "${GALAXY_RELEASE_TIMESTAMP:-}" <<'PY'
import pathlib, plistlib, sys
app = pathlib.Path(sys.argv[1])
version, channel, commit, build_id, identity, built_at = sys.argv[2:]
path = app / 'Contents/Info.plist'
with path.open('rb') as stream:
    data = plistlib.load(stream)
data.update(CFBundleVersion=version, CFBundleShortVersionString=version,
            WarpVersion=version, LSBackgroundOnly=False,
            GalaxyBuildChannel=channel, GalaxySourceCommit=commit,
            GalaxyBuildId=build_id, GalaxyArtifactIdentity=identity, GalaxyBuiltAt=built_at,
            SUPPORTS_CHAT_GPT_WORK_WITH_APPS=True,
            NSDockTilePlugIn='GalaxyDockTilePlugin.docktileplugin')
for key, value in list(data.items()):
    if key.endswith('UsageDescription') and isinstance(value, str):
        data[key] = value.replace('Warp', 'Galaxy')
data['NSServices'] = [dict(NSMenuItem={'default': f'New Galaxy {kind} Here'},
                          NSMessage=message,
                          NSRequiredContext={'NSTextContent': 'FilePath'},
                          NSSendTypes=['NSFilenamesPboardType', 'public.plain-text'])
                      for kind, message in [('Tab', 'openTab'), ('Window', 'openWindow')]]
with path.open('wb') as stream:
    plistlib.dump(data, stream)
plugin = app / 'Contents/PlugIns/GalaxyDockTilePlugin.docktileplugin/Contents/Info.plist'
with plugin.open('rb') as stream:
    plugin_data = plistlib.load(stream)
plugin_data['MainAppBundleIdentifier'] = data['CFBundleIdentifier']
with plugin.open('wb') as stream:
    plistlib.dump(plugin_data, stream)
PY

resources="$app/Contents/Resources"
schema="$CARGO_TARGET_DIR/settings_schema.json"
"$CARGO_TARGET_DIR/$target/release/generate_settings_schema" --channel oss "$schema"
NO_LICENSES= SKIP_SETTINGS_SCHEMA= REQUIRE_WORMHOLE_HELPERS=1 SETTINGS_SCHEMA_CACHE="$schema" \
  bash script/prepare_bundled_resources "$resources" oss release
# The shared helper verifies both committed Linux archives without rebuilding them.
node --input-type=module - "$root" "$resources/wormhole-helpers" <<'JS'
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const [root, destination] = process.argv.slice(2);
const { inspectWormholeHelpers, verifyWormholeHelpers } = await import(pathToFileURL(path.join(root, 'script/lib/bundled-wormhole-helpers.mjs')));
await verifyWormholeHelpers(await inspectWormholeHelpers(path.join(root, 'resources/wormhole-helpers')), destination);
JS
bash script/compile_icon oss "$app"
bash script/macos/create_galaxyctrl_wrapper "$resources/bin/galaxyctrl-oss" ../../MacOS/galaxy-oss
cat > "$resources/bin/galaxy-ai-oss" <<'SH'
#!/bin/bash
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec -a "$0" "$script_dir/../../MacOS/galaxy-oss" "$@"
SH
chmod +x "$resources/bin/galaxy-ai-oss"

# Fail before signing if a release would depend on this runner's Homebrew paths
# or an unbundled @rpath library. Linux Wormhole archives are resources, not code
# executed on macOS, so only inspect Mach-O files.
python3 - "$app" <<'PY'
import pathlib, re, subprocess, sys
app = pathlib.Path(sys.argv[1]).resolve()
main = app / 'Contents/MacOS'
def rpaths(binary):
    commands = subprocess.check_output(['/usr/bin/otool', '-l', str(binary)], text=True)
    return [value.replace('@loader_path', str(binary.parent)).replace('@executable_path', str(main))
            for value in re.findall(r'cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset', commands)]
main_rpaths = rpaths(main / 'galaxy-oss')
for binary in app.rglob('*'):
    if not binary.is_file() or binary.is_symlink():
        continue
    kind = subprocess.check_output(['/usr/bin/file', '-b', str(binary)], text=True)
    if 'Mach-O' not in kind:
        continue
    subprocess.run(['/usr/bin/lipo', str(binary), '-verify_arch', 'arm64'], check=True)
    def expand(value):
        return value.replace('@loader_path', str(binary.parent)).replace('@executable_path', str(main))
    search_paths = rpaths(binary) + main_rpaths
    identities = subprocess.check_output(['/usr/bin/otool', '-D', str(binary)], text=True).splitlines()[1:]
    dependencies = subprocess.check_output(['/usr/bin/otool', '-L', str(binary)], text=True).splitlines()[1:]
    for line in dependencies:
        value = line.strip().split(' (compatibility version', 1)[0]
        if not value or value.endswith(':') or value in identities:
            continue
        if value.startswith(('/System/Library/', '/usr/lib/')):
            continue
        candidates = ([pathlib.Path(item) / value[len('@rpath/'):] for item in search_paths]
                      if value.startswith('@rpath/') else [pathlib.Path(expand(value))])
        if not any(candidate.exists() and (candidate.resolve() == app or app in candidate.resolve().parents) for candidate in candidates):
            raise SystemExit(f'Unbundled macOS dependency in {binary.relative_to(app)}: {value}')
PY
verify_source
# Resource/schema/license generation and all dependency checks are complete.
# Release packaging needs the app bundle and executable outputs, not compiler
# intermediates. Reclaim only exact owned paths before notarization copies.
[[ ${RUNNER_TEMP:-} == /* && ${GALAXY_JOB_ID:-} =~ ^[0-9]+-[0-9]+$ && ! -L "$CARGO_TARGET_DIR" ]] || fail 'Compiler cleanup requires an exact hosted runner build directory.'
if [[ -n ${GALAXY_BUILD_CACHE_ROOT:-} ]]; then
  [[ $GALAXY_BUILD_CACHE_ROOT == "$RUNNER_TEMP/galaxy-ci-cache-v1" &&
     ${GALAXY_CACHED_TARGET_DIR:-} == "$RUNNER_TEMP/galaxy-build" &&
     $CARGO_TARGET_DIR == "$GALAXY_CACHED_TARGET_DIR" && ! -L "$GALAXY_BUILD_CACHE_ROOT" ]] || fail 'Compiler cleanup requires the validated stable cache target.'
else
  [[ $CARGO_TARGET_DIR == "$RUNNER_TEMP/galaxy-build-$GALAXY_JOB_ID" ]] || fail 'Compiler cleanup requires this exact job-owned build directory.'
fi
for base in "$CARGO_TARGET_DIR/release" "$CARGO_TARGET_DIR/$target/release"; do
  [[ ! -L "$CARGO_TARGET_DIR/$target" && ! -L "$base" ]] || continue
  for kind in deps build .fingerprint incremental; do
    candidate="$base/$kind"
    [[ -d "$candidate" && ! -L "$candidate" ]] || continue
    rm -rf -- "$candidate"
  done
done
echo 'Unsigned Galaxy app, DockTile plug-in, resources, and Wormhole archives verified; ready for signing.'
