# herdr-board docs

The reference detail behind the [root README](../README.md). Start here to find the right page.

| Doc | Covers | Read it if you… |
|---|---|---|
| [design.md](design.md) | Architecture, data model, column configuration, the full dispatch → run → transition data flow, pane placement, and the standing design decisions. | want to understand how the board works end to end, or are changing behavior. |
| [protocol.md](protocol.md) | The boardd unix-socket protocol (v1) — transport (NDJSON), auto-start, every method and event, error codes. **The single source of truth** for the daemon⇄client contract; serde types live in `board-core::protocol`. | are writing a client, adding a method, or debugging the wire. |
| [implementation.md](implementation.md) | The cargo workspace crate layout, crate ownership, shared dependencies, key traits (`BoardClient`, `Spawner`), and the build phases with their tests. | are navigating the codebase or picking up a build task. |
| [research.md](research.md) | The verified herdr capability map (commands/events/IDs), prior-art survey of agent-kanban tools, and verified harness invocation flags (Pi/Claude/codex/gemini/opencode). | are scoping a feature that touches herdr or a new harness, and want the background that grounded the design. |
| [releasing.md](releasing.md) | The release contract: Prepare Release, version bumps, CI-gated tagging/publishing, artifacts, reruns, and tag policy. | are cutting a release or need the repo's release policy. |
| [herdr.md](herdr.md) | How to learn and verify **herdr** facts (there is no man page): the live sources of truth (`herdr api schema --json`, `herdr <cmd> --help`, `herdr api snapshot`), herdr's own per-harness agent integrations, and the version-drift note (facts pinned to herdr 0.7.3 / protocol 16). | hit a herdr command/shape that misbehaves, or need to confirm what the installed herdr actually does. |
| [testing.md](testing.md) | The testing pyramid in this repo (unit/pure → daemon+CLI integration → TUI snapshots → live e2e scenarios), how the live e2e harness (`e2e/`) works, and how to write a new scenario. The live suite's use case ↔ scenario catalog lives in [`../e2e/README.md`](../e2e/README.md). | are adding a feature and need to test it, or are writing/running the live e2e suite. |

The [`schema.sql`](../schema.sql) at the repo root is the SQLite schema (migration source of truth).
