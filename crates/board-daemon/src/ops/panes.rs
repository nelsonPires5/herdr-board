//! `pane.set_title`: rename one pane in the **caller's own** herdr session.
//!
//! The daemon owns every Herdr interaction (`AGENTS.md`), so a client that
//! wants its own pane border relabelled asks for it here instead of shelling
//! out to `herdr pane rename` itself. The only current caller is the TUI
//! plugin pane keeping its border in sync with the board it shows.

use super::*;

use std::path::Path;

use board_herdr::PaneRenameParams;

/// Rename `pane_id` in the session `origin_socket` belongs to.
///
/// Takes no [`Daemon`](crate::state::Daemon): this touches no board state, only
/// the caller's own Herdr session. That session is named by `origin_socket`
/// exactly as it is for `run.focus`, because only the caller knows which Herdr
/// it is running inside; the socket is canonicalized and then opened through
/// [`crate::herdr_conn`], so the pinned Herdr 0.8.0 / protocol-19 gate applies
/// before the rename reaches it.
///
/// **A failure here is a real error (code 4), not a silent success.** The
/// daemon must not answer `{renamed:true}` for a rename that did not happen:
/// this method is also readable from the CLI, the logs, and e2e, and those
/// callers need the diagnosis. Treating a cosmetic pane title as non-fatal is
/// the *caller's* policy — the TUI drops the result, exactly as it dropped the
/// subprocess exit status before — and it stays in that one layer.
pub(super) fn pane_set_title(p: PaneSetTitleParams) -> Result<Value> {
    if p.pane_id.trim().is_empty() {
        return Err(Error::BadRequest(
            "pane.set_title requires a non-empty pane_id".into(),
        ));
    }
    let socket = crate::herdr_conn::normalize_socket(Path::new(&p.origin_socket), "origin")?;
    let mut client = crate::herdr_conn::connect_checked(&socket)
        .map_err(|e| Error::HerdrUnavailable(format!("connecting to Herdr: {e}")))?;
    client
        .pane_rename(&PaneRenameParams {
            pane_id: p.pane_id.clone(),
            label: p.title,
        })
        .map_err(|e| Error::HerdrUnavailable(format!("pane.rename {}: {e}", p.pane_id)))?;
    Ok(json!(PaneSetTitleResult { renamed: true }))
}
