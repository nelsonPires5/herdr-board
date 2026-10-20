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
        dunce::canonicalize(&root).unwrap()
    );
}

#[test]
fn non_git_directory_resolves_to_canonical_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plain");
    std::fs::create_dir(&dir).unwrap();

    assert_eq!(
        resolve_scope_path(&dir).unwrap(),
        dunce::canonicalize(&dir).unwrap()
    );
}

/// Project keys and pane cwds must never carry Windows' `\\?\` verbatim
/// prefix (std's `canonicalize` adds it; Herdr and git never report it).
/// On unix `dunce` is plain `canonicalize`, so this also pins equality with
/// std there.
#[test]
fn resolved_scope_paths_carry_no_verbatim_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let repo = tmp.path().join("repo");
    let nested = repo.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(status.success());

    for (candidate, expected) in [(&plain, &plain), (&nested, &repo)] {
        let resolved = resolve_scope_path(candidate).unwrap();
        let spelled = resolved.to_string_lossy();
        assert!(
            !spelled.starts_with(r"\\?\"),
            "verbatim prefix leaked into {spelled:?}"
        );
        assert!(resolved.is_absolute(), "{spelled:?} must stay absolute");
        #[cfg(unix)]
        assert_eq!(resolved, expected.canonicalize().unwrap());
        #[cfg(not(unix))]
        assert!(
            resolved.ends_with(expected.file_name().unwrap()),
            "{spelled:?} must resolve to {expected:?}"
        );
    }
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
