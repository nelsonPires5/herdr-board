//! The structured spawn failure.
//!
//! A spawn can fail because the Herdr transport did not answer (a deadline or a
//! dropped connection) or because Herdr answered and *refused* (a protocol
//! error such as "workspace not found"). Dispatch used to flatten both into one
//! string, so it could no longer tell a transient transport failure from a
//! permanent rejection.
//!
//! The spawner already keeps the typed [`board_herdr::HerdrError`] alive inside
//! its `anyhow` chain (see `spawner::placement::race`), so classification is a
//! chain walk at the boundary rather than new plumbing through every call site.
//!
//! **This type carries no policy.** Every variant still ends its run exactly as
//! before; the discriminant exists so dispatch can log it and so a future
//! requeue-on-transport-error has something to branch on.

use board_herdr::HerdrError;

/// Why a [`Spawner::spawn`](super::Spawner::spawn) call failed.
///
/// `Display` is deliberately transparent — it renders the underlying `anyhow`
/// chain with `{:#}` — so the run summary and system comment persisted by
/// dispatch are byte-identical to the pre-classification text.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// A bounded Herdr socket operation ran out of time. The request may or may
    /// not have been applied.
    #[error("{0:#}")]
    Deadline(anyhow::Error),

    /// Herdr answered with an `error` envelope, i.e. it understood the request
    /// and refused it.
    #[error("{0:#}")]
    Protocol(anyhow::Error),

    /// The Herdr connection dropped, or socket I/O failed outright.
    #[error("{0:#}")]
    Transport(anyhow::Error),

    /// Anything not attributable to the Herdr transport: local process spawn,
    /// placement geometry, argv materialization, a malformed reply.
    #[error("{0:#}")]
    Other(anyhow::Error),
}

impl SpawnError {
    /// A short, stable label for logs and metrics.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            SpawnError::Deadline(_) => "deadline",
            SpawnError::Protocol(_) => "protocol",
            SpawnError::Transport(_) => "transport",
            SpawnError::Other(_) => "other",
        }
    }

    /// Whether the failure is, in principle, worth retrying: the launch never
    /// reached a Herdr decision.
    ///
    /// Nothing acts on this yet — dispatch ends the run either way. It is the
    /// hook a follow-up requeue policy would use.
    pub(crate) fn retriable(&self) -> bool {
        matches!(self, SpawnError::Deadline(_) | SpawnError::Transport(_))
    }
}

impl From<anyhow::Error> for SpawnError {
    fn from(error: anyhow::Error) -> Self {
        // `downcast_ref` walks the whole context chain, which is why the
        // spawner's `anyhow::Error::new(herdr_error).context(..)` style matters:
        // a stringified `anyhow!("...: {e}")` would arrive here as `Other`.
        match error.downcast_ref::<HerdrError>() {
            Some(HerdrError::Deadline { .. }) => SpawnError::Deadline(error),
            Some(HerdrError::Protocol { .. }) => SpawnError::Protocol(error),
            Some(HerdrError::Io(_)) | Some(HerdrError::Disconnected) => {
                SpawnError::Transport(error)
            }
            Some(HerdrError::Decode(_)) | None => SpawnError::Other(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    fn protocol_error() -> HerdrError {
        HerdrError::Protocol {
            code: "workspace_not_found".into(),
            message: "no such workspace".into(),
        }
    }

    #[test]
    fn classifies_a_herdr_error_buried_under_context() {
        let error = anyhow::Error::new(protocol_error())
            .context("placing pane in tab 'card-1' for card-1-execute");
        let spawn_error = SpawnError::from(error);
        assert_eq!(spawn_error.label(), "protocol");
        assert!(!spawn_error.retriable());
    }

    #[test]
    fn classifies_transport_failures_as_retriable() {
        for (error, label) in [
            (
                HerdrError::Deadline {
                    operation: "handshake",
                },
                "deadline",
            ),
            (HerdrError::Disconnected, "transport"),
        ] {
            let spawn_error =
                SpawnError::from(anyhow::Error::new(error).context("herdr agent.start"));
            assert_eq!(spawn_error.label(), label);
            assert!(spawn_error.retriable(), "{label} should be retriable");
        }
    }

    #[test]
    fn a_plain_error_is_other_and_terminal() {
        let spawn_error = SpawnError::from(anyhow::anyhow!("empty argv"));
        assert_eq!(spawn_error.label(), "other");
        assert!(!spawn_error.retriable());
    }

    /// The persisted run summary must not change shape: `SpawnError` renders the
    /// same full chain the old `format!("{e:#}")` produced.
    #[test]
    fn display_is_transparent_to_the_anyhow_chain() {
        let error = anyhow::Error::new(protocol_error()).context("placing pane in tab 'card-1'");
        let expected = format!("{error:#}");
        let spawn_error = SpawnError::from(error);
        assert_eq!(format!("{spawn_error:#}"), expected);
        assert_eq!(spawn_error.to_string(), expected);
    }

    #[test]
    fn the_question_mark_operator_classifies() {
        fn inner() -> Result<(), SpawnError> {
            Err::<(), _>(anyhow::Error::new(protocol_error())).context("herdr pane.split")?;
            Ok(())
        }
        assert_eq!(inner().unwrap_err().label(), "protocol");
    }
}
