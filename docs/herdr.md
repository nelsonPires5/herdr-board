# Learning herdr

herdr-board is a client of **herdr** (the terminal workspace manager it dispatches
agent panes into). This page is about learning and **verifying** herdr itself — its
commands, wire shapes, and events — so you never guess. It is not about our own
board CLI (that is [`skill/SKILL.md`](../skill/SKILL.md); see [below](#ours-vs-herdrs)).

There is **no man page** for herdr. Do not rely on memory or on this repo's prose
for a herdr fact you are about to depend on in code — read it live from the
installed binary.

## Live sources of truth

Query the herdr that is actually installed. These are authoritative; docs (this
repo's included) are only a cache.

| Source | What it gives you |
|---|---|
| `herdr api schema --json` | The **full** socket-API spec: every method, its params/result types, every event, and the top-level `protocol` number. This is the contract board-herdr speaks. Pipe to `python3 -m json.tool`/`jq` and search for the method or type in question. `herdr api schema --output PATH` writes it to a file. |
| `herdr <subcommand> --help` | Human-readable usage for a CLI verb and its flags — e.g. `herdr agent --help`, `herdr pane --help`, `herdr workspace --help`, `herdr session --help`. Use it to confirm flag names/spellings before pinning an argv. |
| `herdr api snapshot` | The **live** runtime state (sessions, workspaces, tabs, panes) of the running server — the ground truth for "what is actually open right now" when debugging placement or liveness. |
| `herdr --help` / `herdr status` | Top-level command list and whether a server/client is up. |

Rule of thumb (mirrors [AGENTS.md](../AGENTS.md)): **never assume a herdr
command, flag, or JSON shape from memory — verify against `api schema` /
`--help`, and pin the argv you verified in a test comment.**

## herdr ships its own agent integrations

herdr can install its **own** per-harness integration hooks so a harness reports
live agent status (idle / working / blocked / done) back to herdr. Manage them with
`herdr integration <subcommand>`:

- `herdr integration install <name>` / `herdr integration uninstall <name>`
- `herdr integration status [--outdated-only]`

As of herdr 0.7.5 the installable integrations are: **pi, omp, claude, codex,
copilot, devin, droid, kimi, opencode, kilo, hermes, qodercli, cursor,
mastracode** (get the current list from `herdr integration install --help`). On the
2026-07-22 verification host, `herdr integration status` reported Pi **v6** and
Claude **v7** as current.

Installing one **writes into that harness's own config** (`pi` installs
`~/.pi/agent/extensions/herdr-agent-state.ts`; `claude` installs a hook under
`~/.claude`). Because it mutates personal configuration, **herdr-board never installs or
updates integrations** — running `herdr integration install <harness>` is a **user
prerequisite** for live lifecycle signals.

What the integration buys you:

- **With it**, herdr reports precise `working` / `blocked` / `done` (plus `idle`) per pane, and
  the board maps them to card statuses: `working` → `running`, `blocked` → `blocked`, and
  `done` without `board done` → `awaiting` (reason `agent_done`) for human review.
- **Without it (degraded mode)**, herdr's `working`/`blocked`/`done` signals don't exist. Spawn,
  explicit `board done`, column timeout, and pane-exit handling still work; the only lifecycle
  hint left is herdr's own `idle` status, so `awaiting` can only be reached via `idle_expired`
  (`idle` sustained past `idle_grace_seconds`). If the pane status stays `unknown`, even that
  watchdog never arms and the card simply stays `running` until `board done`, timeout, or pane
  exit.

To verify what a running herdr actually reports, inspect the live state:
`herdr api snapshot` (panes carry their current agent status), plus
`herdr integration status` for which integrations are installed/current.

Pi users who need precise live working/blocked/done status and session references must run
`herdr integration install pi`; the matching integration is a prerequisite for whichever harness
is being dispatched. Without it, the board continues in the degraded mode described above. The
standard E2E uses checked-in fake Pi and Claude executables and tests watcher status mapping
deterministically rather than changing integrations or calling a provider.

## Protocol 17 launch contract

Herdr 0.7.5 uses pane-first managed-agent launch. First create or split an
existing pane with the required cwd and environment; then call `agent.start`
with `{name, kind, pane_id, args, timeout_ms}`. `kind` selects Herdr's canonical
agent executable and `args` contains only that executable's arguments. The old
workspace/tab/split/env placement fields are not part of `agent.start`.

After start, `agent.get <target>` exposes `interactive_ready` and
`launch_pending`. herdr-board waits for `interactive_ready=true` and
`launch_pending=false`, then submits the exact card task with `agent.prompt`
instead of startup argv or synthetic keystrokes. `agent.read` remains a terminal
screen/scrollback read, not a semantic result channel.

Configured harnesses are intentionally unmanaged. Protocol 17 has a
`herdr pane run <PANE_ID> <COMMAND>...` CLI command but no `pane.run` socket
method, so the daemon invokes that CLI against the selected session socket via
a temporary runner script. Agents must still use `board comment` and `board
done`; the configured runner reports a silent child exit back to boardd as a
failed run with no automatic column transition.

## Version drift

This repo's herdr facts — [`docs/research.md`](research.md), [`docs/design.md`](design.md),
and the wire shapes hard-coded in `board-herdr` — were **verified against
Herdr 0.7.5 / protocol 17 on 2026-07-22**.

herdr updates independently of this repo (`herdr update`, stable/preview channels).
When something that used to work misbehaves on a newer herdr — an unknown method, a
changed field name, a new error code — **re-verify against `herdr api schema
--json` FIRST**, before patching board code. Confirm the current `protocol` number
and the exact method/type shape, then reconcile `board-herdr` (and update the
"verified against" note here and in `AGENTS.md`) to match. Editing code against a
remembered shape is how drift bugs get baked in.

## Ours vs herdr's

Two different things document two different tools — keep them straight:

- **`skill/SKILL.md`** documents **our** `board` CLI (cards, columns, comments,
  `board done`/`move`/`cancel`/`retry`, the daemon). It versions **with this repo**
  and changes when we change the board.
- **herdr's integrations and `api schema`** document **herdr itself**. They version
  with the installed herdr, independently of this repo.

When you need a board fact, read `skill/SKILL.md` / `docs/`. When you need a herdr
fact, read it live from herdr per the table above.
