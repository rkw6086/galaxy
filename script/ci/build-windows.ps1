# Build the native Windows installer without starting the desktop application.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
Set-Location $root
if (-not $IsWindows -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'Use a native Windows x64 runner.'
}
if (-not $env:CARGO_TARGET_DIR -or -not [IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR) -or
    -not $env:GALAXY_RELEASE_DIR -or -not [IO.Path]::IsPathRooted($env:GALAXY_RELEASE_DIR)) {
    throw 'Use absolute job-specific build and artifact directories.'
}
$version = $env:GALAXY_RELEASE_VERSION
$sha = $env:GALAXY_RELEASE_SHA
$channel = $env:GALAXY_BUILD_CHANNEL
if ($version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' -or $sha -notmatch '^[0-9a-f]{40}$') {
    throw 'A validated version and source commit are required.'
}
if ($channel -notin @('release', 'nightly', 'ci')) { throw 'Unknown build channel.' }
if ($channel -eq 'nightly') {
    if ($env:GITHUB_RUN_ID -notmatch '^\d+$' -or $env:GITHUB_RUN_ATTEMPT -notmatch '^[1-9][0-9]*$' -or
        $env:GALAXY_JOB_ID -cne "$env:GITHUB_RUN_ID-$env:GITHUB_RUN_ATTEMPT") {
        throw 'Nightly build needs its current workflow run and attempt.'
    }
    $nightly = [regex]::Match($env:GALAXY_NIGHTLY_ID, "^${sha}_([0-9]+)-([1-9][0-9]*)$")
    if (-not $nightly.Success -or $nightly.Groups[1].Value -cne $env:GITHUB_RUN_ID) {
        throw 'Nightly identity must match the validated source and workflow run.'
    }
    if ([bigint]::Parse($nightly.Groups[2].Value) -gt [bigint]::Parse($env:GITHUB_RUN_ATTEMPT)) {
        throw 'Nightly identity cannot come from a future workflow attempt.'
    }
}
if ((& rustc --version) -notlike 'rustc 1.97.1 *') { throw 'Use Rust 1.97.1.' }
if ((& git rev-parse HEAD) -ne $sha -or $LASTEXITCODE -ne 0) { throw 'Checkout differs from the validated source commit.' }
function Confirm-Source {
    if ($channel -eq 'release') {
        if ($env:GALAXY_VERSION_STAGED -ne '1') { throw 'Tagged releases require controlled version staging.' }
        & node script/release.mjs verify-staged $version $sha
    } else {
        & node script/release.mjs check $version
    }
    if ($LASTEXITCODE -ne 0) { throw 'Source version verification failed.' }
    if ($channel -ne 'release') {
        $status = & git status --porcelain
        if ($LASTEXITCODE -ne 0 -or $status) { throw 'Build requires an unchanged source checkout.' }
    }
}
Confirm-Source
& node script/ci/release-identity.mjs
if ($LASTEXITCODE -ne 0) { throw 'Prepared build identity verification failed.' }
$env:GIT_RELEASE_TAG = $version
$env:GALAXY_GIT_COMMIT = $sha
$env:GALAXY_GIT_DIRTY = 'false'
$env:GALAXY_BUILD_CHANNEL = if ($channel -eq 'ci') { 'release' } else { $channel }
if ($channel -eq 'ci') { $env:GALAXY_GITHUB_CI = '0' }
$env:GALAXY_APP_NAME = 'Galaxy'
$env:CARGO_FULL_PROFILE = 'release'
$env:CARGO_BUILD_JOBS = '2'
$env:CARGO_INCREMENTAL = '0'
$target = 'x86_64-pc-windows-msvc'
$features = 'autoupdate,release_bundle,gui,nld_classifier_v3,nld_heuristic_v2,galaxy_control_cli'
# app/build.rs stages native resource-compiler inputs here independently of
# CARGO_TARGET_DIR. This remains inside the disposable checkout's ignored target.
New-Item -ItemType Directory (Join-Path $root 'target\release') -Force | Out-Null
& cargo build --locked --release --target $target --bin galaxy-oss --bin generate_settings_schema --features $features
if ($LASTEXITCODE -ne 0) { throw 'Windows release compilation failed.' }
$binaryDirectory = Join-Path $env:CARGO_TARGET_DIR "$target\release"
$binary = Join-Path $binaryDirectory 'galaxy-oss.exe'
$generator = Join-Path $binaryDirectory 'generate_settings_schema.exe'
if (-not (Test-Path $binary -PathType Leaf) -or -not (Test-Path $generator -PathType Leaf)) { throw 'Built executables are missing.' }
$schema = Join-Path $env:CARGO_TARGET_DIR 'settings_schema.json'
& $generator --channel oss $schema
if ($LASTEXITCODE -ne 0) { throw 'Could not generate the settings schema.' }
$env:SETTINGS_SCHEMA_CACHE = $schema
$env:SKIP_SETTINGS_SCHEMA = ''
$env:REQUIRE_WORMHOLE_HELPERS = '1'
$resources = Join-Path $binaryDirectory 'resources'
& (Join-Path $root 'script\windows\prepare_bundled_resources.ps1') -DestinationDir $resources -Channel oss -CargoProfile release
if (-not $?) { throw 'Could not prepare the installer resources.' }
$verification = @'
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const [root, destination] = process.argv.slice(2);
const { inspectWormholeHelpers, verifyWormholeHelpers } = await import(pathToFileURL(path.join(root, 'script/lib/bundled-wormhole-helpers.mjs')));
await verifyWormholeHelpers(await inspectWormholeHelpers(path.join(root, 'resources/wormhole-helpers')), destination);
'@
$verification | & node --input-type=module - $root (Join-Path $resources 'wormhole-helpers')
if ($LASTEXITCODE -ne 0) { throw 'Bundled Wormhole helpers did not match their sources.' }
if (Test-Path $env:GALAXY_RELEASE_DIR) { throw 'Use a fresh artifact directory.' }
New-Item -ItemType Directory $env:GALAXY_RELEASE_DIR | Out-Null
$stem = if ($channel -eq 'release') { "Galaxy_$version" } elseif ($channel -eq 'nightly') { "Galaxy_$env:GALAXY_ARTIFACT_IDENTITY" } else { "Galaxy_ci_$sha" }
$name = "${stem}_x86_64-setup"
$arguments = @(
    (Join-Path $root 'script\windows\windows-installer.iss')
    '/DReleaseChannel=oss'
    '/DMyAppExeName=galaxy-oss.exe'
    "/DTargetProfileDir=$binaryDirectory"
    '/DMyAppName=GalaxyOss'
    "/DMyAppVersion=$version"
    '/DArch=x64'
    "/DOutputName=$name"
    "/O$env:GALAXY_RELEASE_DIR"
)
# Windows signing has no configured certificate; the EXE is an unsigned native
# installer. Apple signing is handled only by the separate macOS package job.
& ISCC @arguments
if ($LASTEXITCODE -ne 0) { throw 'Could not build the Windows installer.' }
$installer = Join-Path $env:GALAXY_RELEASE_DIR "$name.exe"
if (-not (Test-Path $installer -PathType Leaf) -or (Get-Item $installer).Length -eq 0) { throw 'Windows installer is missing.' }
$digest = (Get-FileHash $installer -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText((Join-Path $env:GALAXY_RELEASE_DIR 'SHA256SUMS'), "$digest  $name.exe`n", [Text.UTF8Encoding]::new($false))
Confirm-Source
Write-Output "Unsigned Windows installer is ready in $env:GALAXY_RELEASE_DIR"
