# Copy the plugin-built CLI out of Herdr's managed checkout and into a user bin
# directory. The PowerShell twin of scripts/install-cli.sh for Windows: it keeps
# the same ownership contract (a checksum marker is the only permission to
# replace an existing command) and installs board.exe to
# %USERPROFILE%\.local\bin unless HERDR_BOARD_CLI_INSTALL_DIR overrides it.
#
# Kept compatible with Windows PowerShell 5.1 (always present on Windows), and
# ASCII-only because 5.1 reads BOM-less scripts in the ANSI code page.
Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

function Fail([int]$Code, [string]$Message) {
    [Console]::Error.WriteLine("install-cli.ps1: $Message")
    exit $Code
}

if ($args.Count -ne 0) {
    Fail 2 'no arguments expected; set HERDR_BOARD_CLI_INSTALL_DIR to override the destination directory'
}

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$sourceBin = Join-Path $repoRoot 'target\release\board.exe'

# Note: Windows PowerShell 5.1 cannot observe a set-but-empty variable (Windows
# treats it as unset), so there an empty override falls back to the default.
if (Test-Path Env:HERDR_BOARD_CLI_INSTALL_DIR) {
    if ([string]::IsNullOrEmpty($env:HERDR_BOARD_CLI_INSTALL_DIR)) {
        Fail 2 'HERDR_BOARD_CLI_INSTALL_DIR must not be empty'
    }
    $installDir = $env:HERDR_BOARD_CLI_INSTALL_DIR
} else {
    if ([string]::IsNullOrEmpty($env:USERPROFILE)) {
        Fail 2 'USERPROFILE must be set when HERDR_BOARD_CLI_INSTALL_DIR is not provided'
    }
    $installDir = Join-Path $env:USERPROFILE '.local\bin'
}

# Fully qualified only: a drive-letter path (C:\...) or a UNC path (\\server\...).
# Drive-relative (C:foo) and root-relative (\foo) paths depend on process state.
if ($installDir -notmatch '^(?:[A-Za-z]:[\\/]|\\\\[^\\/])') {
    Fail 2 "install directory must be an absolute path: $installDir"
}

if (-not (Test-Path -LiteralPath $sourceBin -PathType Leaf)) {
    Fail 1 "expected executable source binary at $sourceBin; run scripts/build.ps1 first"
}

# Lowercase hex SHA-256 of a file, via .NET directly: Get-FileHash lives in a
# module that does not always load (e.g. a 5.1 child of PowerShell 7 inherits a
# PSModulePath that hides it).
function Get-Sha256Hex([string]$Path) {
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        try {
            $bytes = $sha.ComputeHash($stream)
        } finally {
            $sha.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
    return (-join ($bytes | ForEach-Object { $_.ToString('x2') }))
}

function Get-Sha256([string]$Path) {
    try {
        $checksum = Get-Sha256Hex $Path
    } catch {
        Fail 1 "failed to calculate SHA-256 checksum for ${Path}: $($_.Exception.Message)"
    }
    if ($checksum -cnotmatch '^[0-9a-f]{64}$') {
        Fail 1 "SHA-256 returned an invalid checksum for $Path"
    }
    return $checksum
}

# A path counts as present when anything (including a dangling link) sits there.
function Get-Entry([string]$Path) {
    return Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
}

# Regular file, not a directory, not a symlink/junction/other reparse point.
function Test-RegularFile([string]$Path) {
    $item = Get-Entry $Path
    if ($null -eq $item -or $item.PSIsContainer) {
        return $false
    }
    return -not ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)
}

$utf8NoBom = New-Object System.Text.UTF8Encoding $false

function Read-Marker([string]$Path) {
    return [System.IO.File]::ReadAllText($Path).TrimEnd("`r", "`n")
}

$installEntry = Get-Entry $installDir
if ($null -ne $installEntry -and -not $installEntry.PSIsContainer) {
    Fail 1 "install directory exists but is not a directory: $installDir"
}
if ($null -eq $installEntry) {
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
}

$destination = Join-Path $installDir 'board.exe'
$marker = Join-Path $installDir '.herdr-board-cli-managed'
$markerPrefix = 'herdr-board install-cli.ps1 managed board.exe sha256:'

# Never read a marker through a link or from anything but a regular file.
if ($null -ne (Get-Entry $marker) -and -not (Test-RegularFile $marker)) {
    Fail 1 "ownership marker path is not a regular non-symlink file: $marker"
}

$managed = $false
$managedChecksum = ''
if (Test-RegularFile $marker) {
    $existingMarker = Read-Marker $marker
    if ($existingMarker.StartsWith($markerPrefix, [System.StringComparison]::Ordinal)) {
        $candidate = $existingMarker.Substring($markerPrefix.Length)
        if ($candidate -cmatch '^[0-9a-f]{64}$') {
            $managed = $true
            $managedChecksum = $candidate
        }
    }
}

# A valid checksum marker is the only permission this script gives itself to
# replace an existing command. Before every managed update, ensure the command
# is still the exact regular file installed previously.
if ($null -ne (Get-Entry $destination) -and -not $managed) {
    [Console]::Error.WriteLine("install-cli.ps1: refusing to overwrite unmanaged destination: $destination")
    Fail 1 'move it aside or set HERDR_BOARD_CLI_INSTALL_DIR to a different absolute directory'
}
if ($managed) {
    if (-not (Test-RegularFile $destination)) {
        Fail 1 "managed destination is not a regular non-symlink file; refusing to overwrite it: $destination"
    }
    if ((Get-Sha256 $destination) -cne $managedChecksum) {
        Fail 1 "managed destination checksum does not match its ownership marker; refusing to overwrite it: $destination"
    }
}

# Stage both files beside their destinations so readers never observe partial
# contents; the final placement is a same-directory rename.
$suffix = [System.Guid]::NewGuid().ToString('N').Substring(0, 12)
$temporary = Join-Path $installDir ".board.$suffix.exe"
$markerTemporary = Join-Path $installDir ".herdr-board-cli-managed.$suffix"
$backup = Join-Path $installDir ".board.$suffix.old"

# Best-effort sweep of backups left by earlier updates that replaced a running
# board.exe. Only this script's own `.board.<hex>.old` names are touched.
Get-ChildItem -LiteralPath $installDir -Force -File -Filter '.board.*.old' -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -cmatch '^\.board\.[0-9a-f]{12}\.old$' } |
    ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }
$placedDestination = $false
$exitCode = 1
try {
    Copy-Item -LiteralPath $sourceBin -Destination $temporary
    $installedChecksum = Get-Sha256 $temporary
    $markerValue = "$markerPrefix$installedChecksum"
    [System.IO.File]::WriteAllText($markerTemporary, "$markerValue`n", $utf8NoBom)

    if ($managed) {
        # ReplaceFile swaps the verified, owned destination in one step. A
        # running board.exe cannot be deleted but can be renamed, so the old
        # image goes to a named backup that is removed now if possible, or by
        # the next install once that process has exited.
        try {
            [System.IO.File]::Replace($temporary, $destination, $backup)
        } catch {
            Fail 1 "could not replace managed destination: $destination ($($_.Exception.Message))"
        }
        Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
    } else {
        # File.Move never overwrites: a destination created during install
        # makes this fail instead of being clobbered.
        try {
            [System.IO.File]::Move($temporary, $destination)
        } catch {
            Fail 1 "refusing to overwrite destination created during install: $destination"
        }
        $placedDestination = $true
    }

    # Postcondition: destination must be a regular non-symlink file whose
    # content matches the staged checksum.
    if (-not (Test-RegularFile $destination)) {
        Fail 1 "destination is not a regular non-symlink file after install: $destination"
    }
    if ((Get-Sha256 $destination) -cne $installedChecksum) {
        Fail 1 'destination checksum mismatch after install'
    }

    try {
        if (Test-RegularFile $marker) {
            [System.IO.File]::Replace($markerTemporary, $marker, [NullString]::Value)
        } else {
            [System.IO.File]::Move($markerTemporary, $marker)
        }
    } catch {
        Fail 1 "could not create ownership marker: $marker"
    }
    # Postcondition: marker must be a regular non-symlink file whose content
    # matches the expected value exactly.
    if (-not (Test-RegularFile $marker)) {
        Fail 1 "ownership marker path is not a regular non-symlink file after placement: $marker"
    }
    if ((Read-Marker $marker) -cne $markerValue) {
        Fail 1 'ownership marker content mismatch after placement'
    }
    $placedDestination = $false
    $exitCode = 0
} finally {
    # A first install whose marker could not be placed must not leave an
    # unowned command behind: roll it back if it is still exactly our copy.
    if ($placedDestination -and (Test-RegularFile $destination)) {
        try {
            if ((Get-Sha256Hex $destination) -ceq $installedChecksum) {
                Remove-Item -LiteralPath $destination -Force
            }
        } catch {
            # Best-effort rollback.
        }
    }
    Remove-Item -LiteralPath $temporary, $markerTemporary -Force -ErrorAction SilentlyContinue
}

if ($exitCode -eq 0) {
    Write-Output "install-cli.ps1: installed managed CLI at $destination"
}
exit $exitCode
