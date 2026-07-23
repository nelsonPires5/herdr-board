use super::*;
use board_core::capability::{available_harnesses, capabilities_for};
use board_core::pi_catalog;
pub(super) fn harness_capabilities(d: &Arc<Daemon>, p: HarnessCapabilitiesParams) -> Result<Value> {
    match capabilities_for(&p.harness, &d.config) {
        Some(mut caps) => {
            // Pi's static catalog is free-form (models: []); overlay the live
            // catalog read from the pi agent dir when one is configured. Tests
            // leave `pi_agent_dir` unset, so this stays the static catalog.
            if p.harness == "pi" {
                let models = pi_catalog::live_models(d.config.pi_agent_dir.as_deref(), "pi");
                if !models.is_empty() {
                    caps.models = models;
                }
            }
            Ok(json!(caps))
        }
        None => {
            let known = available_harnesses(&d.config);
            Err(Error::NotFound(format!(
                "unknown harness '{}'; known: {}",
                p.harness,
                known.join(", ")
            )))
        }
    }
}

pub(super) fn harness_list(d: &Arc<Daemon>) -> Result<Value> {
    Ok(json!(HarnessListResult {
        harnesses: available_harnesses(&d.config)
    }))
}

pub(super) fn space_list(d: &Arc<Daemon>, p: SpaceListParams) -> Result<Value> {
    let reg = d
        .session_registry
        .as_ref()
        .ok_or_else(|| Error::HerdrUnavailable("herdr not connected".into()))?;
    // Resolve the requested session (None = default) to its socket; an
    // unknown/stopped session errors listing the known ones.
    let resolved = reg
        .resolve(p.session.as_deref())
        .map_err(|e| Error::HerdrUnavailable(format!("session '{:?}': {e:#}", p.session)))?;
    let mut client = board_herdr::HerdrClient::connect(&resolved.socket)
        .map_err(|e| Error::HerdrUnavailable(format!("herdr unavailable: {e}")))?;
    let workspaces = client
        .workspace_list()
        .map_err(|e| Error::HerdrUnavailable(format!("workspace.list: {e}")))?;
    let spaces = workspaces
        .into_iter()
        .map(|w| SpaceInfo {
            id: w.workspace_id,
            label: w.label,
        })
        .collect();
    Ok(json!(SpaceListResult { spaces }))
}

pub(super) fn session_list(d: &Arc<Daemon>) -> Result<Value> {
    let reg = d
        .session_registry
        .as_ref()
        .ok_or_else(|| Error::HerdrUnavailable("herdr not connected".into()))?;
    let sessions = reg
        .session_infos()
        .map_err(|e| Error::HerdrUnavailable(format!("session.list: {e:#}")))?;
    Ok(json!(SessionListResult { sessions }))
}
