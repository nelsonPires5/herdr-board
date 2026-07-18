# Prototype: shared harness metadata for Card + Column forms

Branch: `proto/harness-meta-forms` (worktree `worktree/harness-meta-prototype/`).

## What the problem was

The card-create/edit and column-config forms duplicated harness-metadata logic
(models / efforts / permissions), the column's `harness_override` was a free-text
box, and `permission_override` was shown even for harnesses that have no
permission modes (Pi).

## What was prototyped

### 1. `HarnessMeta` trait (board-core, the single adapter contract)
`crates/board-core/src/capability.rs` now defines:

```rust
pub trait HarnessMeta {
    fn id(&self) -> &str;
    fn models(&self) -> Vec<ModelInfo>;
    fn efforts(&self, model: Option<&str>) -> Vec<Effort>; // None = default/free-form set
    fn permissions(&self) -> Vec<String>;                   // empty = none (Pi)
    fn model_freeform(&self) -> bool;
}
```

Implementations: `Pi`, `Claude` (ZSTs) and `ConfigHarness` (owns a cloned
`HarnessDef`). Resolver `meta_for(harness, config) -> Option<Box<dyn HarnessMeta>>`.
The existing wire DTO `HarnessCapabilities` is retained (it is the serialized
`harness.capabilities` result and is referenced by `docs/protocol.md` + tests) and
is now built via `HarnessCapabilities::from_meta(&dyn HarnessMeta)`. The old
`capabilities_for` / `pi_capabilities` / `claude_capabilities` now delegate to the
trait, so all existing tests still pass.

### 2. `harness.list` RPC (so config-defined harnesses are selectable)
- `board-core::protocol::HarnessListResult { harnesses: Vec<String> }`.
- `board-core::capability::available_harnesses(config)` → built-ins + config keys,
  sorted + de-duped.
- Daemon handler `harness.list` (`crates/board-daemon/src/ops.rs`); the
  "unknown harness" error message reuses the same helper.
- TUI `fetch_harness_list` + testkit stub.

### 3. Shared form logic (board-tui, no more duplication)
`crates/board-tui/src/forms.rs` extracts three shared builders used by **both** the
card and the column field builders:

- `effort_choice_opts(efforts, current)` — `(default)` + each effort.
- `permission_choice_opts(modes, current)` — `(default)` + each mode.
- `harness_override_opts(harnesses, current)` — `(none)` + each harness.

`Form` gained `harnesses: Vec<String>` and `columns: Vec<Column>` (sibling columns,
retained so rebuilds regenerate on-success/on-fail options).

### 4. Column-config fixes (the explicit asks)
- `harness_override` is now a **select** (`(none)` + pi/claude + every config-defined
  harness from `harness.list`), not free text. Unknown existing values are preserved
  by appending.
- `permission_override` is **hidden** when the driving harness has no permission
  modes. The "driving harness" for a column is its `harness_override` (or `pi` when
  `(none)`); `Form::current_harness()` is now form-kind aware (card → `Harness`,
  column → `HarnessOverride`).
- `effort_override` is now populated from the override harness's catalog (was a
  hardcoded `low/medium/high/xhigh/max` list).
- Cycling the `harness_override` emits `LoadFormOptions` (refetch caps); the rebuild
  keeps still-valid overrides and resets an effort/permission that the new harness
  doesn't offer back to `(default)`.

### 5. State management
Switching harness (card) or harness-override (column) refetches caps and rebuilds
only the dependent selectors; compatible selections survive, invalid ones reset to
the default option (the builders do `unwrap_or(0)`).

## Tests added
- board-core: trait resolves pi/claude/config; `efforts(Some(id))` authoritative,
  `efforts(None)` = default; `available_harnesses` sorts; wire snapshot == trait.
- daemon: `harness.list` returns built-ins only / built-ins + config-defined.
- board-tui: column `harness_override` is a select with builtins + config-defined;
  `permission_override` hidden for pi / shown for claude with catalog modes;
  `effort_override` follows the catalog; cascading drops a stale effort on harness
  change; `(none)` override extracts to `None`.
- Snapshot `column_form` updated to reflect the new select + hidden permission row.

## Build gates (all green)
```
cargo test --workspace --all-features        # +17 new tests, all pass
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

## Key decisions & tradeoffs

1. **Trait vs struct.** The card asked for a `HarnessMeta` trait. The repo already
   had a `HarnessCapabilities` struct produced by free functions, which alone is a
   uniform interface. I introduced the trait **on the daemon/adapter side** as the
   single source of truth that *produces* the wire struct — this satisfies the card
   and keeps exactly one wire representation. If the team prefers the lighter
   weight, the trait can be dropped and `capabilities_for` kept as the only seam;
   the TUI/CLI are unaffected either way (they only see the DTO).

2. **`efforts(model: Option<&str>)`.** The card listed `efforts(model)`. I take
   `Option<&str>` so one method covers both "this model's efforts" and "the default
   / free-form effort set" (`efforts(None)` maps to `default_efforts` on the wire).
   This avoids a separate `default_efforts()` trait method.

3. **`model_override` stays free text.** The card only mandated `harness_override`
   → select and `permission_override` → hidden. Since every harness is
   `model_freeform`, a model-override select would add noise without adding safety.
   Effort/permission overrides *are* selects (they have finite, harness-specific
   menus). Revisit if a harness ever gains a closed model set.

4. **Card `harness` select still uses builtins + preserve-unknown**, not
   `harness.list`. Threading `harnesses` into `build_card_fields` would also include
   config-defined harnesses there (an improvement), but `available_harnesses` is
   **sorted** (`claude, pi`) while the card currently shows `pi` first — adopting
   the list would reorder the card harness select and churn card snapshots. Left as
   a follow-up; the column fix (the actual ask) is done.

5. **`permission_override` visibility when caps aren't loaded.** Falls back to
   `current_harness() != "pi"` (matches the card form's existing heuristic), so the
   field is reachable for claude/config harnesses before the fetch lands and hidden
   for pi.

## What needs to change for production

- **Docs (`docs/protocol.md`)**: document the new `harness.list` method + result
  shape; note `HarnessCapabilities` is now `from_meta`. Update `docs/design.md` if
  it references the catalog functions.
- **`CHANGELOG.md`**: entry under board-core (trait + `harness.list`) and board-tui
  (column harness_override select, hidden permission_override).
- **e2e scenario** under `e2e/`: a column-config flow that sets `harness_override`
  to a config-defined harness and verifies `permission_override` hides for pi.
  Required by the repo's "new herdr-touching flow ⇒ e2e" policy — not added in the
  prototype (no live herdr here).
- **Consider** a `board harness list` CLI subcommand mirroring `harness models`
  (optional; the RPC exists either way).
- **Snapshot review**: `column_form` was re-accepted; confirm the `(default)` labels
  for effort/permission override are the desired wording (was `none`).
- Decide on the trait-vs-struct question (decision #1) before merging; if dropping
  the trait, `meta_for`/`from_meta` collapse back into `capabilities_for`.
