# Fake harness used by the daemon integration tests on Windows: the twin of
# fake-agent.sh, with the same env contract. Receives the board env
# (BOARD_PROMPT / BOARD_CARD_ID / BOARD_RUN_ID / BOARD_SOCKET) and the built
# `board` binary via BOARD_BIN. Sleeps, then comments + closes the run through
# the real CLI (so the CLI request path is exercised too).
$ErrorActionPreference = 'Stop'

foreach ($name in 'BOARD_CARD_ID', 'BOARD_SOCKET', 'BOARD_BIN') {
    if (-not [Environment]::GetEnvironmentVariable($name)) {
        [Console]::Error.WriteLine("$name required")
        exit 1
    }
}

$sleep = 1.0
if ($env:FAKE_AGENT_SLEEP) {
    $sleep = [double]::Parse($env:FAKE_AGENT_SLEEP, [Globalization.CultureInfo]::InvariantCulture)
}
Start-Sleep -Milliseconds ([int]($sleep * 1000))

# Simulate an agent that crashes / exits without ever calling `board done`.
if ($env:FAKE_AGENT_SILENT -eq '1') {
    exit 0
}

& $env:BOARD_BIN comment 'fake: done work'
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
$outcome = if ($env:FAKE_AGENT_OUTCOME) { $env:FAKE_AGENT_OUTCOME } else { 'ok' }
& $env:BOARD_BIN done --outcome $outcome
exit $LASTEXITCODE
