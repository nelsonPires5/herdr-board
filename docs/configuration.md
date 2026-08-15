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
(`--session-id <id>`), `claude` (`--resume <id>`), `codex` (`resume <id>` subcommand), and
`opencode` (`-s <id>`) declare
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

### The built-in `opencode` vs. headless `opencode run`

`opencode` is a **built-in harness** dispatched as a Herdr-managed interactive agent (kind
`"opencode"`), starting the OpenCode **TUI** (`opencode [project]`). Models are free-form
`provider/model` via `-m`; the daemon runs `opencode models --verbose` to discover live models and
their per-model reasoning variants, and falls back to a static catalog whenever the CLI is missing or
yields nothing. The fallback truthfully lists OpenCode Zen Nemotron 3 Ultra Free —
`opencode/nemotron-3-ultra-free` — which declares no variants (verified live: `variants: {}`), so
selecting it offers **no board effort**; alongside it a fixture model
`opencode/deepseek-v4-flash-free` carries `low`/`high`/`max` efforts so the model/effort UX stays
demonstrable without a live CLI. The binary is resolved from `$OPENCODE_BIN`, else
`opencode` on `PATH`. The board calls the variant dimension **effort**; the root/TUI has **no
`--variant` flag** (verified against opencode 1.18.15 — the spelling exists only on `opencode run`),
so with an effort the launch defines a process-local `OPENCODE_CONFIG_CONTENT` config (see
[`board_core::harness::opencode`]) carrying a stable custom agent `herdr-board` with exactly
`model` + `variant` (board `off` → opencode `none`) and selects it with `--agent herdr-board`; the
env is persisted in the launch spec, so resume/rescue keep it. Without an effort the model stays
`-m provider/model` and no config env is injected. The
two permission modes map to exact verified spellings: `default` (no flag) and `auto-approve`
(`--auto`).

**The `opencode` name is shadowed by the built-in**, so a `[harness.opencode]` section in
`config.toml` is unreachable. A headless `opencode run` wrapper must be configured under a
**different name**, e.g.:

```toml
[harness.opencode-run]
argv = ["opencode", "run", "--model", "{model}", "--variant", "{effort}"]
```

(`opencode run` is the one surface that accepts `--variant`; the TUI does not.) It then behaves
like every other config-defined harness: prompt via `$BOARD_PROMPT`, `resume` only
if you declare it, and the configured runner bridge — never the managed `agent.start` path.

### The built-in `antigravity` (agy) vs. a headless `agy run`

`antigravity` is a **built-in harness** dispatched as a Herdr-managed interactive agent (kind
`"agy"`), starting the Antigravity CLI **TUI** (`agy`). Models come from a **live catalog only**: the
daemon runs `agy --output-format json models` (a root flag — `agy models --output-format json`
fails) per validation and normalizes variant ids (`gemini-3.7-flash-high|medium|low`) onto one base
model with efforts `low|medium|high` (`--model <base> --effort <effort>`); fixed-effort models
(`claude-sonnet-4-6`, `claude-opus-4-6-thinking`) carry no effort. There is **no static fallback**:
while the catalog is unavailable (`agy` missing/failing) model selection is free-form and stored
models keep running; once the catalog is back, removed models are rejected at enqueue/edit. The
binary is resolved from `$AGY_BIN`, else `agy` on `PATH`.

The two permission modes map to exact verified spellings: `sandbox` (`--sandbox`),
`always-proceed` (`--dangerously-skip-permissions`); no flag (the harness default) keeps the
user's configured `toolPermission`. The TUI mints its own
conversation id, which the daemon captures from the `herdr:antigravity_cli` integration after the
first prompt; `board retry` re-attaches to the same conversation (`--conversation <id>`) in a fresh
pane (agy has no fork, and every `--conversation` hop launches a fresh pane by design). When the
recorded conversation no longer exists agy starts a new one: the daemon persists the new id and a
`system` card comment names both. Without the integration the run still executes, a `system`
warning explains that focus/retry may be unavailable, and reuse/rescue fail closed.

**The `antigravity` name is shadowed by the built-in**, so a `[harness.antigravity]` section in
`config.toml` is unreachable. A headless `agy run` wrapper must be configured under a
**different name**, e.g.:

```toml
[harness.agy-run]
argv = ["agy", "run", "--model", "{model}", "--conversation", "{session}"]
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
| `OPENCODE_BIN` | OpenCode binary used for live `opencode models --verbose` model discovery; default `opencode` on `PATH`. An unset/invalid binary keeps the static fallback catalog. |
| `AGY_BIN` | Antigravity binary used for the live `agy --output-format json models` model catalog; default `agy` on `PATH`. An unset/invalid binary means the catalog is unavailable: model selection becomes free-form (stored models keep running) and the picker is empty. |
| `BOARD_TIMEOUT_UNIT_SECS` / `BOARD_LOCAL_POLL_MS` / `BOARD_TICK_MS` | Test-tuning knobs. |
