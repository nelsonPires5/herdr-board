# Release policy

This repo has two long-lived branches and three workflows:

| Branch | Role |
|---|---|
| `dev` | long-lived integration branch; feature and release branches open PRs here |
| `main` | production, the default branch, and the **only** branch the Release workflow publishes from |

1. **Prepare Release** is manually dispatched with `patch`, `minor`, or `major`, plus a `base`
   (default `dev`; `main` only for hotfixes). It runs the stdlib-only Python tests, updates the
   release files, verifies them, runs the Rust gates, and creates or updates `release/vX.Y.Z`
   plus its PR against the selected base.
2. **Release** is triggered by a completed **CI workflow run whose event was a push to `main`**.
   It checks the exact green run SHA and publishes only when the workspace version changed in
   `Cargo.toml` versus that commit's first parent.

A normal commit is a successful no-op. The unchanged Release workflow still consumes the existing
`CI` result; that result now includes the provider-free live E2E job before publication can begin.

## Branch model

| Branch | Opens PRs to | Notes |
|---|---|---|
| feature | `dev` | same required checks as `main` |
| `release/vX.Y.Z` | `dev` (default) or `main` (hotfix) | created/updated by Prepare Release |
| `dev -> main` promotion | `main` | merge commit; the only way a normal release reaches `main` |
| `main -> dev` back-merge | `dev` | after promotion, so `dev` keeps the merge history |

Normal release flow:

1. A maintainer starts **Prepare Release** with the bump; the default base is `dev`. The workflow
   cuts `release/vX.Y.Z` from `dev` and opens (or updates) its PR to `dev`.
2. The PR is reviewed and merged into `dev` with a merge commit. CI runs on that `dev` push —
   the full workflow including the dependent live E2E job — but never publishes.
3. A maintainer opens a `dev -> main` promotion PR (merge commit). CI runs on the merge into
   `main`.
4. The **Release** workflow consumes the green `main` CI run, sees the version bump versus the
   merge's first parent, and creates the tag at that exact promotion SHA. The tag therefore lands
   on `main` and the release commit at the same time.
5. `main` is merged back into `dev` so `dev` retains the promotion commit and stays able to serve
   as the base of the next release.

Hotfix flow: run Prepare Release with `base = main`. The PR targets `main` directly; after the
merge, Release tags the green main commit as usual. **Always back-merge `main -> dev` after a
hotfix** before the next normal release, or the next promotion will try to re-land the hotfix
bump and confuse the version comparison.

Recovery: if the CI of a promotion merge fails, rerun that CI run; Release re-evaluates the same
green SHA. Any other commit on `main` without a version bump is a no-op.

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

A maintainer manually starts **Prepare Release**, selects the bump, and picks the base (`dev` by
default, `main` for hotfixes). The workflow computes the target from `Cargo.toml`, cuts
`release/vX.Y.Z` from the selected base, reuses the same branch/PR on reruns (retargeting the PR
to the current base if it changed), applies the four release files atomically one file at a time,
runs Python/Rust tests, then explicitly dispatches CI for the branch. GitHub credentials are
disabled for checkout and supplied only to steps that need GitHub API/git access.

The PR must be reviewed and merged into its base. Dispatching CI on the branch is useful proof,
but it does not authorize publication.

## Promotion to main (action-owned)

For a normal release the release PR merges into `dev`, not `main`. The bump reaches `main` only
through the **Promote** workflow, which opens the `dev -> main` PR, waits for its required checks,
and merges it with a merge commit. CI runs on the merge, and Release creates the tag at that exact
SHA — "tag and main at the same time" means the tag points to the promotion commit. The tag is
still created only after CI is green; there is no manual or pre-CI tagging anywhere in the flow.
A commit on `main` that does not bump the version (back-merge, documentation, other branches'
merges) is a no-op.

## main protection

`main` is protected by an active branch ruleset: every change must come through a pull request
merged with a **merge commit** (squash/rebase are not allowed), the six fast CI jobs are required
status checks with a strict policy, and **signed commits** are required — merge commits created
by GitHub for a PR merge are GitHub-verified, so every commit on `main` carries a verified
signature. Direct pushes to `main` are blocked by the pull-request rule; the only writer in
practice is the automation (`github-actions[bot]`), whose promotion and hotfix merges are
GitHub-verified. `dev` is protected the same way (PR + merge commit + required checks) but has no
signature requirement.

## Release gate

The Release workflow consumes only a successful CI `workflow_run`—including its dependent live E2E
job—satisfying all of these:

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

Promotion failures recover the same way: if the promotion PR's checks fail or its merge is
blocked, rerun the green `dev` CI run — the Promote workflow re-evaluates the same SHA under its
per-SHA lock, updates the PR, and retries the merge. If a promotion already landed, a rerun is a
no-op (the dev tip is an ancestor of `main`).

Expected assets:

- `board-X.Y.Z-x86_64-unknown-linux-gnu.tar.gz`
- `board-X.Y.Z-x86_64-unknown-linux-gnu.tar.gz.sha256`

The tarball contains the release binary, `herdr-plugin.toml`, `skill/`, packaging scripts,
`README.md`, and any license file present at build time.

## Tag policy

Version tags are owned by the release flow. Maintainers and agents must not create, push, move, or
delete `v*` tags manually. A maintainer starts **Prepare Release** and merges its PRs; after the
promotion's `main` CI succeeds, the **Release** workflow creates the tag at that exact green SHA.
If a tag points elsewhere, stop and repair the process rather than retargeting it.

This policy is enforced by workflow validation and by a repository tag ruleset protecting `v*`
against deletion and non-fast-forward updates — the Release workflow's own tag creation is the
only path that may touch them. Defense in depth: even with the ruleset, never create release tags
from a local checkout or the GitHub UI.
