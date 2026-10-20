# Idempotent launcher for the board overlay on Windows -- the PowerShell twin of
# scripts/open-board.sh, used by the Windows `open-board-windows` action and by
# a herdr keybinding. Same "open-or-focus, toggle off on repeat" decision:
#
#   - no board pane in the current workspace     -> open the overlay (focused)
#   - a board pane exists but isn't focused        -> focus it
#   - the focused pane IS the board pane           -> close it
#
# It shells out to the herdr CLI via $env:HERDR_BIN_PATH (herdr injects it; fall
# back to `herdr` on PATH) and parses `herdr pane list` with ConvertFrom-Json
# instead of python3. The pane is identified by its static title (`Board`), the
# legacy filter title, or the scoped title (`Board [scope <middle dot> FILTER]`).
# Any failure degrades to OPEN, preserving always-open behavior.
#
# Kept compatible with Windows PowerShell 5.1 (always present on Windows), and
# ASCII-only because 5.1 reads BOM-less scripts in the ANSI code page.
Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Continue'

$herdrBin = $env:HERDR_BIN_PATH
if ([string]::IsNullOrEmpty($herdrBin)) {
    $herdrBin = 'herdr'
}

# The Windows entrypoint id; the Unix manifest entry is `board`.
$entrypoint = 'board-windows'

# Board pane titles. `\u00B7` is the middle dot the TUI writes in scoped titles;
# spelled as an escape to keep this file ASCII.
$boardTitle = '^(?:Board|Board \[(?:(?:ACTIVE|ALL|ARCHIVED)|.+ \u00B7 (?:ACTIVE|ALL|ARCHIVED))\])$'

function Get-Decision {
    # herdr prints UTF-8; read it as UTF-8 so non-ASCII titles survive. Without
    # a console (e.g. a detached action) the setter can throw: keep the default.
    try {
        [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
    } catch {
        # Fall through with the default code page.
    }
    try {
        $raw = & $herdrBin pane list 2>$null   # outputs JSON (no --json flag)
    } catch {
        return 'OPEN'
    }
    if ($LASTEXITCODE -ne 0 -or $null -eq $raw) {
        return 'OPEN'
    }
    try {
        $data = ($raw -join "`n") | ConvertFrom-Json
    } catch {
        return 'OPEN'
    }
    if ($null -eq $data) {
        return 'OPEN'
    }
    $result = $data
    if ($data.PSObject.Properties['result']) {
        $result = $data.result
    }
    if ($null -eq $result -or -not $result.PSObject.Properties['panes']) {
        return 'OPEN'
    }
    foreach ($pane in @($result.panes)) {
        if ($null -eq $pane) {
            continue
        }
        # The TUI appends its active archive filter via `pane rename`.
        $name = ''
        if ($pane.PSObject.Properties['label'] -and $pane.label) {
            $name = [string]$pane.label
        } elseif ($pane.PSObject.Properties['title'] -and $pane.title) {
            $name = [string]$pane.title
        }
        if ($name -cnotmatch $boardTitle) {
            continue
        }
        $paneId = ''
        if ($pane.PSObject.Properties['pane_id'] -and $pane.pane_id) {
            $paneId = [string]$pane.pane_id
        }
        if (-not $paneId) {
            return 'OPEN'
        }
        if ($pane.PSObject.Properties['focused'] -and $pane.focused -eq $true) {
            return "CLOSE $paneId"
        }
        return "FOCUS $paneId"
    }
    return 'OPEN'
}

$decision = 'OPEN'
try {
    $decision = Get-Decision
} catch {
    $decision = 'OPEN'
}

try {
    if ($decision.StartsWith('FOCUS ')) {
        & $herdrBin plugin pane focus $decision.Substring(6)
    } elseif ($decision.StartsWith('CLOSE ')) {
        & $herdrBin pane close $decision.Substring(6)
    } else {
        & $herdrBin plugin pane open --plugin herdr-board --entrypoint $entrypoint --placement overlay --focus
    }
} catch {
    [Console]::Error.WriteLine("open-board.ps1: could not run ${herdrBin}: $($_.Exception.Message)")
    exit 127
}
exit $LASTEXITCODE
