# Research notes (verified through 2026-07-22)

This page is historical research and verification context, not a runtime or wire-contract source.
The current contract is the typed code plus [`docs/README.md`](README.md), `docs/protocol.md`,
`schema.sql`, and the migration tests.

Condensed output of three research passes: local herdr introspection, prior art, and technical building blocks.

## A. herdr capability map (v0.7.5, protocol 17, verified locally)

JSON request/response + events use the Unix socket at
`~/.config/herdr/herdr.sock` (or `HERDR_SOCKET_PATH` for a named session).
The captured `herdr api schema --json` contains 89 request methods and 26 event
subscription selectors. `herdr api snapshot` exposes live
workspaces/tabs/panes/agents. IDs remain shaped like `w3`, `w3:t1`, `w3:p1`.

| Need | Herdr 0.7.5 command / protocol-17 API |
|---|---|
| Create workspace | `herdr workspace create --cwd PATH --label TEXT --env K=V --no-focus` |
| Worktree per card | `herdr worktree create --workspace ID\|--cwd PATH --branch NAME --base REF --json` (+ open/remove/list) |
| Place a pane first | Use `tab.create` or `pane.split {workspace_id, target_pane_id, cwd, env, direction, focus}`. Placement, cwd, and environment are established before managed launch. |
| Start a managed agent | `herdr agent start NAME --kind KIND --pane ID [--timeout MS] -- [AGENT_ARG…]`; socket `agent.start` is `{name, kind, pane_id, args, timeout_ms}`. `kind` chooses the canonical executable; `args` excludes it. The protocol-16 workspace/tab/split/env start fields are gone. |
| Inspect readiness | `herdr agent get TARGET` / `agent.get {target}` returns `interactive_ready` and `launch_pending`; readiness is `interactive_ready=true && launch_pending=false`. `agent.wait` waits for agent status, not this startup predicate. |
| Submit a card task | `herdr agent prompt TARGET TEXT`; `agent.prompt {target,text,wait?}` preserves multiline text and optionally waits for status. No keystroke/Enter pair is needed. |
| Read output | `herdr agent read TARGET --source recent-unwrapped --lines N` / `agent.read` reads terminal screen/scrollback, not a semantic result. |
| Run an unmanaged command | `herdr pane run PANE_ID COMMAND…` exists only as a CLI boundary: protocol 17 exposes no `pane.run` socket method. It schedules the command, so herdr-board uses a temporary self-cleaning runner and a board callback for silent child exit. |
| Event stream | `events.subscribe` is a persistent raw-socket connection. Subscriptions use dotted names; emitted envelopes may use underscore `data.type` names or a dotted top-level `event` with no `data.type`. See exact shapes below. |
| Notify human | `herdr notification show TITLE --body … --sound none\|done\|request` |
| Integration input | `herdr pane report-agent PANE --source ID --agent LABEL --state idle\|working\|blocked\|unknown [--seq N]`; `done` is an output status, not an accepted report input. |

**Event shapes**: a status subscription is
`{"type":"pane.agent_status_changed","pane_id":"w1:p2"}` and requires a
concrete existing pane. Emitted status data requires
`{pane_id, workspace_id, agent_status}` and may add `agent`, `display_agent`,
`title`, and `state_labels`. Exit/close subscriptions are global
`pane.exited`/`pane.closed`; their emitted data carries `pane_id` and
`workspace_id`. The client accepts both
`{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p2","workspace_id":"w1","agent_status":"working","agent":"pi"}}`
and protocol-17's observed
`{"event":"pane.agent_status_changed","data":{"pane_id":"w1:p2","workspace_id":"w1","agent_status":"working","agent":"pi"}}`
form.

**Agent status**: per-harness integrations report precise lifecycle and session
identity. On 2026-07-22, `herdr integration status` reported Pi current at **v6**
(`~/.pi/agent/extensions/herdr-agent-state.ts`) and Claude current at **v7**
(`~/.claude/hooks/herdr-agent-state.sh`). Installation mutates personal harness
configuration, so herdr-board never performs it; the matching integration is a
user prerequisite for precise live status. On a managed pane, Herdr can derive
output `done` from the integration's terminal end-of-turn idle report.
`idle`/`done` still do not semantically complete a board run: explicit
`board done` remains terminal truth, while agent `done` parks the card in
`awaiting` for review.

**Plugin architecture** (learned from installed `herdr-file-viewer`): manifest `herdr-plugin.toml` with `id/name/version/min_herdr_version`, `[[build]]` (install-time command), `[[panes]]` (id, title, placement=split/tab/overlay, command argv → herdr spawns the TUI in a pane), `[[actions]]` (shell commands, invocable via `herdr plugin action invoke` or `[[keys.command]]` keybindings, receive `PluginInvocationContext`: focused pane/cwd/agent, workspace/tab, selected text). Install from github or local → `~/.config/herdr/plugins/…`, registry `plugins.json`. Runtime env: `HERDR_BIN_PATH`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_CONTEXT_JSON`. Plugins have no special powers — they shell out to the same CLI/socket.

**Gaps to design around**: Herdr has no per-agent model/effort abstraction (the adapter layer is ours); configured commands need a CLI bridge because `pane.run` is absent from the socket schema; terminal output remains screen-scrape, so agents should write files/comments; and `events.subscribe` needs a persistent raw-socket client with one concrete status subscription per watched pane.

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
**not** the shipped managed-launch transport. Under Herdr 0.7.5/protocol 17, herdr-board creates a
pane first, starts the explicit agent kind with prompt-free startup args, supplies the system prompt
through a temporary `0600` file, waits for `interactive_ready`, and sends the card prompt only via
`agent.prompt`.

**Pi Coding Agent 0.80.10**: `--model <provider/model>`; `--thinking off|minimal|low|medium|high|xhigh|max`; direct CLI supports `--append-system-prompt <text>` and positional prompts; exact mint/resume via `--session-id <id>`; retry fork via `--fork <source-id> --session-id <new-id>`. Pi has no per-tool permission prompts; `--approve`/`--no-approve` controls project trust and must not be mapped to the board permission field. Models are runtime provider/auth/user configuration, so the board does not persist a parsed `--list-models` catalog. At verification time the user default was `openai-codex/gpt-5.6-sol`, thinking `xhigh`; the isolated smoke detects this at runtime and overrides only the invocation to `low`.

**Claude Code**: `-p/--print` headless; `--output-format text|json|stream-json` (+`--verbose`); `--system-prompt` / **`--append-system-prompt`** (+ `-file` variants); `--model`; **`--effort low|medium|high|xhigh|max`** (first-class flag); `--permission-mode acceptEdits|auto|bypassPermissions|manual|dontAsk|plan`; `--allowedTools`/`--disallowedTools`; **`--session-id <uuid>`** (pre-assign), `--resume <id>`, `--fork-session` (retry without polluting), `--no-session-persistence`; `--max-budget-usd` (print-only); `--json-schema` (structured final output); `--input-format stream-json` (long-lived multi-prompt process); `--bare`; `--bg`; `-n/--name` (label session). Hooks: Stop/StopFailure, SessionStart/SessionEnd — **Stop not fired on silent tool stop** ([#29881](https://github.com/anthropics/claude-code/issues/29881)), don't rely on it alone.

**Adapter shape for built-in/future harnesses** = (binary, prompt style, model flag, permission flag, resume mechanism, resulting session id):
- codex: `codex exec "p"` — `-m`, `--sandbox read-only|workspace-write|danger-full-access`, `--json`, `--output-last-message <path>`, resume `codex exec resume <id>`; effort via `-c model_reasoning_effort=…` (unverified key).
- gemini: `gemini -p "p"` — `-m`, `--approval-mode default|auto_edit|yolo|plan`; `-o json` (unverified spelling).
- opencode: `opencode run "p"` — `--model provider/model`; `opencode serve` + `--attach` to amortize startup (session flags unverified).

**Agent SDK** (`@anthropic-ai/claude-agent-sdk` / `claude-agent-sdk` py): `query()` with systemPrompt/permissionMode/model/resume + programmatic `canUseTool`. Beats CLI when you want per-tool-call permission decisions in code; loses when the orchestrator must be harness-agnostic (our case → CLI subprocess).

## D. Building-block recommendations (from research; adapted to our TUI-in-pane choice)

- **Storage**: SQLite WAL, daemon sole writer; CLI/TUI go through the daemon socket. JSON/md files race with concurrent writers.
- **Agent→board channel**: tiny CLI (`board comment/move/done`) > MCP for v1 — works from any harness via Bash, allowlistable (`Bash(board *)`), zero per-harness MCP config. MCP wrapper later.
- **Completion**: explicit agent signal > process exit (headless) > Stop/SessionEnd hook > herdr status events > idle heuristics. Never idle-scraping alone.
- **Concurrency**: per-space FIFO + global semaphore; worktree mode for parallelism on one repo.
- **TUI kanbans that exist** (rust_kanban, kanban-tui/ratatui, kanbanban) are standalone apps, not embeddable libs — we write our own view (ratatui or bubbletea).
- **Cost/safety**: per-run timeout; `--max-budget-usd` where supported; `bypassPermissions` explicit opt-in only.

Sources: vibe-kanban repo/DeepWiki/docs, claude-task-master, claude-squad, Backlog.md, kandev, ai-agent-board, agent-kanban, Copilot coding-agent docs, claude-code-action docs, Claude Code hooks reference, Agent SDK TS/Python refs, codex/gemini/opencode docs. (Full links in the repos above; local herdr facts verified with `herdr api schema`, `herdr --default-config`, `herdr integration status`.)
