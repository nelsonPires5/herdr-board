# herdr-board docs

The reference detail behind the [root README](../README.md). Start here to find the right page.

## Contract at a glance

| Surface | Final version / owner | Canonical source |
|---|---|---|
| Board socket | v1; `board-core::protocol` (additive `active_runs`, error `kind`/`details`) | [protocol.md](protocol.md) |
| SQLite | schema v14; `schema.sql` + `board-core::db` migrations | [design.md](design.md) |
| CLI | canonical nested `board board/card/comment/run/column` taxonomy; `board-cli` wiring | [README CLI reference](../README.md#cli-reference), [skill](../skill/SKILL.md) |
| Herdr client | 0.8.0 / socket protocol 19; `board-herdr` typed calls | [herdr.md](herdr.md) |
| Herdr integrations | Pi v8; Claude v7 (installed and updated by the user) | [herdr.md](herdr.md), [install.md](install.md) |
| Runtime launch | daemon-owned `Spawner`, placement, process/pane handles | [implementation.md](implementation.md) |
| Config | typed `RootConfig`, one parse, environment overrides after parse | [configuration.md](configuration.md), [design.md](design.md) |
| Live catalog | scenarios 01–37; provider-free fake/safe harness boundary | [e2e/README.md](../e2e/README.md) |
| Branches | `dev` integration; `main` production (PR-only, merge-commit, signed); action-owned promotion; tags only from green `main` | [releasing.md](releasing.md) |

Keep these links as navigation, not duplicate wire definitions: serde types and migrations are the
source of truth. The old worktree API is intentionally absent from `board-herdr`; repository
isolation is an agent prompt concern, not a board space primitive.

| Doc | Covers | Read it if you… |
|---|---|---|
| [install.md](install.md) | Installation details beyond the README's one-liner: a custom CLI install directory and its managed-checksum marker, adding a Herdr keybinding, installing the harness integration (Pi/Claude) and the optional agent skill, and how named Herdr sessions register the plugin. | are installing or reinstalling herdr-board, or want the optional integration/keybinding/skill setup. |
| [configuration.md](configuration.md) | `~/.config/herdr-board/config.toml`: top-level and typed `[daemon]` settings, config-defined `[harness.NAME]` adapters (argv placeholders, capability catalogs, `resume`), parse/override precedence, and every environment variable. | are tuning the daemon, adding a config-defined harness, or need what an environment variable does. |
| [operations.md](operations.md) | Day-two operations: updating over an existing plugin (including the graceful `board daemon --stop` handshake), the safe uninstall sequence for the daemon/CLI/plugin, removing board data, and the local-development source install. | are updating, uninstalling, or setting up a checkout you plan to edit. |
| [design.md](design.md) | Architecture, data model, column configuration, the full dispatch → run → transition data flow, pane placement, and the standing design decisions. | want to understand how the board works end to end, or are changing behavior. |
| [protocol.md](protocol.md) | The boardd unix-socket protocol (v1) — transport (NDJSON), auto-start, every method and event, error codes. **The single source of truth** for the daemon⇄client contract; serde types live in `board-core::protocol`. | are writing a client, adding a method, or debugging the wire. |
| [implementation.md](implementation.md) | The cargo workspace crate layout, crate ownership, shared dependencies, key traits (`BoardClient`, `Spawner`), and the build phases with their tests. | are navigating the codebase or picking up a build task. |
| [research.md](research.md) | The verified herdr capability map (commands/events/IDs), prior-art survey of agent-kanban tools, and verified harness invocation flags (Pi/Claude/codex/gemini/opencode). | are scoping a feature that touches herdr or a new harness, and want the background that grounded the design. |
| [releasing.md](releasing.md) | The release contract: the `dev`/`main` branch model (feature → dev, action-owned promotion to main, hotfix, back-merge), Prepare Release, version bumps, CI-gated tagging/publishing, artifacts, reruns, and tag policy. | are cutting a release or need the repo's release policy. |
| [herdr.md](herdr.md) | How to learn and verify **Herdr** facts (there is no man page): the live sources of truth (`herdr api schema --json`, `herdr <cmd> --help`, `herdr api snapshot`), the Herdr 0.8.0/protocol-19 delta, per-harness integrations, and the exact compatibility gate. | hit a Herdr command/shape that misbehaves, or need to confirm what the installed Herdr actually does. |
| [testing.md](testing.md) | The testing pyramid in this repo (unit/pure → daemon+CLI integration → TUI snapshots → live E2E), how the provider-free fake Pi/Claude suite works (including the current pane-first scenarios 16/17, whose filenames are historical), and how to write a scenario. The use case ↔ scenario catalog lives in [`../e2e/README.md`](../e2e/README.md). | are adding a feature and need to test it, or are writing/running the live E2E suite. |
| [sandbox.md](sandbox.md) | The Docker sandbox (`scripts/sandbox.sh`): running the full gate set and every live E2E scenario in an isolated, network-disabled, non-root container from a read-only worktree mount; shell/CLI/TUI use against a container-local Herdr; the explicit real-provider smoke opt-in; artifacts, cache reset, architecture behavior, and troubleshooting. | want a fast edit-test loop without touching the host's active Herdr, board daemon, or sessions. |

The [`schema.sql`](../schema.sql) at the repo root is the fresh SQLite schema; migration behavior
and upgrade tests live in `board-core::db`. Before handoff, check that docs still point to existing
schema v14
the scenario catalog lists every `e2e/NN-*.sh` from 01 through 37.

## Test gates (single source)

This block is the **only** maintained copy of the gate list; `AGENTS.md`, `CONTRIBUTING.md`, and the
root `README.md` link here instead of repeating it. Every command below is also a `run:` step in
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml), and
`scripts/tests/test_docs.py` asserts the two lists match in both directions, so neither can drift.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python3 -m unittest discover -s scripts/tests -p 'test_docs.py'
python3 -m unittest discover -s scripts/tests -p 'test_prepare_release.py'
python3 -m unittest discover -s scripts/tests -p 'test_install_cli.py'
python3 -m unittest discover -s scripts/tests -p 'test_stage_claude_config.py'
python3 -m unittest discover -s scripts/tests -p 'test_sandbox.py'
python3 -m unittest discover -s scripts/tests -p 'test_e2e_*.py'
bash e2e/test-harness.sh
bash e2e/ci.sh
```

CI runs the fast commands as independent `fmt`, `clippy`, `docs`, `scripts`, `e2e-safety`, and
`test` jobs split by what each protects. The dependent `live-e2e` job starts only after all six
succeed, installs the SHA-verified Herdr 0.8.0 binary, and runs the wrapper above. This keeps cheap failures
fast while making the complete live suite part of the same required `CI` workflow. `test_docs.py`
asserts that every `scripts/tests/test_*.py` is matched by a pattern above, so a new module cannot
land in no job at all.

Locally the whole Python tier is one command:

```bash
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
```

`e2e/test-harness.sh` is the provider-free **static** safety gate: it starts no Herdr and needs no
provider, which is why it belongs in CI. It overlaps `test_e2e_safety.py` by design — they are two
implementations of the same checks, which is why they share the `e2e-safety` job.

`e2e/ci.sh` is the CI and local-equivalent live gate. It caches only the exact SHA-verified Herdr
0.8.0 Linux x86_64 binary, verifies socket protocol 19, runs `e2e/run-all.sh --require-all`, and exports
sanitized runner/scenario evidence to `e2e-artifacts/` for the workflow's always-run 30-day upload.
The standard suite remains provider-free and uses only suite-owned ephemeral resources.
