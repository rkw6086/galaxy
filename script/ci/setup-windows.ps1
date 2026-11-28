# Install unsigned, pinned build tools on the hosted native Windows x64 runner.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'Use a native Windows x64 runner.'
}
if (-not $env:RUNNER_TEMP -or -not [IO.Path]::IsPathRooted($env:RUNNER_TEMP) -or $env:GALAXY_JOB_ID -notmatch '^\d+-\d+$') {
    throw 'Missing job-specific Actions temporary directory or identity.'
}
if (-not $env:GITHUB_ENV -or -not $env:GITHUB_PATH) { throw 'Missing Actions environment output files.' }

function Confirm-CacheDirectory([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($null -ne $item) {
        if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Build cache directories must be real directories, never links or junctions.'
        }
    }
}
function Confirm-CacheFile([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($null -ne $item) {
        if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Cached tools must be regular files, never links or junctions.'
        }
    }
}
function Test-ToolVersion([string]$Path, [string]$Version) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $false }
    Confirm-CacheFile $Path
    try {
        $actual = (& $Path --version 2>$null) -join "`n"
        return $LASTEXITCODE -eq 0 -and $actual -ceq $Version
    } catch {
        return $false
    }
}
$cachedTools = $null
$buildTarget = Join-Path $env:RUNNER_TEMP "galaxy-build-$env:GALAXY_JOB_ID"
if ($env:GALAXY_BUILD_CACHE_ROOT) {
    $expectedRoot = Join-Path $env:RUNNER_TEMP 'galaxy-ci-cache-v1'
    $expectedTools = Join-Path $expectedRoot 'tools'
    $expectedTarget = Join-Path $env:RUNNER_TEMP 'galaxy-build'
    $expectedCargoHome = Join-Path "$env:SystemDrive\" 'gca'
    if ($env:GALAXY_BUILD_CACHE_ROOT -cne $expectedRoot -or
        $env:GALAXY_CACHED_TOOLS_DIR -cne $expectedTools -or
        $env:GALAXY_CACHED_TARGET_DIR -cne $expectedTarget -or
        $env:CARGO_HOME -cne $expectedCargoHome) {
        throw 'Build cache paths must match the hosted runner cache directories.'
    }
    foreach ($directory in @($expectedRoot, $expectedTools, (Join-Path $expectedTools 'bin'), $expectedTarget, $expectedCargoHome)) {
        Confirm-CacheDirectory $directory
    }
    $cachedTools = $expectedTools
    $buildTarget = $expectedTarget
    New-Item -ItemType Directory (Join-Path $cachedTools 'bin') -Force | Out-Null
}
# Keep Cargo's deeply nested dependency checkouts below Windows path limits.
# Stable paths permit compiler-cache reuse on disposable GitHub-hosted VMs.
if ($cachedTools) {
    $cargoHome = $expectedCargoHome
    New-Item -ItemType Directory $cargoHome -Force | Out-Null
} else {
    $cargoHome = Join-Path "$env:SystemDrive\" "gca-$env:GALAXY_JOB_ID"
    if (Test-Path $cargoHome) { throw 'Use a fresh job Cargo home.' }
    New-Item -ItemType Directory $cargoHome | Out-Null
}
$rustupBin = Split-Path -Parent (Get-Command rustup -CommandType Application).Source
$env:PATH = "$rustupBin;$env:PATH"
$env:CARGO_HOME = $cargoHome
& git config --global core.longpaths true
if ($LASTEXITCODE -ne 0) { throw 'Could not enable Windows Git long paths.' }
$env:RUSTUP_TOOLCHAIN = '1.97.1'
$env:CARGO_BUILD_JOBS = '2'
& rustup toolchain install 1.97.1 --profile minimal --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Could not install the pinned Rust toolchain.' }

$tools = Join-Path $env:RUNNER_TEMP "galaxy-tools-$env:GALAXY_JOB_ID"
if (Test-Path $tools) { throw 'Use a fresh job tools directory.' }
New-Item -ItemType Directory $tools | Out-Null
New-Item -ItemType Directory (Join-Path $tools 'bin') | Out-Null
$env:PATH = "$(Join-Path $tools 'bin');$env:PATH"
# A native wrapper avoids PowerShell/cmd command-line limits and preserves
# Cargo's @argfile when sccache would otherwise expand it past Windows' limit.
# Build this std-only tool from the checked-out source, never from the cache.
$rustcWrapper = Join-Path $tools 'bin\galaxy-rustc-wrapper.exe'
& rustc --edition=2021 --crate-name galaxy_windows_rustc_wrapper --crate-type bin -C opt-level=1 -C strip=symbols `
    (Join-Path $PSScriptRoot 'windows-rustc-wrapper.rs') -o $rustcWrapper
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $rustcWrapper -PathType Leaf)) {
    throw 'Could not build the Windows response-file compiler wrapper.'
}
# The existing interactive bootstrap is intentionally not used for hosted CI.
# protoc 25.1 supplies proto3 optional fields and its well-known schema SDK.
$protoc = Join-Path $tools 'protoc'
$protoNames = @('descriptor', 'any', 'api', 'duration', 'empty', 'field_mask', 'source_context', 'struct', 'timestamp', 'type', 'wrappers')
function Test-Protoc([string]$Directory) {
    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) { return $false }
    Confirm-CacheDirectory $Directory
    # Check every restored component before copying the schema SDK recursively.
    foreach ($entry in Get-ChildItem -LiteralPath $Directory -Recurse -Force) {
        if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Cached Protobuf SDK must not contain links or junctions.' }
    }
    if (-not (Test-ToolVersion (Join-Path $Directory 'bin\protoc.exe') 'libprotoc 25.1')) { return $false }
    foreach ($name in $protoNames) {
        if (-not (Test-Path (Join-Path $Directory "include\google\protobuf\$name.proto") -PathType Leaf)) { return $false }
    }
    return $true
}
function Copy-Protoc([string]$Source, [string]$Destination) {
    New-Item -ItemType Directory (Join-Path $Destination 'bin') -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $Source 'bin\protoc.exe') -Destination (Join-Path $Destination 'bin\protoc.exe') -Force
    $include = Join-Path $Source 'include'
    foreach ($schema in Get-ChildItem -LiteralPath $include -Recurse -File -Filter '*.proto') {
        $relative = [IO.Path]::GetRelativePath($include, $schema.FullName)
        $destinationSchema = Join-Path (Join-Path $Destination 'include') $relative
        New-Item -ItemType Directory (Split-Path -Parent $destinationSchema) -Force | Out-Null
        Copy-Item -LiteralPath $schema.FullName -Destination $destinationSchema -Force
    }
}
$cachedProtoc = if ($cachedTools) { Join-Path $cachedTools 'protoc' } else { $null }
if ($cachedProtoc -and (Test-Protoc $cachedProtoc)) {
    Copy-Protoc $cachedProtoc $protoc
} else {
    $protocArchive = Join-Path $tools 'protoc-25.1-win64.zip'
    Invoke-WebRequest 'https://github.com/protocolbuffers/protobuf/releases/download/v25.1/protoc-25.1-win64.zip' -OutFile $protocArchive
    Expand-Archive $protocArchive $protoc
    Remove-Item $protocArchive
}
if (-not (Test-Protoc $protoc)) { throw 'Expected protoc 25.1 with its complete schema SDK.' }
$env:PROTOC = Join-Path $protoc 'bin\protoc.exe'
$env:PROTOC_INCLUDE = Join-Path $protoc 'include'
if ($cachedProtoc) {
    # Only the compiler and .proto schemas enter the tools cache.
    Confirm-CacheDirectory $cachedProtoc
    if (Test-Path -LiteralPath $cachedProtoc) {
        foreach ($entry in Get-ChildItem -LiteralPath $cachedProtoc -Recurse -Force) {
            if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Cached Protobuf SDK must not contain links or junctions.' }
        }
        Remove-Item -LiteralPath $cachedProtoc -Recurse -Force
    }
    Copy-Protoc $protoc $cachedProtoc
}
& $env:PROTOC --version
if ($LASTEXITCODE -ne 0) { throw 'Could not invoke the pinned Protobuf compiler.' }
$env:CARGO_TARGET_DIR = Join-Path $tools 'build'
$licenseTool = Join-Path $tools 'bin\cargo-about.exe'
$cachedLicenseTool = if ($cachedTools) { Join-Path $cachedTools 'bin\cargo-about.exe' } else { $null }
if ($cachedLicenseTool -and (Test-ToolVersion $cachedLicenseTool 'cargo-about 0.8.4')) {
    Copy-Item -LiteralPath $cachedLicenseTool -Destination $licenseTool
} else {
    # Tool installers have their own binary cache, separate from app compilation.
    $compilerWrapper = $env:RUSTC_WRAPPER
    try {
        $env:RUSTC_WRAPPER = ''
        & cargo '+1.97.1' install --locked --root $tools cargo-about@0.8.4
        if ($LASTEXITCODE -ne 0) { throw 'Could not install the pinned license tool.' }
    } finally {
        $env:RUSTC_WRAPPER = $compilerWrapper
    }
}
if (-not (Test-ToolVersion $licenseTool 'cargo-about 0.8.4')) { throw 'Expected cargo-about 0.8.4.' }
if ($cachedLicenseTool) {
    Confirm-CacheFile $cachedLicenseTool
    Copy-Item -LiteralPath $licenseTool -Destination $cachedLicenseTool -Force
}
if (Test-Path -LiteralPath $env:CARGO_TARGET_DIR) {
    Confirm-CacheDirectory $env:CARGO_TARGET_DIR
    Remove-Item -LiteralPath $env:CARGO_TARGET_DIR -Recurse -Force
}
$isccDirectory = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6'
if (-not (Test-Path (Join-Path $isccDirectory 'ISCC.exe'))) { throw 'The hosted runner must provide Inno Setup 6.' }
$clangDirectory = Join-Path $env:ProgramFiles 'LLVM\bin'
if (-not (Test-Path (Join-Path $clangDirectory 'libclang.dll'))) { throw 'The hosted runner must provide LLVM and libclang.' }
foreach ($directory in @($rustupBin, (Join-Path $tools 'bin'), (Join-Path $protoc 'bin'), $isccDirectory)) {
    Add-Content $env:GITHUB_PATH $directory -Encoding utf8
}
@(
    'RUSTUP_TOOLCHAIN=1.97.1'
    'CARGO_BUILD_JOBS=2'
    'CARGO_INCREMENTAL=0'
    'CARGO_PROFILE_RELEASE_DEBUG=0'
    "CARGO_HOME=$cargoHome"
    "LIBCLANG_PATH=$clangDirectory"
    "PROTOC=$env:PROTOC"
    "PROTOC_INCLUDE=$env:PROTOC_INCLUDE"
    "GALAXY_TOOLS_DIR=$tools"
    "GALAXY_WINDOWS_RUSTC_WRAPPER=$rustcWrapper"
    "CARGO_TARGET_DIR=$buildTarget"
    "GALAXY_RELEASE_DIR=$(Join-Path $env:RUNNER_TEMP "galaxy-artifacts-$env:GALAXY_JOB_ID")"
) | Add-Content $env:GITHUB_ENV -Encoding utf8
