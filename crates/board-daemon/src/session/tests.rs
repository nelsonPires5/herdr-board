use super::*;

const SAMPLE: &str = r#"{"sessions":[
      {"default":true,"name":"default","running":true,"session_dir":"/d",
       "socket_path":"/home/np/.config/herdr/herdr.sock"},
      {"default":false,"name":"new","running":true,"session_dir":"/d/sessions/new",
       "socket_path":"/d/sessions/new/herdr.sock"},
      {"default":false,"name":"stopped","running":false,"session_dir":"/d/sessions/stopped",
       "socket_path":"/d/sessions/stopped/herdr.sock"}
    ]}"#;

fn registry() -> SessionRegistry {
    let reg = SessionRegistry::new(PathBuf::from("/home/np/.config/herdr/herdr.sock"));
    *reg.cache.lock().unwrap() = Some((Instant::now(), parse_session_list(SAMPLE).unwrap()));
    reg
}

#[test]
fn parses_captured_session_list() {
    let entries = parse_session_list(SAMPLE).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].name, "default");
    assert!(entries[0].default && entries[0].running);
    assert_eq!(entries[1].socket_path, "/d/sessions/new/herdr.sock");
    assert!(!entries[2].running);
}

#[test]
fn resolve_none_matches_default_socket_name() {
    let r = registry().resolve(None).unwrap();
    assert_eq!(r.name, "default");
    assert_eq!(r.socket, PathBuf::from("/home/np/.config/herdr/herdr.sock"));
}

#[test]
fn resolve_none_synthesizes_default_when_no_match() {
    let reg = SessionRegistry::new(PathBuf::from("/nowhere/herdr.sock"));
    *reg.cache.lock().unwrap() = Some((Instant::now(), parse_session_list(SAMPLE).unwrap()));
    let r = reg.resolve(None).unwrap();
    assert_eq!(r.name, "default");
    assert_eq!(r.socket, PathBuf::from("/nowhere/herdr.sock"));
}

#[test]
fn resolve_named_running_session() {
    let r = registry().resolve(Some("new")).unwrap();
    assert_eq!(r.name, "new");
    assert_eq!(r.socket, PathBuf::from("/d/sessions/new/herdr.sock"));
}

#[test]
fn resolve_unknown_session_errors_with_known() {
    let err = registry().resolve(Some("ghost")).unwrap_err().to_string();
    assert!(err.contains("ghost"));
    assert!(err.contains("default"));
    assert!(err.contains("new"));
}

#[test]
fn resolve_stopped_session_errors() {
    let err = registry().resolve(Some("stopped")).unwrap_err().to_string();
    assert!(err.contains("not running"));
}

#[test]
fn session_infos_maps_shape() {
    let infos = registry().session_infos().unwrap();
    assert_eq!(infos.len(), 3);
    assert_eq!(infos[0].name, "default");
    assert!(infos[0].default);
}

/// A stand-in `herdr` binary in a short-path tempdir (AF_UNIX-safe habit, and
/// `argv[1..]` is always `session list --json`, which the script ignores).
fn fake_herdr_bin(body: &str) -> (tempfile::TempDir, String) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr");
    let mut file = std::fs::File::create(&path).unwrap();
    write!(file, "#!/bin/sh\n{body}\n").unwrap();
    file.flush().unwrap();
    drop(file);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let bin = path.to_string_lossy().into_owned();
    (dir, bin)
}

fn registry_with_bin(bin: String, fetch_timeout: Duration) -> SessionRegistry {
    SessionRegistry {
        herdr_bin: bin,
        default_socket: PathBuf::from("/nowhere/herdr.sock"),
        ttl: Duration::from_secs(3),
        fetch_timeout,
        cache: Mutex::new(None),
    }
}

#[test]
fn fetch_kills_a_hung_herdr_and_reports_the_deadline() {
    // `sleep 60` stands in for a wedged herdr: without a deadline this pins a
    // blocking-pool thread forever. The assertion is that `list` returns within
    // a small multiple of the budget, not after the child's own 60s.
    let (_dir, bin) = fake_herdr_bin("sleep 60");
    let reg = registry_with_bin(bin, Duration::from_millis(150));
    let started = Instant::now();
    let error = reg.list().unwrap_err().to_string();
    assert!(started.elapsed() < Duration::from_secs(5), "{error}");
    assert!(error.contains("timed out"), "{error}");
    assert!(error.contains("killed"), "{error}");
}

#[test]
fn fetch_reads_a_prompt_reply_within_the_deadline() {
    let (_dir, bin) = fake_herdr_bin(&format!("cat <<'EOF'\n{SAMPLE}\nEOF"));
    let reg = registry_with_bin(bin, Duration::from_secs(10));
    let entries = reg.list().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].name, "default");
}

#[test]
fn fetch_surfaces_a_nonzero_exit_with_stderr() {
    let (_dir, bin) = fake_herdr_bin("echo 'no session dir' >&2; exit 3");
    let reg = registry_with_bin(bin, Duration::from_secs(10));
    let error = reg.list().unwrap_err().to_string();
    assert!(error.contains("no session dir"), "{error}");
}
