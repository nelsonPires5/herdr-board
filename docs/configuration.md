# Configuration

Everything the daemon reads at startup: the TOML document, config-defined harnesses, and the
environment overrides applied after it is parsed. The [root README](../README.md) links here;
[`design.md`](design.md) explains how these settings feed dispatch.

## Configure the daemon and custom harnesses

Configuration lives at `~/.config/herdr-board/config.toml`; override it with
`HERDR_BOARD_CONFIG`.

```toml
max_concurrent = 3         # global cap on concurrent runs
idle_grace_seconds = 90    # idle without board done before the card is parked in `awaiting` for review

[daemon]
spawner = "herdr"          # herdr = agent panes (default); local = child processes
timeout_unit_secs = 60      # seconds per column timeout_minutes unit
tick_ms = 1000              # timeout/idle watcher interval
local_poll_ms = 2000        # local-spawner liveness interval

[harness.myharness]
argv = ["mytool", "--model", "{model}"]
resume = false             # can this harness resume a recorded conversation? default false
```

Custom harness prompts are delivered through `$BOARD_PROMPT`. The placeholders `{model}`, `{effort}`,
and `{permission_mode}` are available in `argv`. Optional keys `models`, `efforts`, and
`permission_modes` declare the harness's capability catalog.

`resume` declares whether this harness can re-attach to a conversation it recorded earlier, which is
what lets `board card run focus` **reopen a run whose pane was closed** (see
[`protocol.md`](protocol.md) → `run.focus`). It **defaults to `false`**: there is no
universal CLI syntax for resuming, so herdr-board never guesses one — the built-ins `pi`
(`--session-id <id>`) and `claude` (`--resume <id>`) declare it themselves. Setting `resume = true`
promises that your `argv` re-attaches to the conversation named by `$BOARD_RESUME_SESSION_ID`, which
the daemon sets on the reopened pane along with `BOARD_RESCUE=1` (the run's argv is persisted fully
materialized, so there is no placeholder left to substitute). Without it, focusing such a run is
refused explicitly rather than starting a fresh conversation that would re-run the task.

The daemon parses the complete document once, including `[daemon]`, into typed settings. A missing
file or omitted section uses the defaults shown above. An existing file with malformed TOML or an
invalid typed value (including an unknown `spawner`) is an error: the daemon does not silently fall
back to defaults. Environment overrides are applied after parsing and take precedence; malformed
override values also prevent daemon startup.

### Environment variables

| Variable | Purpose |
|---|---|
| `BOARD_DB` | SQLite path. Default: `~/.local/share/herdr-board/board.db`. |
| `BOARD_SOCKET` | Daemon socket. Default: `~/.local/share/herdr-board/boardd.sock`. |
| `BOARD_LOG_DIR` | Structured diagnostic log directory. Default: `~/.local/share/herdr-board/logs`. |
| `HERDR_BOARD_CONFIG` | Configuration path override. |
| `BOARD_SCOPE_PATH` | Canonicalizable scope override for CLI/TUI automation. |
| `BOARD_SPAWNER` | `herdr` or `local`; overrides `[daemon] spawner`. |
| `BOARD_CARD_ID` / `BOARD_RUN_ID` | Injected into runs; `comment`/`done` use them by default. |
| `BOARD_PROMPT` / `BOARD_SYSTEM_PROMPT` | Prompt delivery for custom harnesses. |
| `BOARD_RESCUE` / `BOARD_RESUME_SESSION_ID` / `BOARD_RESCUED_RUN_ID` | Set on a *reopened* run pane only: marks it as an ephemeral rescue (not a tracked run), names the conversation to resume, and labels which run it continues. A reopened pane gets `BOARD_CARD_ID`/`BOARD_SOCKET`/`BOARD_BIN` but **never** `BOARD_RUN_ID` — that is the actor credential for `comment`/`done`, and a rescued pane must not be able to write to the finished run. |
| `BOARD_TIMEOUT_UNIT_SECS` / `BOARD_LOCAL_POLL_MS` / `BOARD_TICK_MS` | Test-tuning knobs. |
