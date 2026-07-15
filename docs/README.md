# herdr-board docs

The reference detail behind the [root README](../README.md). Start here to find the right page.

| Doc | Covers | Read it if you… |
|---|---|---|
| [design.md](design.md) | Architecture, data model, column configuration, the full dispatch → run → transition data flow, pane placement, and the standing design decisions. | want to understand how the board works end to end, or are changing behavior. |
| [protocol.md](protocol.md) | The boardd unix-socket protocol (v1) — transport (NDJSON), auto-start, every method and event, error codes. **The single source of truth** for the daemon⇄client contract; serde types live in `board-core::protocol`. | are writing a client, adding a method, or debugging the wire. |
| [implementation.md](implementation.md) | The cargo workspace crate layout, crate ownership, shared dependencies, key traits (`BoardClient`, `Spawner`), and the build phases with their tests. | are navigating the codebase or picking up a build task. |
| [research.md](research.md) | The verified herdr capability map (commands/events/IDs), prior-art survey of agent-kanban tools, and verified harness invocation flags (claude/codex/gemini/opencode). | are scoping a feature that touches herdr or a new harness, and want the background that grounded the design. |

The [`schema.sql`](../schema.sql) at the repo root is the SQLite schema (migration source of truth).
