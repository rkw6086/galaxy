# Compile the real installer with disposable payloads before the long Rust build.
# The resulting fixture installer is never executed, published, or retained.
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string[]]$InstallerDefines
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'Installer preflight requires the native Windows x64 runner.'
}
if (-not $env:RUNNER_TEMP -or -not [IO.Path]::IsPathRooted($env:RUNNER_TEMP) -or
    $env:GALAXY_JOB_ID -notmatch '^\d+-\d+$') {
    throw 'Installer preflight requires a job-specific Actions temporary directory and identity.'
}

# Share these definitions with final packaging, while keeping filesystem and
# signing overrides out of the fixture compile.
$definitionPatterns = @{
    ReleaseChannel = '^oss$'
    MyAppExeName = '^galaxy-oss\.exe$'
    MyAppName = '^GalaxyOss$'
    MyAppVersion = '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
    Arch = '^x64$'
    OutputName = '^Galaxy_[A-Za-z0-9_.-]+_x86_64-setup$'
}
$definitions = @{}
foreach ($definition in $InstallerDefines) {
    $match = [regex]::Match($definition, '^/D([A-Za-z][A-Za-z0-9_]*)=(.+)$')
    if (-not $match.Success) { throw 'Invalid installer preflight definition.' }
    $key = $match.Groups[1].Value
    $value = $match.Groups[2].Value
    if (-not $definitionPatterns.ContainsKey($key) -or $definitions.ContainsKey($key) -or
        $value -cnotmatch $definitionPatterns[$key]) {
        throw 'Installer preflight definitions must match unsigned Galaxy x64 packaging.'
    }
    $definitions[$key] = $value
}
if ($definitions.Count -ne $definitionPatterns.Count) {
    throw 'Installer preflight is missing a packaging definition.'
}

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
# PATH can expose both the actual compiler and a Chocolatey shim. Match native
# command invocation by selecting its first application, never an array of paths.
$compilerCommand = Get-Command ISCC -CommandType Application | Select-Object -First 1
$compiler = $compilerCommand.Source
if ($compiler -isnot [string] -or -not [IO.Path]::IsPathRooted($compiler) -or
    -not (Test-Path -LiteralPath $compiler -PathType Leaf)) {
    throw 'Installer preflight must resolve one existing native compiler application.'
}
$temporaryRoot = [IO.Path]::GetFullPath($env:RUNNER_TEMP)
$temporaryRootItem = Get-Item -LiteralPath $temporaryRoot -Force
if (-not $temporaryRootItem.PSIsContainer -or
    ($temporaryRootItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw 'Installer preflight requires a real temporary directory.'
}
$directory = Join-Path $temporaryRoot "galaxy-installer-preflight-$env:GALAXY_JOB_ID"
if ($null -ne (Get-Item -LiteralPath $directory -Force -ErrorAction SilentlyContinue)) {
    throw 'Installer preflight requires a fresh job-owned directory.'
}

$created = $false
try {
    New-Item -ItemType Directory $directory | Out-Null
    $created = $true
    $payload = Join-Path $directory 'payload'
    $resources = Join-Path $payload 'resources'
    $output = Join-Path $directory 'output'
    New-Item -ItemType Directory $resources -Force | Out-Null
    New-Item -ItemType Directory $output | Out-Null
    # ISCC is an installed, valid Windows PE. Copy it as data only: the actual
    # repository DLLs, bootstrap, icons and bitmaps are read by the real script.
    Copy-Item -LiteralPath $compiler -Destination (Join-Path $payload $definitions.MyAppExeName)
    [IO.File]::WriteAllText((Join-Path $resources 'installer-preflight.txt'),
        "Disposable Galaxy installer compile fixture.`n", [Text.UTF8Encoding]::new($false))

    $arguments = @((Join-Path $root 'script\windows\windows-installer.iss')) +
        $InstallerDefines + @("/DTargetProfileDir=$payload", "/O$output")
    $compilerLog = Join-Path $directory 'compiler.log'
    Write-Host 'Checking the actual Windows installer before Rust compilation.'
    & $compiler @arguments 2>&1 | Tee-Object -FilePath $compilerLog
    $compilerExitCode = $LASTEXITCODE
    if ($compilerExitCode -ne 0) {
        throw "Windows installer preflight failed (exit $compilerExitCode), before Rust compilation."
    }
    if (Select-String -LiteralPath $compilerLog -Pattern '^\s*Warning:' -Quiet) {
        throw 'Windows installer preflight reported warnings, before Rust compilation.'
    }
    $installer = Join-Path $output "$($definitions.OutputName).exe"
    if (-not (Test-Path -LiteralPath $installer -PathType Leaf) -or
        (Get-Item -LiteralPath $installer).Length -eq 0) {
        throw 'Windows installer preflight produced no installer.'
    }
} finally {
    if ($created) {
        if ($directory -cne (Join-Path $temporaryRoot "galaxy-installer-preflight-$env:GALAXY_JOB_ID")) {
            throw 'Refusing cleanup outside the job-owned installer preflight directory.'
        }
        $item = Get-Item -LiteralPath $directory -Force -ErrorAction SilentlyContinue
        if ($null -ne $item) {
            if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw 'Refusing to clean an installer preflight link or unexpected file.'
            }
            foreach ($entry in Get-ChildItem -LiteralPath $directory -Recurse -Force) {
                if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                    throw 'Refusing to clean installer preflight links or junctions.'
                }
            }
            Remove-Item -LiteralPath $directory -Recurse -Force
        }
    }
}
Write-Host 'Windows installer preflight passed; disposable installer files removed.'
