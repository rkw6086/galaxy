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
# Keep Cargo's deeply nested dependency checkouts below Windows path limits.
# This directory is unique to one job on a disposable GitHub-hosted VM.
$cargoHome = Join-Path "$env:SystemDrive\" "gca-$env:GALAXY_JOB_ID"
if (Test-Path $cargoHome) { throw 'Use a fresh job Cargo home.' }
New-Item -ItemType Directory $cargoHome | Out-Null
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
$env:CARGO_TARGET_DIR = Join-Path $tools 'build'
& cargo install --locked --root $tools cargo-about@0.8.4
if ($LASTEXITCODE -ne 0) { throw 'Could not install the pinned license tool.' }
Remove-Item $env:CARGO_TARGET_DIR -Recurse -Force

# The existing interactive bootstrap is intentionally not used for hosted CI.
# protoc 25.1 supports proto3 optional fields and matches Linux's build tooling.
$protocArchive = Join-Path $tools 'protoc-25.1-win64.zip'
Invoke-WebRequest 'https://github.com/protocolbuffers/protobuf/releases/download/v25.1/protoc-25.1-win64.zip' -OutFile $protocArchive
$protoc = Join-Path $tools 'protoc'
Expand-Archive $protocArchive $protoc
Remove-Item $protocArchive
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
    "GALAXY_TOOLS_DIR=$tools"
    "CARGO_TARGET_DIR=$(Join-Path $env:RUNNER_TEMP "galaxy-build-$env:GALAXY_JOB_ID")"
    "GALAXY_RELEASE_DIR=$(Join-Path $env:RUNNER_TEMP "galaxy-artifacts-$env:GALAXY_JOB_ID")"
) | Add-Content $env:GITHUB_ENV -Encoding utf8
