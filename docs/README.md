# herdr-board docs

The reference detail behind the [root README](../README.md). Start here to find the right page.

## Contract at a glance

| Surface | Final version / owner | Canonical source |
|---|---|---|
| Board socket | v1; `board-core::protocol` | [protocol.md](protocol.md) |
| SQLite | schema v11; `schema.sql` + `board-core::db` migrations | [design.md](design.md) |
| Herdr client | 0.7.5 / socket protocol 17; `board-herdr` typed calls | [herdr.md](herdr.md) |
| Runtime launch | daemon-owned `Spawner`, placement, process/pane handles | [implementation.md](implementation.md) |
| Config | typed `RootConfig`, one parse, environment overrides after parse | [design.md](design.md) |
| Live catalog | scenarios 01–21; provider-free fake/safe harness boundary | [e2e/README.md](../e2e/README.md) |

Keep these links as navigation, not duplicate wire definitions: serde types and migrations are the
source of truth. The old worktree API is intentionally absent from `board-herdr`; repository
isolation is an agent prompt concern, not a board space primitive.

| Doc | Covers | Read it if you… |
|---|---|---|
| [design.md](design.md) | Architecture, data model, column configuration, the full dispatch → run → transition data flow, pane placement, and the standing design decisions. | want to understand how the board works end to end, or are changing behavior. |
| [protocol.md](protocol.md) | The boardd unix-socket protocol (v1) — transport (NDJSON), auto-start, every method and event, error codes. **The single source of truth** for the daemon⇄client contract; serde types live in `board-core::protocol`. | are writing a client, adding a method, or debugging the wire. |
| [implementation.md](implementation.md) | The cargo workspace crate layout, crate ownership, shared dependencies, key traits (`BoardClient`, `Spawner`), and the build phases with their tests. | are navigating the codebase or picking up a build task. |
| [research.md](research.md) | The verified herdr capability map (commands/events/IDs), prior-art survey of agent-kanban tools, and verified harness invocation flags (Pi/Claude/codex/gemini/opencode). | are scoping a feature that touches herdr or a new harness, and want the background that grounded the design. |
| [releasing.md](releasing.md) | The release contract: Prepare Release, version bumps, CI-gated tagging/publishing, artifacts, reruns, and tag policy. | are cutting a release or need the repo's release policy. |
| [herdr.md](herdr.md) | How to learn and verify **Herdr** facts (there is no man page): the live sources of truth (`herdr api schema --json`, `herdr <cmd> --help`, `herdr api snapshot`), per-harness agent integrations, and the exact compatibility gate (Herdr 0.7.5 / protocol 17 only; no protocol-16 path). | hit a Herdr command/shape that misbehaves, or need to confirm what the installed Herdr actually does. |
| [testing.md](testing.md) | The testing pyramid in this repo (unit/pure → daemon+CLI integration → TUI snapshots → live E2E), how the provider-free fake Pi/Claude suite works (including protocol-17 scenarios 16/17), and how to write a scenario. The use case ↔ scenario catalog lives in [`../e2e/README.md`](../e2e/README.md). | are adding a feature and need to test it, or are writing/running the live E2E suite. |

The [`schema.sql`](../schema.sql) at the repo root is the fresh SQLite schema; migration behavior
and upgrade tests live in `board-core::db`. Before handoff, check that docs still point to existing
files, that the version matrix above says v11 / v1 / Herdr 0.7.5-protocol 17, and that the scenario
catalog lists every `e2e/NN-*.sh` from 01 through 21. The provider-free static safety gate is:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace --all-features
python3 -m unittest scripts.tests.test_docs
bash e2e/test-harness.sh
```

The full live suite is a separate gate and is intentionally not run by this cleanup task.
