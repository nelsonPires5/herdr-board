# Build the `board` binary in release mode on Windows. Idempotent: cargo is a
# no-op when nothing changed. This is the plugin's Windows [[build]] step (herdr
# runs it at install time) and the PowerShell twin of scripts/build.sh.
#
# Output: target\release\board.exe (relative to the repo root).
#
# Kept compatible with Windows PowerShell 5.1 (always present on Windows), and
# ASCII-only because 5.1 reads BOM-less scripts in the ANSI code page.
[CmdletBinding()]
param()

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

# Run a native command without letting its stderr become a terminating error:
# Windows PowerShell 5.1 wraps native stderr in error records when its own
# output is redirected (as it is under herdr's build runner). Callers check
# $LASTEXITCODE instead.
function Invoke-Native([scriptblock]$Block) {
    $ErrorActionPreference = 'Continue'
    try {
        & $Block
    } catch {
        # A best-effort call failed to start; the caller checks $LASTEXITCODE.
        $global:LASTEXITCODE = 1
    }
}

# Resolve the repo root from this script's location so the build works no
# matter what cwd herdr / the caller invokes it from.
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $repoRoot

# Stop a running boardd before rebuilding. On Windows this matters more than on
# Unix: a running board.exe cannot be overwritten at all. Best-effort, exactly
# like build.sh: an old `board` without `--stop` errors and we just continue.
if (Get-Command board -CommandType Application -ErrorAction SilentlyContinue) {
    Invoke-Native { & board daemon --stop *> $null }
}

# Prefer a user-local cargo if PATH doesn't already have one.
if (-not (Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue)) {
    if ($env:USERPROFILE) {
        $env:PATH = (Join-Path $env:USERPROFILE '.cargo\bin') + [System.IO.Path]::PathSeparator + $env:PATH
    }
}
if (-not (Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue)) {
    [Console]::Error.WriteLine('build.ps1: cargo not found (install the Rust toolchain: https://rustup.rs)')
    exit 1
}

Write-Output "build.ps1: cargo build --release -p board-cli (repo: $repoRoot)"
# Fold cargo's stderr (progress) into plain output lines.
Invoke-Native { & cargo build --release -p board-cli 2>&1 | ForEach-Object { "$_" } }
if ($LASTEXITCODE -ne 0) {
    [Console]::Error.WriteLine("build.ps1: cargo build failed with exit code $LASTEXITCODE")
    exit $LASTEXITCODE
}

if ($env:CARGO_TARGET_DIR) {
    $targetDir = $env:CARGO_TARGET_DIR
} else {
    $targetDir = Join-Path $repoRoot 'target'
}
$bin = Join-Path (Join-Path $targetDir 'release') 'board.exe'
if (-not (Test-Path -LiteralPath $bin -PathType Leaf)) {
    [Console]::Error.WriteLine("build.ps1: expected binary not found at $bin")
    exit 1
}
Write-Output "build.ps1: ok -> $bin"
exit 0
