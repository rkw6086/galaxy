#!/bin/bash
# Keep credentials out of tracing, process output, artifacts, and the checkout.
set +x
set -euo pipefail
umask 077

fail() { echo "Galaxy macOS package: $*" >&2; exit 1; }
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || fail 'Use the native macos-arm64 runner.'
channel=${GALAXY_BUILD_CHANNEL:-release}
version=${GALAXY_RELEASE_VERSION:-}
[[ $version =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || fail 'A validated stable package version is required.'
if [[ $channel == nightly ]]; then
  [[ ( ${GITHUB_EVENT_NAME:-} == schedule || ${GITHUB_EVENT_NAME:-} == workflow_dispatch ) && ${GITHUB_REF:-} == refs/heads/master ]] || fail 'Nightly signing requires a scheduled or manual master run.'
  [[ ${GALAXY_RELEASE_SHA:-} =~ ^[0-9a-f]{40}$ && ${GITHUB_RUN_ID:-} =~ ^[1-9][0-9]{0,18}$ && ${GITHUB_RUN_ATTEMPT:-} =~ ^[1-9][0-9]{0,8}$ ]] || fail 'Nightly source or run identity is invalid.'
  nightly_pattern="^${GALAXY_RELEASE_SHA}_${GITHUB_RUN_ID}-([1-9][0-9]{0,8})$"
  [[ ${GALAXY_NIGHTLY_ID:-} =~ $nightly_pattern ]] || fail 'Nightly identity differs from its prepared source and run.'
  (( ${BASH_REMATCH[1]} <= GITHUB_RUN_ATTEMPT )) || fail 'Nightly preparation attempt exceeds this run attempt.'
  [[ -z $(git status --porcelain) ]] || fail 'Nightly signing requires an unchanged source checkout.'
  node script/ci/release-identity.mjs
  artifact_version="$GALAXY_ARTIFACT_IDENTITY"
  volume_name='Galaxy Nightly'
elif [[ $channel == beta ]]; then
  [[ ( ${GITHUB_EVENT_NAME:-} == schedule || ${GITHUB_EVENT_NAME:-} == workflow_dispatch ) && ${GITHUB_REF:-} == refs/heads/master ]] || fail 'Beta signing requires a scheduled or manual master run.'
  [[ ${GALAXY_RELEASE_SHA:-} =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ && ${GALAXY_BETA_ID:-} == "$GALAXY_RELEASE_SHA" ]] || fail 'Beta identity must match the validated master commit.'
  [[ -z $(git status --porcelain) ]] || fail 'Beta signing requires an unchanged source checkout.'
  artifact_version="beta_$GALAXY_BETA_ID"
  volume_name='Galaxy Beta'
elif [[ $channel == release ]]; then
  [[ ${GITHUB_EVENT_NAME:-} == push && ${GITHUB_REF:-} == "refs/tags/$version" ]] || fail 'Signing requires a matching pushed version tag.'
  [[ ${GALAXY_VERSION_STAGED:-} == 1 ]] || fail 'Tagged releases require controlled CI version staging.'
  node script/release.mjs verify-staged "$version" "$GALAXY_RELEASE_SHA"
  artifact_version=$version
  volume_name=Galaxy
else
  fail 'Build channel must be release or nightly.'
fi
[[ $(git rev-parse HEAD) == "${GALAXY_RELEASE_SHA:-}" ]] || fail 'Checkout no longer matches the validated source commit.'
for variable in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_TEAM_ID APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_P8 CARGO_TARGET_DIR GALAXY_RELEASE_DIR; do
  [[ -n ${!variable:-} ]] || fail "Missing signing secret or job variable: $variable"
done
[[ $APPLE_SIGNING_IDENTITY == 'Developer ID Application: '* ]] || fail 'Use a Developer ID Application signing identity.'
[[ $APPLE_TEAM_ID =~ ^[A-Z0-9]{10}$ ]] || fail 'Invalid Apple Team ID.'
[[ $APPLE_SIGNING_IDENTITY == *"($APPLE_TEAM_ID)" ]] || fail 'Signing identity and Team ID do not match.'
[[ $APPLE_API_KEY =~ ^[A-Za-z0-9]+$ ]] || fail 'Invalid notarization Key ID.'
[[ $APPLE_API_ISSUER =~ ^[a-fA-F0-9-]{36}$ ]] || fail 'Invalid notarization Issuer ID.'
[[ $CARGO_TARGET_DIR == /* && $GALAXY_RELEASE_DIR == /* ]] || fail 'Use absolute job-owned build and artifact directories.'
[[ -e "$GALAXY_RELEASE_DIR" || -L "$GALAXY_RELEASE_DIR" ]] && fail 'Artifact path already exists; use a fresh job-owned path.'

app="$CARGO_TARGET_DIR/aarch64-apple-darwin/release/bundle/osx/Galaxy.app"
[[ -x "$app/Contents/MacOS/galaxy-oss" && -x "$app/Contents/PlugIns/GalaxyDockTilePlugin.docktileplugin/Contents/MacOS/GalaxyDockTilePlugin" ]] || fail 'Build the complete app first.'
bundle_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")
bundle_channel=$(/usr/libexec/PlistBuddy -c 'Print :GalaxyBuildChannel' "$app/Contents/Info.plist")
bundle_commit=$(/usr/libexec/PlistBuddy -c 'Print :GalaxySourceCommit' "$app/Contents/Info.plist")
[[ $bundle_version == "$version" && $bundle_channel == "$channel" && $bundle_commit == "$GALAXY_RELEASE_SHA" ]] || fail 'App bundle version, channel, or source commit does not match the validated build.'
# macOS's basic certificate-chain lookup must discover this keychain too.
# Register it in the standard location before importing its certificates.
work=$(mktemp -d "$HOME/Library/Keychains/galaxy-sign.XXXXXX")
keychain="$work/signing.keychain-db"
keychain_password=$(/usr/bin/openssl rand -hex 32)
original_keychains=()
original_user_keychains=()
keychain_created=false
keychain_list_changed=false
artifact_complete=false
cleanup() {
  local status=$?
  trap - EXIT
  if [[ $keychain_list_changed == true ]]; then
    /usr/bin/security list-keychains -s "${original_keychains[@]}" >/dev/null 2>&1 || { echo 'Could not restore the runner keychain list.' >&2; status=1; }
    /usr/bin/security list-keychains -d user -s "${original_user_keychains[@]}" >/dev/null 2>&1 || { echo 'Could not restore the runner user keychain preferences.' >&2; status=1; }
  fi
  if [[ $keychain_created == true ]]; then
    /usr/bin/security delete-keychain "$keychain" >/dev/null 2>&1 || { echo 'Could not remove the temporary signing keychain.' >&2; status=1; }
  fi
  rm -rf -- "$work"
  if [[ $artifact_complete != true ]]; then rm -rf -- "$GALAXY_RELEASE_DIR"; fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# A failed/empty listing must never replace the user's search list with an empty one.
# Use the effective search list used by codesign, not just the saved user domain.
/usr/bin/security list-keychains > "$work/keychains.txt"
/usr/bin/security list-keychains -d user > "$work/user-keychains.txt"
while IFS= read -r entry; do
  entry=${entry#*\"}; entry=${entry%\"*}
  [[ -n $entry ]] && original_keychains+=("$entry")
done < "$work/keychains.txt"
[[ ${#original_keychains[@]} -gt 0 ]] || fail 'The runner has no keychain search list to preserve.'
while IFS= read -r entry; do
  entry=${entry#*\"}; entry=${entry%\"*}
  [[ -n $entry ]] && original_user_keychains+=("$entry")
done < "$work/user-keychains.txt"
[[ ${#original_user_keychains[@]} -gt 0 ]] || fail 'The runner has no user keychain preferences to preserve.'

mkdir -p "$GALAXY_RELEASE_DIR"
python3 - "$work" <<'PY'
import base64, os, pathlib, sys
root = pathlib.Path(sys.argv[1])
for variable, filename in [('APPLE_CERTIFICATE', 'certificate.p12'), ('APPLE_API_KEY_P8', 'notary.p8')]:
    try:
        value = base64.b64decode(''.join(os.environ[variable].split()), validate=True)
    except Exception:
        raise SystemExit(f'{variable} must contain base64 file data') from None
    if not value:
        raise SystemExit(f'{variable} decoded to an empty file')
    (root / filename).write_bytes(value)
PY
keychain_list_changed=true
/usr/bin/security create-keychain -p "$keychain_password" "$keychain" >/dev/null
keychain_created=true
/usr/bin/security set-keychain-settings -lut 21600 "$keychain"
/usr/bin/security unlock-keychain -p "$keychain_password" "$keychain"
/usr/bin/security list-keychains -s "$keychain" "${original_keychains[@]}"
# PKCS#12 identity import does not reliably install its additional CA certificates.
# Add Apple's public intermediate explicitly; retain the Mac's system trust roots.
/usr/bin/openssl x509 -in script/ci/certificates/DeveloperIDG2CA.pem -outform DER -out "$work/DeveloperIDG2CA.cer"
/usr/bin/security add-certificates -k "$keychain" "$work/DeveloperIDG2CA.cer"
/usr/bin/security import "$work/certificate.p12" -k "$keychain" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign >/dev/null
/usr/bin/security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$keychain_password" "$keychain" >/dev/null
/usr/bin/security find-identity -v -p codesigning "$keychain" > "$work/identities.txt"
grep -F -- "$APPLE_SIGNING_IDENTITY" "$work/identities.txt" >/dev/null || fail 'Certificate with private key is not a valid signing identity.'
/usr/bin/security find-certificate -c "$APPLE_SIGNING_IDENTITY" -p "$keychain" > "$work/signing-cert.pem"
# Do not pass -k: this must validate using the same effective list as codesign.
/usr/bin/security verify-cert -p basic -L -c "$work/signing-cert.pem" || fail 'The Developer ID certificate chain is not trusted by this Mac.'
/usr/bin/security verify-cert -p codeSign -L -c "$work/signing-cert.pem" || fail 'The Developer ID certificate chain is not trusted by this Mac.'
rm -f "$work/certificate.p12"

# Only Apple tooling below needs credentials. No npm/cargo/build hooks run here.
unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_API_KEY_P8
notary_args=(--key "$work/notary.p8" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER")
notarize() {
  local artifact=$1 label=$2 result="$work/notary-$2.json" submission
  # Inspect Apple's status even if the command returns a failure exit code.
  xcrun notarytool submit "$artifact" "${notary_args[@]}" --wait --timeout 90m --output-format json > "$result" || true
  submission=$(python3 - "$result" <<'PY'
import json, sys
try: print(json.load(open(sys.argv[1])).get('id', ''))
except Exception: pass
PY
)
  if [[ -n $submission ]]; then
    xcrun notarytool log "$submission" "${notary_args[@]}" "$GALAXY_RELEASE_DIR/notary-$label.json" >/dev/null 2>&1 || true
  fi
  python3 - "$result" <<'PY'
import json, sys
try: result = json.load(open(sys.argv[1]))
except Exception: raise SystemExit('Apple notarization returned no valid result; check this step for errors.') from None
print('Apple notarization:', result.get('status', 'Unknown'), 'submission:', result.get('id', 'Unknown'))
if result.get('status') != 'Accepted': raise SystemExit('Notarization did not succeed; no distributable artifact will be uploaded.')
PY
}

# Keep Galaxy's hardware/automation permissions, without requesting the upstream
# Warp team's private app group. The unsandboxed app already has a per-user path
# fallback when that optional shared container is unavailable.
python3 - script/Entitlements.plist "$work/entitlements.plist" "$work/node-entitlements.plist" <<'PY'
import plistlib, sys
with open(sys.argv[1], 'rb') as stream:
    entitlements = plistlib.load(stream)
entitlements.pop('com.apple.security.application-groups', None)
with open(sys.argv[2], 'wb') as stream:
    plistlib.dump(entitlements, stream)
entitlements['com.apple.security.cs.allow-jit'] = True
with open(sys.argv[3], 'wb') as stream:
    plistlib.dump(entitlements, stream)
PY

# Sign every nested Mach-O helper/library, then its enclosing bundles, before
# sealing Galaxy.app. Linux Wormhole .tar.gz files are opaque data resources.
# --deep is reserved for verification, never used to choose signing order.
while IFS= read -r -d '' executable; do
  # codesign recognizes this executable as Galaxy.app and validates its nested
  # code. Defer it until the DockTile plug-in and other helpers are signed; the
  # final app signature below also signs the main executable with entitlements.
  [[ $executable != "$app/Contents/MacOS/galaxy-oss" ]] || continue
  kind=$(/usr/bin/file -b "$executable")
  [[ $kind == *Mach-O* ]] || continue
  signing_args=(--force --timestamp --options runtime --keychain "$keychain" --sign "$APPLE_SIGNING_IDENTITY")
  if [[ $kind == *executable* ]]; then
    entitlements="$work/entitlements.plist"
    [[ $(basename "$executable") == node ]] && entitlements="$work/node-entitlements.plist"
    signing_args+=(--entitlements "$entitlements")
  fi
  /usr/bin/codesign "${signing_args[@]}" "$executable"
done < <(find "$app" -type f -print0)
while IFS= read -r -d '' bundle; do
  [[ $bundle != "$app" ]] || continue
  case "$bundle" in
    *.framework|*.app|*.xpc|*.appex|*.bundle|*.docktileplugin)
      /usr/bin/codesign --force --timestamp --options runtime --keychain "$keychain" --sign "$APPLE_SIGNING_IDENTITY" "$bundle"
      ;;
  esac
done < <(find "$app" -depth -type d -print0)
/usr/bin/codesign --force --timestamp --options runtime --keychain "$keychain" --sign "$APPLE_SIGNING_IDENTITY" --entitlements "$work/entitlements.plist" "$app"
/usr/bin/codesign --verify --deep --strict --verbose=2 "$app"
/usr/bin/codesign -dv --verbose=4 "$app" 2> "$work/signature.txt"
grep -F "TeamIdentifier=$APPLE_TEAM_ID" "$work/signature.txt" >/dev/null || fail 'Unexpected signing team.'
grep -F 'runtime' "$work/signature.txt" >/dev/null || fail 'Hardened runtime is missing.'

ditto -c -k --keepParent "$app" "$work/Galaxy-notarization.zip"
notarize "$work/Galaxy-notarization.zip" app
xcrun stapler staple "$app"
xcrun stapler validate "$app"
/usr/sbin/spctl --assess --type execute --verbose=2 "$app"

stage="$work/dmg-root"
mkdir "$stage"
ditto "$app" "$stage/Galaxy.app"
ln -s /Applications "$stage/Applications"
dmg="$GALAXY_RELEASE_DIR/Galaxy_${artifact_version}_aarch64.dmg"
hdiutil create -volname "$volume_name" -srcfolder "$stage" -format UDZO -ov "$dmg" >/dev/null
/usr/bin/codesign --force --timestamp --keychain "$keychain" --sign "$APPLE_SIGNING_IDENTITY" --identifier com.galaxy.GalaxyOss.dmg "$dmg"
/usr/bin/codesign --verify --strict --verbose=2 "$dmg"
notarize "$dmg" dmg
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
/usr/sbin/spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
hdiutil verify "$dmg" >/dev/null
(cd "$GALAXY_RELEASE_DIR" && shasum -a 256 "$(basename "$dmg")" > SHA256SUMS)
artifact_complete=true
echo "Verified signed and notarized DMG: $(basename "$dmg")"
