//! Unit tests for the launch-plan argv fork detection.

use super::*;

fn argv(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|s| s.to_string()).collect()
}

#[test]
fn opencode_fork_is_the_exact_trailing_session_shape() {
    // `-s <id> --fork` closes the argv: the fork spelling is the trailing
    // three-token session shape, exactly what the adapter appends last.
    assert!(argv_is_fork(
        "opencode",
        &argv(&[
            "opencode",
            "--agent",
            "herdr-board",
            "--auto",
            "-s",
            "ses-1",
            "--fork"
        ])
    ));
    assert!(argv_is_fork(
        "opencode",
        &argv(&["opencode", "-m", "a/b", "--auto", "-s", "ses-1", "--fork"])
    ));
}

#[test]
fn opencode_resume_and_mint_are_not_forks() {
    // Resume closes the argv at `-s <id>` — no `--fork`.
    assert!(!argv_is_fork(
        "opencode",
        &argv(&[
            "opencode",
            "--agent",
            "herdr-board",
            "--auto",
            "-s",
            "ses-1"
        ])
    ));
    // A Mint carries no session flags at all.
    assert!(!argv_is_fork(
        "opencode",
        &argv(&["opencode", "--agent", "herdr-board", "--auto"])
    ));
    assert!(!argv_is_fork("opencode", &argv(&["opencode"])));
}

#[test]
fn opencode_model_literally_spelled_fork_is_not_misclassified() {
    // OpenCode models are free-form, so a no-effort `-m` value could literally
    // be `--fork` (or `-s`) on a Mint; the trailing-window check must not treat
    // it as a fork hop. After the model value a board argv only ever appends
    // `--auto`, never a `-s <id> --fork` tail.
    assert!(!argv_is_fork(
        "opencode",
        &argv(&["opencode", "-m", "--fork"])
    ));
    assert!(!argv_is_fork(
        "opencode",
        &argv(&["opencode", "-m", "--fork", "--auto"])
    ));
    assert!(!argv_is_fork(
        "opencode",
        &argv(&["opencode", "-m", "-s", "--auto"])
    ));
    // A `--fork` that does not close the argv is not the fork spelling.
    assert!(!argv_is_fork(
        "opencode",
        &argv(&["opencode", "-m", "a", "--fork", "--auto"])
    ));
}

#[test]
fn other_harness_fork_spellings_keep_their_shapes() {
    assert!(argv_is_fork(
        "pi",
        &argv(&["pi", "--fork", "source", "--session-id", "t"])
    ));
    assert!(!argv_is_fork("pi", &argv(&["pi", "--session-id", "t"])));
    assert!(argv_is_fork(
        "claude",
        &argv(&["claude", "--resume", "s", "--fork-session"])
    ));
    assert!(!argv_is_fork("claude", &argv(&["claude", "--resume", "s"])));
    assert!(argv_is_fork("codex", &argv(&["codex", "fork", "t"])));
    assert!(!argv_is_fork("codex", &argv(&["codex", "resume", "t"])));
    assert!(!argv_is_fork("unknown", &argv(&["any", "--fork"])));
}
