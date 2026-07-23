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
