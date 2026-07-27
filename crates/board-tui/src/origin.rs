//! The invoking Herdr/plugin context, read once at the composition root.

/// Explicit context supplied by the composition root to the TUI.
///
/// Test and embedded drivers use [`Default::default`] so ambient Herdr/plugin
/// variables cannot affect their state. Only [`crate::Driver::new`] and
/// [`crate::run_with_board`] construct this from the process environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OriginContext {
    pub origin_socket: Option<String>,
    pub session: Option<String>,
    pub plugin_id: Option<String>,
    pub pane_id: Option<String>,
}

impl OriginContext {
    /// Read the invoking Herdr/plugin context at the production boundary.
    pub fn from_environment() -> OriginContext {
        let origin_socket = std::env::var("HERDR_SOCKET_PATH")
            .ok()
            .filter(|socket| !socket.is_empty());
        OriginContext {
            session: board_core::paths::session_name_from_socket(origin_socket.as_deref()),
            origin_socket,
            plugin_id: std::env::var("HERDR_PLUGIN_ID")
                .ok()
                .filter(|value| !value.is_empty()),
            pane_id: std::env::var("HERDR_PANE_ID")
                .ok()
                .filter(|value| !value.is_empty()),
        }
    }
}
