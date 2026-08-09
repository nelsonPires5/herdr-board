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
(`--session-id <id>`), `claude` (`--resume <id>`), and `codex` (`resume <id>` subcommand) declare
it themselves. Setting `resume = true`
promises that your `argv` re-attaches to the conversation named by `$BOARD_RESUME_SESSION_ID`, which
the daemon sets on the reopened pane along with `BOARD_RESCUE=1` (the run's argv is persisted fully
materialized, so there is no placeholder left to substitute). Without it, focusing such a run is
refused explicitly rather than starting a fresh conversation that would re-run the task.

### The built-in `codex` vs. headless `codex exec`

`codex` is a **built-in harness** dispatched as a Herdr-managed interactive agent (kind `"codex"`).
The daemon reads the CLI's local `$CODEX_HOME/models_cache.json` (default
`~/.codex/models_cache.json`) to offer its visible models and each model's supported reasoning
levels. If the cache is missing or malformed, model entry remains free-form and the static effort
ladder remains available. Cache-only levels the board protocol does not support, such as `ultra`,
are omitted.

The permission selector mirrors Codex's three presets and stores stable ids:

- **Ask for approval** (`ask-for-approval`) — `--sandbox workspace-write --ask-for-approval on-request`.
- **Approve for me** (`approve-for-me`) — `--approve-for-me`, which uses Codex's automatic reviewer
  within its workspace-write sandbox and may consume additional model usage.
- **Full access** (`full-access`) — `--dangerously-bypass-approvals-and-sandbox`; no sandbox or
  approval prompts, so use only in an externally isolated environment.

**Headless `codex exec`** remains separate: the one-shot non-interactive mode is deliberately not
the built-in. If you want it, configure it as an ordinary unmanaged harness under a **different
name**, e.g.:

```toml
[harness.codex-exec]
argv = ["codex", "exec", "--sandbox", "workspace-write", "{model}", "{permission_mode}"]
```

It then behaves like every other config-defined harness: prompt via `$BOARD_PROMPT`, `resume` only
if you declare it, and the configured runner bridge — never the managed `agent.start` path.

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
| `BOARD_RESCUE` / `BOARD_RESUME_SESSION_ID` / `BOARD_RESCUED_RUN_ID` | Set on a *reopened* run pane only: marks it as an ephemeral rescue (not a tracked run), names the conversation to resume, and labels which run it continues. A reopened pane gets `BOARD_CARD_ID`/`BOARD_SOCKET`/`BOARD_BIN` but explicitly clears `BOARD_RUN_ID` to empty (treated as unset) — that is the actor credential for `comment`/`done`, and a rescued pane must not be able to write to the finished run. |
| `BOARD_TIMEOUT_UNIT_SECS` / `BOARD_LOCAL_POLL_MS` / `BOARD_TICK_MS` | Test-tuning knobs. |
