# Release policy

This repo has two workflows:

1. **Prepare Release** is manually dispatched with `patch`, `minor`, or `major`. It runs the
   stdlib-only Python tests, updates the release files, verifies them, runs the Rust gates, and
   creates or updates `release/vX.Y.Z` plus its PR.
2. **Release** is triggered by a completed **CI workflow run whose event was a push to `main`**.
   It checks the exact green run SHA and publishes only when the workspace version changed in
   `Cargo.toml` versus that commit's first parent.

A normal commit is a successful no-op. No live e2e run is part of either workflow.

## Version and verification contract

The bump means:

| choice | result |
|---|---|
| `patch` | `x.y.(z+1)` |
| `minor` | `x.(y+1).0` |
| `major` | `(x+1).0.0` |

The release PR synchronizes:

- root `Cargo.toml` `[workspace.package].version`;
- `herdr-plugin.toml` `version`;
- all five local package entries in `Cargo.lock` (`board-cli`, `board-core`, `board-daemon`,
  `board-herdr`, `board-tui`);
- the `CHANGELOG.md` release section, empty `[Unreleased]`, and matching links.

`scripts/prepare-release.py verify` is the single read-only check for this contract. Prepare runs
it after applying the files, and Release runs it before building with `cargo build --locked`.
The helper uses only Python's standard library.

## Prepare Release workflow

A maintainer manually starts **Prepare Release** and selects the bump. It computes the target from
`Cargo.toml`, reuses the same branch/PR on reruns, applies the four release files atomically one
file at a time, runs Python/Rust tests, then explicitly dispatches CI for the branch. GitHub
credentials are disabled for checkout and supplied only to steps that need GitHub API/git access.

The PR must be reviewed and merged into `main`. Dispatching CI on the branch is useful proof, but
it does not authorize publication.

## Release gate

The Release workflow consumes only a successful CI `workflow_run` satisfying all of these:

- `workflow_run.event == push` and `head_branch == main`;
- `Cargo.toml` version at `head_sha` differs from the version at `head_sha^1`.

It checks out `head_sha`, verifies the release files, installs stable Rust with the same cache
used by CI, builds with `--locked`, and creates the tag at that exact SHA. A per-CI-commit concurrency lock serializes retries for the same recovery run without dropping a pending release when later main commits complete CI.

## Recovery and reruns

Release state is inspected before mutation:

- the tag must be absent or point to the exact CI `head_sha`; a tag at another SHA is a hard
  error and is never moved;
- a GitHub Release is checked for draft status and both exact asset names;
- an existing release with no tag fails closed. The workflow never recreates a missing tag from
  the current CI run;
- a missing release is created as a **draft** after the tag exists;
- existing drafts are reused;
- both assets are uploaded with `gh release upload --clobber`, then the draft is published;
- the only no-op is a release that is already published and has both expected assets.

Therefore a failure after tag creation, draft creation, or one asset upload can be recovered by
rerunning the same green `workflow_run`; the per-CI-commit lock serializes retries for that commit. A release with a
missing tag must be repaired manually and then rerun.

Expected assets:

- `board-X.Y.Z-x86_64-unknown-linux-gnu.tar.gz`
- `board-X.Y.Z-x86_64-unknown-linux-gnu.tar.gz.sha256`

The tarball contains the release binary, `herdr-plugin.toml`, `skill/`, packaging scripts,
`README.md`, and any license file present at build time.

## Tag policy

Version tags are owned by the release flow. Maintainers and agents must not create, push, move, or
delete `v*` tags manually. A maintainer starts **Prepare Release** and merges its PR; after the
resulting `main` CI succeeds, the **Release** workflow creates the tag at that exact green SHA.
If a tag points elsewhere, stop and repair the process rather than retargeting it.

This policy is currently enforced by convention and workflow validation: **no tag ruleset has been
configured yet**. Because only the repository owner currently has write access, leaving the ruleset
disabled is an accepted temporary choice. A tag ruleset or dedicated release identity can be added
later as defense in depth; until then, do not create release tags from a local checkout or the GitHub
UI.
