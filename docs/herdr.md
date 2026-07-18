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

As of herdr 0.7.4 the installable integrations are: **pi, omp, claude, codex,
copilot, devin, droid, kimi, opencode, kilo, hermes, qodercli, cursor,
mastracode** (get the current list from `herdr integration --help`).

Installing one **writes into that harness's own config** (`pi` installs
`~/.pi/agent/extensions/herdr-agent-state.ts`; `claude` installs a hook under
`~/.claude`). Because it mutates personal configuration, herdr-board never installs or
updates it automatically. Pi users should optionally run `herdr integration install pi`
for precise working/blocked/idle status and session references. Spawn, explicit
`board done`, timeout, and pane-exit handling work without it; the idle-lost watchdog
does not arm while status remains `unknown`. The standard E2E uses a fake Pi and tests
watcher status mapping deterministically rather than changing integrations.

## Version drift

This repo's herdr facts — [`docs/research.md`](research.md), [`docs/design.md`](design.md),
and the wire shapes hard-coded in `board-herdr` — were **verified against
herdr 0.7.4 / protocol 16 on 2026-07-17**.

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
