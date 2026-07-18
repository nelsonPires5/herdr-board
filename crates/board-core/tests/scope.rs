use std::path::{Path, PathBuf};

use board_core::scope::{resolve_scope_path, select_scope_candidate};

#[test]
fn candidate_precedence_uses_override_then_focused_then_workspace_then_cwd() {
    let cwd = Path::new("/current");
    let context = r#"{"focused_pane_cwd":"/focused","workspace_cwd":"/workspace"}"#;

    assert_eq!(
        select_scope_candidate(Some("/override"), Some(context), cwd).unwrap(),
        PathBuf::from("/override")
    );
    assert_eq!(
        select_scope_candidate(Some("  "), Some(context), cwd).unwrap(),
        PathBuf::from("/focused")
    );
    assert_eq!(
        select_scope_candidate(None, Some(r#"{"workspace_cwd":"/workspace"}"#), cwd).unwrap(),
        PathBuf::from("/workspace")
    );
    assert_eq!(select_scope_candidate(None, Some("{}"), cwd).unwrap(), cwd);
}

#[test]
fn malformed_plugin_context_falls_back_to_cwd() {
    let cwd = Path::new("/current");
    assert_eq!(
        select_scope_candidate(None, Some("not json"), cwd).unwrap(),
        cwd
    );
}

#[test]
fn git_subdirectory_resolves_to_canonical_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let subdir = root.join("nested/deep");
    std::fs::create_dir_all(&subdir).unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(
        resolve_scope_path(&subdir).unwrap(),
        root.canonicalize().unwrap()
    );
}

#[test]
fn non_git_directory_resolves_to_canonical_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plain");
    std::fs::create_dir(&dir).unwrap();

    assert_eq!(
        resolve_scope_path(&dir).unwrap(),
        dir.canonicalize().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn fallback_and_git_root_are_canonicalized() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    assert_eq!(
        resolve_scope_path(&link).unwrap(),
        real.canonicalize().unwrap()
    );
}
