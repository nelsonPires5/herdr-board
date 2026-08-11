//! Daemon-owned display labels for card optionals.
//!
//! Protocol option (a): the daemon stamps ready display strings onto the
//! read payloads it serves, and the clients (TUI + CLI) render them verbatim
//! — no fallback strings live in the clients. The wire fields (`session`,
//! `effort`, …) keep their `None`-means-default semantics for round-trips;
//! these labels are read-only display data.
//!
//! The session label is the only one that depends on live herdr state: the
//! daemon resolves `None` through its session registry (the session matching
//! its bound socket, else the synthetic name `"default"`) and falls back to
//! the `default session` marker when nothing resolves. Effort, permission and
//! model labels derive from the card alone.

use crate::model::Card;
use crate::protocol::CardLabels;

/// Marker label for a card whose session is unset and could not be resolved.
pub fn default_session_label() -> &'static str {
    "default session"
}

/// Marker label for a card with no effort override (harness default).
pub fn default_effort_label() -> &'static str {
    "default effort"
}

/// Marker label for a card with no permission override (harness default).
pub fn default_permission_label() -> &'static str {
    "default permission"
}

/// Marker label for a card with no model override (harness default).
pub fn default_model_label() -> &'static str {
    "default model"
}

/// Human label for a permission-mode wire id. Codex's stable wire ids get the
/// same labels as its `/permissions` picker; config-defined modes stay
/// verbatim. Shared by the daemon (label stamping) and the TUI (form option
/// labels) so the two can never drift.
pub fn permission_label(mode: &str) -> String {
    match mode {
        "ask-for-approval" => "Ask for approval".to_string(),
        "approve-for-me" => "Approve for me".to_string(),
        "full-access" => "Full access".to_string(),
        other => other.to_string(),
    }
}

/// Display label for a card's `session`: the explicit name, else the resolved
/// default-session name, else the `default session` marker.
pub fn session_label(session: Option<&str>, resolved_default_session: Option<&str>) -> String {
    match session {
        Some(name) => name.to_string(),
        None => resolved_default_session
            .map(str::to_string)
            .unwrap_or_else(|| default_session_label().to_string()),
    }
}

/// Build the full display-label set for a card. `resolved_default_session` is
/// the daemon's resolution of the unset session (herdr's session matching the
/// daemon's bound socket, normally named `default`); `None` yields the marker.
pub fn card_labels(card: &Card, resolved_default_session: Option<&str>) -> CardLabels {
    CardLabels {
        session: session_label(card.session.as_deref(), resolved_default_session),
        effort: card
            .effort
            .map(|e| e.as_str().to_string())
            .unwrap_or_else(|| default_effort_label().to_string()),
        permission: card
            .permission_mode
            .as_deref()
            .map(permission_label)
            .unwrap_or_else(|| default_permission_label().to_string()),
        model: card
            .model
            .clone()
            .unwrap_or_else(|| default_model_label().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Effort;

    fn card_with(session: Option<&str>, effort: Option<Effort>) -> Card {
        Card {
            id: 1,
            board_id: 1,
            column_id: 1,
            position: 0,
            title: "t".to_string(),
            description: String::new(),
            harness: "pi".to_string(),
            model: None,
            effort,
            permission_mode: None,
            session: session.map(str::to_string),
            space_kind: crate::protocol::SpaceKind::Workspace,
            space_ref: None,
            space_cwd: None,
            status: crate::protocol::CardStatus::Idle,
            awaiting_reason: None,
            session_id: None,
            created_at: String::new(),
            updated_at: String::new(),
            archived_at: None,
            labels: CardLabels::default(),
        }
    }

    #[test]
    fn explicit_session_label_is_the_name_verbatim() {
        let card = card_with(Some("feature"), None);
        assert_eq!(card_labels(&card, None).session, "feature");
        // The resolved default never shadows an explicit name.
        assert_eq!(card_labels(&card, Some("default")).session, "feature");
    }

    #[test]
    fn unset_session_uses_resolved_default_then_marker() {
        let card = card_with(None, None);
        assert_eq!(card_labels(&card, Some("default")).session, "default");
        assert_eq!(card_labels(&card, Some("other")).session, "other");
        assert_eq!(card_labels(&card, None).session, "default session");
    }

    #[test]
    fn effort_permission_model_markers() {
        let card = card_with(None, None);
        let labels = card_labels(&card, None);
        assert_eq!(labels.effort, "default effort");
        assert_eq!(labels.permission, "default permission");
        assert_eq!(labels.model, "default model");

        let card = Card {
            model: Some("opus".to_string()),
            effort: Some(Effort::High),
            permission_mode: Some("ask-for-approval".to_string()),
            ..card_with(None, None)
        };
        let labels = card_labels(&card, None);
        assert_eq!(labels.model, "opus");
        assert_eq!(labels.effort, "high");
        assert_eq!(labels.permission, "Ask for approval");
    }

    #[test]
    fn permission_labels_cover_codex_modes_verbatim_otherwise() {
        assert_eq!(permission_label("ask-for-approval"), "Ask for approval");
        assert_eq!(permission_label("approve-for-me"), "Approve for me");
        assert_eq!(permission_label("full-access"), "Full access");
        assert_eq!(permission_label("acceptEdits"), "acceptEdits");
    }
}
