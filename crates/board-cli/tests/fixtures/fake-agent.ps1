# Windows twin of fake-agent.sh (same contract): receives the board env
# (BOARD_PROMPT / BOARD_CARD_ID / BOARD_RUN_ID / BOARD_SOCKET) and the built
# `board` binary via BOARD_BIN. Sleeps, then comments + closes the run through
# the real CLI (so the CLI request path is exercised too).
$ErrorActionPreference = 'Stop'

foreach ($name in 'BOARD_CARD_ID', 'BOARD_SOCKET', 'BOARD_BIN') {
    if (-not [Environment]::GetEnvironmentVariable($name)) { throw "$name required" }
}

$sleep = if ($env:FAKE_AGENT_SLEEP) { [double]$env:FAKE_AGENT_SLEEP } else { 1 }
Start-Sleep -Milliseconds ([int]($sleep * 1000))

# Simulate an agent that crashes / exits without ever calling `board done`.
if ($env:FAKE_AGENT_SILENT -eq '1') { exit 0 }

& $env:BOARD_BIN comment 'fake: done work'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$outcome = if ($env:FAKE_AGENT_OUTCOME) { $env:FAKE_AGENT_OUTCOME } else { 'ok' }
& $env:BOARD_BIN done --outcome $outcome
exit $LASTEXITCODE
