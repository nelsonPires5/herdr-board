# Research notes (verified through 2026-08-04)

This page is historical research and verification context, not a runtime or wire-contract source.
The current contract is the typed code plus [`docs/README.md`](README.md), `docs/protocol.md`,
`schema.sql`, and the migration tests.

Condensed output of three research passes: local Herdr introspection, prior art, and technical building blocks.

## A. Herdr capability map (Herdr 0.8.2, socket protocol 20; local introspection)

JSON request/response and events use the Unix socket at
`~/.config/herdr/herdr.sock` (or `HERDR_SOCKET_PATH` for a named session).
The captured `herdr api schema --json` contains **90 request methods, 26 emitted event
kinds, and 27 event-subscription selectors**. `herdr api snapshot` exposes live
workspaces/tabs/panes/agents, with IDs shaped like `w3`, `w3:t1`, and `w3:p1`.

| Need | Herdr 0.8.2 command / current socket API |
|---|---|
| Create workspace | `herdr workspace create --cwd PATH --label TEXT --env K=V --no-focus` |
| Worktree per card | `herdr worktree create --workspace ID\|--cwd PATH --branch NAME --base REF --json` (+ open/remove/list) |
| Place a pane first | Use `tab.create` or `pane.split {workspace_id, target_pane_id, cwd, env, direction, ratio, focus}`. Placement, cwd, and environment are established before managed launch. |
| Start a managed agent | `herdr agent start NAME --kind KIND --pane ID [--timeout MS] -- [AGENT_ARG…]`; the socket `agent.start` request is `{name, kind, pane_id, args, timeout_ms}`. `kind` selects the canonical executable and `args` excludes it. Board-side launch therefore stays pane-first rather than embedding placement in the agent request. |
| Inspect readiness | `herdr agent get TARGET` / `agent.get {target}` returns `interactive_ready` and `launch_pending`; readiness is `interactive_ready=true && launch_pending=false`. `agent.wait` waits for agent status, not this startup predicate. |
| Capture a reported session | `agent.get` → `AgentInfo.agent_session` (`AgentSessionInfo | null` in the protocol-20 schema): `{agent, kind, source, value}` with `kind` ∈ {`id`, `path`}; the field is absent when no session was reported. The codex integration reports its self-minted thread id here and the opencode integration its `ses_…` session id; board-herdr decodes the field and the daemon's bounded post-launch capture accepts only the expected agent + `kind:"id"` + non-empty `value` (opencode additionally pins the integration's `source`), promoting it atomically onto run+card. |
| Submit a card task | `herdr agent prompt TARGET TEXT`; `agent.prompt {target,text,wait?}` preserves multiline text and optionally waits for status. Herdr handles the short send-text/Enter settling delay; no synthetic keystroke/Enter pair is needed. |
| Read output | `herdr agent read TARGET --source recent-unwrapped --lines N` / `agent.read` reads terminal screen/scrollback, not a semantic result. Read responses include `truncated`; `true` means older terminal rows were omitted. |
| Run an unmanaged command | `herdr pane run PANE_ID COMMAND…` remains a CLI-only boundary: the current socket schema has no `pane.run` method. It schedules the command, so herdr-board uses a temporary self-cleaning runner and a board callback for silent child exit. |
| Event stream | `events.subscribe` is a persistent raw-socket connection. Subscription entries use dotted names; emitted envelopes may use underscore `data.type` names or a dotted top-level `event` with no `data.type`. See exact shapes below. |
| Notify human | `herdr notification show TITLE --body … --sound none\|done\|request` |
| Integration input | `herdr pane report-agent PANE --source ID --agent LABEL --state idle\|working\|blocked\|unknown [--seq N]`; `done` is an output status, not an accepted report input. |

### The protocol-20 delta

The board-used RPC shapes are unchanged between the earlier capture and the current
contract: `ping`, `session.snapshot`, workspace list/create/close, tab create/list,
pane split/list/get/focus/rename/read/send-text/send-keys/close/layout, agent
start/get/prompt/wait, `notification.show`, and `events.subscribe` retain their
request fields, result envelopes, and event forms. The delta is additive:

- `workspace.move_block` accepts `workspace_ids` and an optional `before_workspace_id`
  for atomic block movement, including worktree groups.
- `workspace.reordered` is a new emitted event, and a matching
  `workspace.reordered` subscription selector is available. Its event data carries
  `workspace_ids`, `workspaces`, and an optional `before_workspace_id`.
- The integration target vocabulary adds `antigravity_cli` (CLI spelling
  `antigravity-cli`) and `grok`, including their session-reporting/native-restore
  support. They are not board built-in harnesses.

- `AgentInfo.agent_session` is `AgentSessionInfo | null` (`{agent, kind,
  source, value}`, `kind` ∈ {`id`, `path`}) and absent when a pane reported no
  session. The board decodes it for the self-minting built-ins (codex and
  opencode): the integration's reported id is captured after launch and
  persisted atomically with the run promotion; a Mint keeps `session_id = NULL`
  until then. OpenCode's capture additionally pins the `source` the current
  Herdr opencode integration reports.

The board client does not call `workspace.move_block` and does not need to decode
`workspace.reordered` to run its queue. Unknown additive events remain observable as
unhandled events rather than changing the used transport.

**Event shapes**: a status subscription is
`{"type":"pane.agent_status_changed","pane_id":"w1:p2"}` and requires a
concrete existing pane. Emitted status data requires
`{pane_id, workspace_id, agent_status}` and may add `agent`, `display_agent`,
`title`, and `state_labels`. Exit/close subscriptions are global
`pane.exited`/`pane.closed`; their emitted data carries `pane_id` and
`workspace_id`. The client accepts both
`{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p2","workspace_id":"w1","agent_status":"working","agent":"pi"}}`
and
`{"event":"pane.agent_status_changed","data":{"pane_id":"w1:p2","workspace_id":"w1","agent_status":"working","agent":"pi"}}`
forms.

### 0.8.0 behavior that shaped live validation

The release notes include several runtime changes that are not new board RPC
fields, but matter at the Herdr boundary:

- **Lifecycle ownership:** known-agent integrations now leave pane ownership with a
  confirmed process exit. Restarting Pi with the same saved session can restore its
  lifecycle state; nested or ephemeral Codex sessions do not replace the owning
  pane's resumable session, and Pi RPC/JSON/print processes do not claim the TUI
  pane's lifecycle. Board status is still a hint: `pane.exited` and explicit
  `board done` remain the reliable run boundaries.
- **Prompt delivery:** Herdr now waits briefly after sending prompt text before
  pressing Enter. The board still waits for `interactive_ready && !launch_pending`,
  then uses `agent.prompt` for the exact multiline task; live validation must check
  arrival at the agent, not just startup success.
- **Headless restore:** headless servers can resume restored agent sessions without
  waiting for a TUI client to attach. This is relevant to `run.focus` rescue and
  restart recovery, which must not assume an attached desktop client.
- **Plugin root:** relative plugin commands resolve from the plugin root. A plugin's
  build/action commands are therefore not allowed to depend on the caller's cwd;
  installation and the open-board action should be checked from a disposable session.
- **Read truncation/history:** pane and agent reads report `truncated: true` when
  older terminal rows were omitted. Herdr may collect text history for idle
  alternate-screen agents and restore the application viewport afterward. Neither
  read text nor a truncated flag is a completion/result channel; board agents report
  through comments and `board done`.

**Agent status and integrations**: the installed 0.8.0 CLI exposes the integration
targets from `herdr integration install --help`. On the host inspected for this
update, `herdr integration status` reports Pi **current (v8)** at
`~/.pi/agent/extensions/herdr-agent-state.ts` and Claude **current (v7)** at
`~/.claude/hooks/herdr-agent-state.sh`. Installation mutates personal harness
configuration, so herdr-board never performs it; the matching integration is a
user prerequisite for precise live status. On a managed pane, Herdr can derive
output `done` from the integration's terminal end-of-turn idle report.
`idle`/`done` still do not semantically complete a board run: explicit
`board done` remains terminal truth, while agent `done` parks the card in
`awaiting` for review.

**Plugin architecture** (learned from installed `herdr-file-viewer`): manifest `herdr-plugin.toml` with `id/name/version/min_herdr_version`, `[[build]]` (install-time command), `[[panes]]` (id, title, placement=split/tab/overlay, command argv → Herdr spawns the TUI in a pane), `[[actions]]` (shell commands, invocable via `herdr plugin action invoke` or `[[keys.command]]` keybindings, receive `PluginInvocationContext`: focused pane/cwd/agent, workspace/tab, selected text). Install from GitHub or local → `~/.config/herdr/plugins/…`, registry `plugins.json`. Herdr 0.8.2 resolves relative command paths from the plugin root. Runtime env: `HERDR_BIN_PATH`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_CONTEXT_JSON`. Plugins have no special powers — they shell out to the same CLI/socket.

**Gaps to design around**: Herdr has no per-agent model/effort abstraction (the adapter layer is ours); configured commands need a CLI bridge because `pane.run` is absent from the socket schema; terminal reads are bounded screen/scrollback observations and may be truncated, so agents should write files/comments; and `events.subscribe` needs a persistent raw-socket client with one concrete status subscription per watched pane.

> **Historical comparison only — not support policy:** the earlier Herdr 0.7.5 / protocol-17
> capture had 89 request methods, 25 emitted event kinds, 26 subscription selectors, and Pi
> integration v6. It is retained solely to explain the additive protocol-20 delta above; all
> current gates and integration instructions use Herdr 0.8.2 / protocol 20, Pi v8, and Claude v7.

## B. Prior art

| Tool | Storage | Trigger → run | Completion | Human gate | Lesson |
|---|---|---|---|---|---|
| **vibe-kanban** (BloopAI, Rust+React, sunset) | SQLite; evolved to tasks / workspaces(worktree+branch) / sessions / execution_processes | "Start attempt" → worktree + setup script + executor adapter (10 harnesses; named config variants: model, effort, approval policy) | Process exit → auto-move to `inreview` | Diff panel; inline comments batched → follow-up prompt to the **same resumed session**; then PR/squash-merge | task↔attempt↔execution separation; review-feedback-into-session is the killer feature |
| **claude-task-master** | tasks.json | Doesn't spawn — MCP server the agent queries (`next` via dependency graph) | Agent self-reports status | Convention only | Dependency-driven "next task"; agents forget to update status |
| **claude-squad** | ~/.claude-squad | tmux session + worktree per instance | **tmux pane scraping — the known weak point**; `--autoyes` brittle | Diff tab, manual | Don't rely on pane-idle detection |
| **Backlog.md** | one md file per task, YAML frontmatter | Passive; agent drives via CLI/MCP | Agent checks off acceptance criteria | 3 checkpoints: spec/plan/code review | Files-in-repo = agent-legible + git-diffable; columns are just frontmatter values |
| **kandev** | SQLite ~/.kandev | Multi-step pipelines mixing agents per step ("Opus plans → Copilot implements → Codex reviews"); worktree per task | Server-side supervision | Review-first workspace (editor/terminal/diff/chat) | Per-column different agent/model is proven useful |
| **ai-agent-board** | SQLite/Postgres | Drag to In Progress → agent config panel; provider pattern over 6 agents | WebSocket streaming; groups auto-advance to Review when all children succeed | Review column | dnd + per-drag config works; auto-advance never to Done |
| **agent-kanban** | daemon polls board | Pull model: agent claims task atomically, worktree per task | PR merge webhook | Leader-agent-or-human review | Atomic claiming if multiple agents pull |
| **Copilot coding agent / claude-code-action** | GitHub issues | Assign issue / label / @mention → Actions run | Draft PR + updating comment; 59-min cap | PR review | "Column = label" adaptation; hard run caps exist for a reason |

More in the space: Cline kanban, Fusion, Nimbalyst, Crystal, Conductor, Omnara — [awesome-agent-orchestrators](https://github.com/andyrewlee/awesome-agent-orchestrators).

## C. Harness CLI capabilities (verified locally, 2026-07-17)

The flags below describe direct/local CLI capabilities and historical adapter research; they are
**not** the shipped managed-launch transport. For a shipped managed run, herdr-board creates a
pane first, starts the explicit agent kind with prompt-free startup args, supplies the system prompt
through a temporary `0600` file, waits for `interactive_ready`, and sends the card prompt only via
`agent.prompt` under the exact Herdr 0.8.2/protocol-20 gate described above.

**Pi Coding Agent 0.80.10**: `--model <provider/model>`; `--thinking off|minimal|low|medium|high|xhigh|max`; direct CLI supports `--append-system-prompt <text>` and positional prompts; exact mint/resume via `--session-id <id>`; retry fork via `--fork <source-id> --session-id <new-id>`. Pi has no per-tool permission prompts; `--approve`/`--no-approve` controls project trust and must not be mapped to the board permission field. Models are runtime provider/auth/user configuration, so the board does not persist a parsed `--list-models` catalog. At verification time the user default was `openai-codex/gpt-5.6-sol`, thinking `xhigh`; the isolated smoke detects this at runtime and overrides only the invocation to `low`.

**Claude Code**: `-p/--print` headless; `--output-format text|json|stream-json` (+`--verbose`); `--system-prompt` / **`--append-system-prompt`** (+ `-file` variants); `--model`; **`--effort low|medium|high|xhigh|max`** (first-class flag); `--permission-mode acceptEdits|auto|bypassPermissions|manual|dontAsk|plan`; `--allowedTools`/`--disallowedTools`; **`--session-id <uuid>`** (pre-assign), `--resume <id>`, `--fork-session` (retry without polluting), `--no-session-persistence`; `--max-budget-usd` (print-only); `--json-schema` (structured final output); `--input-format stream-json` (long-lived multi-prompt process); `--bare`; `--bg`; `-n/--name` (label session). Hooks: Stop/StopFailure, SessionStart/SessionEnd — **Stop not fired on silent tool stop** ([#29881](https://github.com/anthropics/claude-code/issues/29881)), don't rely on it alone.

**Adapter shape for built-in/future harnesses** = (binary, prompt style, model flag, permission flag, resume mechanism, resulting session id):
- codex (interactive, the managed built-in): `--model` remains free-form, while the visible model slugs and their per-model efforts are discovered from `$CODEX_HOME/models_cache.json`; effort uses `-c model_reasoning_effort=…` (board `off` maps to `none`, protocol-known levels keep their spelling, cache-only `ultra` is filtered). The Codex permission picker is represented by three presets combining approval and sandbox: `ask-for-approval` → `--sandbox workspace-write --ask-for-approval on-request`; `approve-for-me` → `--approve-for-me` (automatic reviewer, workspace-write); `full-access` → `--dangerously-bypass-approvals-and-sandbox`. There is **no `--session-id` for Mint** — codex mints its own thread uuid, so the board persists `NULL` and captures the id the Herdr integration reports via `agent_session`; resume/fork are **subcommands** `codex resume <id>` / `codex fork <id>`; no system-prompt-file equivalent (prompt travels only through `agent.prompt`).
- codex headless: `codex exec "p"` — `-m`, `--sandbox …`, `--json`, `--output-last-message <path>`, resume `codex exec resume <id>`. Not a built-in: configure it under another harness name (e.g. `[harness.codex-exec]`) to keep it unmanaged.
- gemini: `gemini -p "p"` — `-m`, `--approval-mode default|auto_edit|yolo|plan`; `-o json` (unverified spelling).
- opencode (TUI, the managed built-in; verified against opencode 1.18.15): the TUI is `opencode [project]` with `-m/--model provider/model` (free-form), `--auto` (auto-approve), and `-s/--session <id>` with `--fork` (only with `--session`). The root/TUI has **no `--variant` flag** — `--variant` is the `opencode run` subcommand's per-model "model variant" spelling — so a board effort never rides argv: it is applied through a process-local `OPENCODE_CONFIG_CONTENT` config defining a stable custom agent `herdr-board` with exactly `model` + `variant` (the board calls this dimension effort and maps board `off` to opencode's `none`), selected with `--agent herdr-board` (verified: the TUI accepts `--agent`, and the backend applies the agent's `variant` when its model matches). A fresh TUI session mints its own `ses_…` id — there is **no way to pre-allocate one**, so a Mint carries no session flag and the board persists `NULL` until the integration reports the id via `agent_session`; resume/fork are `-s <id>` / `-s <id> --fork`. Model/effort discovery runs `opencode models --verbose`: each `provider/model` header line is followed by a JSON object whose `variants` map holds the per-model reasoning-effort variants (mapped onto the board ladder, `none`→`off`, unknown keys filtered), with a static fallback catalog when the CLI is missing or yields nothing — it truthfully lists OpenCode Zen Nemotron 3 Ultra Free (`opencode/nemotron-3-ultra-free`, which declares `variants: {}` for real and so offers **no** board effort) plus the fixture model `opencode/deepseek-v4-flash-free` (low/high/max); valid models with no recognized variants stay listed with empty efforts. There is no system-prompt-file equivalent and no reliable per-session/system-prompt flag: the managed prompt channels (`agent.prompt`) are the only prompt transport, never startup argv. For completeness, headless `opencode run --variant <effort>` does accept the variant spelling — a configured `[harness.…-run]` wrapper may use it.

**Agent SDK** (`@anthropic-ai/claude-agent-sdk` / `claude-agent-sdk` py): `query()` with systemPrompt/permissionMode/model/resume + programmatic `canUseTool`. Beats CLI when you want per-tool-call permission decisions in code; loses when the orchestrator must be harness-agnostic (our case → CLI subprocess).

## D. Building-block recommendations (from research; adapted to our TUI-in-pane choice)

- **Storage**: SQLite WAL, daemon sole writer; CLI/TUI go through the daemon socket. JSON/md files race with concurrent writers.
- **Agent→board channel**: tiny CLI (`board comment/move/done`) > MCP for v1 — works from any harness via Bash, allowlistable (`Bash(board *)`), zero per-harness MCP config. MCP wrapper later.
- **Completion**: explicit agent signal > process exit (headless) > Stop/SessionEnd hook > herdr status events > idle heuristics. Never idle-scraping alone.
- **Concurrency**: per-space FIFO + global semaphore; worktree mode for parallelism on one repo.
- **TUI kanbans that exist** (rust_kanban, kanban-tui/ratatui, kanbanban) are standalone apps, not embeddable libs — we write our own view (ratatui or bubbletea).
- **Cost/safety**: per-run timeout; `--max-budget-usd` where supported; `bypassPermissions` explicit opt-in only.

Sources: vibe-kanban repo/DeepWiki/docs, claude-task-master, claude-squad, Backlog.md, kandev, ai-agent-board, agent-kanban, Copilot coding-agent docs, claude-code-action docs, Claude Code hooks reference, Agent SDK TS/Python refs, codex/gemini/opencode docs. (Full links in the repos above; the current Herdr facts were checked with `herdr api schema --json`, relevant `--help` commands, `herdr api snapshot`, and `herdr integration status`.)
