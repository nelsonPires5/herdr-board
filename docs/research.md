# Research notes (2026-07-14)

Condensed output of three research passes: local herdr introspection, prior art, and technical building blocks.

## A. herdr capability map (v0.7.3, protocol 16, verified locally)

JSON request/response + events over unix socket `~/.config/herdr/herdr.sock`; every CLI subcommand wraps it. `herdr api schema --json` → 80 methods, 23 event types. `herdr api snapshot` → full live state (workspaces/tabs/panes/agents). IDs: `w3`, `w3:t1`, `w3:p1`.

| Need | herdr command / API |
|---|---|
| Create workspace | `herdr workspace create --cwd PATH --label TEXT --env K=V --no-focus` |
| Worktree per card | `herdr worktree create --workspace ID\|--cwd PATH --branch NAME --base REF --json` (+ open/remove/list) |
| Spawn agent | `herdr agent start <name> [--workspace ID] [--tab ID] [--split right\|down] [--env K=V] -- <argv…>` — harness/model/effort/permission go **in argv**, not herdr flags |
| Send prompt to running pane | `herdr agent send <target> <text>` (no Enter) + `herdr pane send-keys <pane> enter`, or `herdr pane run` (text+Enter) |
| Wait for status | `herdr wait agent-status <pane> --status idle\|working\|blocked\|done --timeout MS`; `herdr wait output <pane> --match TEXT --regex` |
| Read output | `herdr agent read <target> --source recent-unwrapped --lines N` (screen-scrape; bounded by scrollback) |
| Event stream | `events.subscribe` (raw socket only, persistent conn): `pane_agent_status_changed` (pane, workspace, agent, status), `pane_exited`, worktree/workspace/tab events. CLI has only blocking one-shot `events.wait` |
| Notify human | `herdr notification show <title> --body … --sound none\|done\|request` |
| Status injection | `herdr pane report-agent <pane> --state idle\|working\|blocked\|unknown --message … --custom-status …` — how integrations push precise status in |

**Agent status**: built-in detection is heuristic (manifests). `herdr integration install claude` installs `~/.claude/hooks/herdr-agent-state.sh` calling `pane report-agent` — precise idle/working/blocked + session refs. **Not currently installed on this machine** — install it as part of herdr-board setup. `idle ≠ finished`: reportable states are idle/working/blocked/unknown, so "done" needs an explicit channel (our `board done`).

**Plugin architecture** (learned from installed `herdr-file-viewer`): manifest `herdr-plugin.toml` with `id/name/version/min_herdr_version`, `[[build]]` (install-time command), `[[panes]]` (id, title, placement=split/tab/overlay, command argv → herdr spawns the TUI in a pane), `[[actions]]` (shell commands, invocable via `herdr plugin action invoke` or `[[keys.command]]` keybindings, receive `PluginInvocationContext`: focused pane/cwd/agent, workspace/tab, selected text). Install from github or local → `~/.config/herdr/plugins/…`, registry `plugins.json`. Runtime env: `HERDR_BIN_PATH`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_CONTEXT_JSON`. Plugins have no special powers — they shell out to the same CLI/socket.

**Gaps to design around**: no per-agent model/effort abstraction (adapter layer is ours); prompt delivery is keystrokes (race with TUI startup → wait for `idle` before `agent send`, or pass the prompt as argv at spawn); result reading is screen-scrape (prefer agents writing files/comments); `events.subscribe` needs a raw socket client.

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

## C. Harness invocation (verified against installed `claude` CLI, 2026-07)

**Claude Code**: `-p/--print` headless; `--output-format text|json|stream-json` (+`--verbose`); `--system-prompt` / **`--append-system-prompt`** (+ `-file` variants); `--model`; **`--effort low|medium|high|xhigh|max`** (first-class flag); `--permission-mode acceptEdits|auto|bypassPermissions|manual|dontAsk|plan`; `--allowedTools`/`--disallowedTools`; **`--session-id <uuid>`** (pre-assign), `--resume <id>`, `--fork-session` (retry without polluting), `--no-session-persistence`; `--max-budget-usd` (print-only); `--json-schema` (structured final output); `--input-format stream-json` (long-lived multi-prompt process); `--bare`; `--bg`; `-n/--name` (label session). Hooks: Stop/StopFailure, SessionStart/SessionEnd — **Stop not fired on silent tool stop** ([#29881](https://github.com/anthropics/claude-code/issues/29881)), don't rely on it alone.

**Adapter shape for later harnesses** = (binary, prompt style, model flag, permission flag, resume mechanism, output parser):
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
