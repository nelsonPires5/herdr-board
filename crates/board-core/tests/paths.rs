//! Pure path parsing (env-reading resolvers stay out of the unit tier).

use board_core::paths::session_name_from_socket;

#[test]
fn session_name_is_read_only_from_a_named_session_socket() {
    assert_eq!(
        session_name_from_socket(Some("/run/user/1000/herdr/sessions/work/herdr.sock")),
        Some("work".to_string())
    );
    // The default session's socket has no `sessions/<name>` segment.
    assert_eq!(
        session_name_from_socket(Some("/run/user/1000/herdr/herdr.sock")),
        None
    );
    // Unset means the default session too.
    assert_eq!(session_name_from_socket(None), None);
}

#[test]
fn malformed_socket_paths_fall_back_to_the_default_session() {
    for path in [
        "",
        "herdr.sock",
        "/run/user/1000/herdr/sessions/herdr.sock",
        "/run/user/1000/herdr/sessions//herdr.sock",
        "/run/user/1000/herdr/session/work/herdr.sock",
        "/run/user/1000/herdr/sessions/work/herdr.socket",
        "/run/user/1000/herdr/sessions/work/",
    ] {
        assert_eq!(
            session_name_from_socket(Some(path)),
            None,
            "expected {path:?} to read as the default session"
        );
    }
}
