//! The Herdr [`Spawner`]: gate the selected socket, allocate a board-owned
//! pane, then hand off to whichever launch half matches the harness kind.

mod configured;
mod managed;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Context};
use board_herdr::HerdrClient;

use super::card_tabs::CardTabRegistry;
use super::placement::{
    allocate_owned_pane, close_owned_after_error, close_owned_for_retry,
    is_retryable_placement_race, CardOwnership,
};
use super::{HerdrLaunchPlan, RuntimeHandle, SpawnError, Spawner};
use crate::herdr_conn::connect_checked_for;

#[cfg(test)]
pub(crate) use configured::{configured_script, posix_quote, remove_file_if_exists};
pub(crate) use configured::{launch_configured, HerdrCliPaneRunner, PaneRunner};
pub(crate) use managed::{launch_managed, DelayFn, DEFAULT_AGENT_START_DELAY};

/// Launches managed agents through the supported Herdr `agent.start` contract, and configured
/// harnesses through a board-owned split child plus `herdr pane run`.
///
/// New durable card tabs retain their root as a shell anchor; the anchor is
/// never started or closed as a run. Managed (Pi/Claude) launches close the
/// anchor after they succeed, so a managed tab converges to exactly one harness
/// pane; configured harnesses keep the persistent anchor because `pane run`
/// exits close their child. Every operation opens a client bound to the run's
/// selected socket. Handles retain the run's explicit socket override so
/// kill/liveness stay in-session.
#[derive(Clone)]
pub struct HerdrSpawner {
    socket: PathBuf,
    pane_runner: Arc<dyn PaneRunner>,
    agent_start_delay: Arc<DelayFn>,
    /// Shared card-tab allocation state (exact tab/anchor ids plus the per-key
    /// allocation mutex). Shared with the `run.focus` rescue, which places panes
    /// into the same card tabs and must not race this spawner.
    card_tabs: Arc<CardTabRegistry>,
}

impl HerdrSpawner {
    pub fn new(socket: PathBuf) -> HerdrSpawner {
        HerdrSpawner {
            socket,
            pane_runner: Arc::new(HerdrCliPaneRunner),
            agent_start_delay: Arc::new(thread::sleep),
            card_tabs: CardTabRegistry::new(),
        }
    }

    /// The shared card-tab allocation registry, for callers that place panes in
    /// the same card tabs (the `run.focus` rescue).
    pub(crate) fn card_tab_registry(&self) -> Arc<CardTabRegistry> {
        Arc::clone(&self.card_tabs)
    }

    #[cfg(test)]
    pub(crate) fn with_pane_runner(
        socket: PathBuf,
        pane_runner: Arc<dyn PaneRunner>,
    ) -> HerdrSpawner {
        Self::with_pane_runner_and_delay(socket, pane_runner, Arc::new(thread::sleep))
    }

    #[cfg(test)]
    pub(crate) fn with_pane_runner_and_delay(
        socket: PathBuf,
        pane_runner: Arc<dyn PaneRunner>,
        agent_start_delay: Arc<DelayFn>,
    ) -> HerdrSpawner {
        HerdrSpawner {
            socket,
            pane_runner,
            agent_start_delay,
            card_tabs: CardTabRegistry::new(),
        }
    }

    /// Open an ungated client on `socket` (the run's session), else the
    /// default socket.
    ///
    /// This helper is intentionally reserved for [`Spawner::kill`] and
    /// [`Spawner::is_alive`]. Cleanup and observation may need to inspect or
    /// close a pane already owned by this daemon even when Herdr has become
    /// incompatible; every new placement/mutation path starts with
    /// [`connect_checked_for`] instead.
    fn client_for(&self, socket: Option<&Path>) -> anyhow::Result<HerdrClient> {
        let target = socket.unwrap_or(&self.socket);
        HerdrClient::connect(target).map_err(|error| {
            let message = error.to_string();
            anyhow::Error::new(error).context(format!("herdr unavailable: {message}"))
        })
    }

    fn selected_socket<'a>(&'a self, req: &'a HerdrLaunchPlan) -> &'a Path {
        req.herdr_socket.as_deref().unwrap_or(&self.socket)
    }

    /// The launch itself, in `anyhow` terms. Every failure keeps its typed
    /// [`board_herdr::HerdrError`] in the chain, which is what lets
    /// [`SpawnError`] classify it at the boundary below.
    fn spawn_inner(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        let selected_socket = self.selected_socket(req).to_path_buf();
        // The gate is the first protocol call: no placement or external runner
        // action is allowed against an incompatible socket. `kill`/`is_alive`
        // deliberately keep the ungated `client_for`: they only ever close or
        // observe a pane this daemon already placed.
        let mut client = connect_checked_for(&selected_socket, "pane placement")?;

        let workspace_id = req
            .workspace_ref
            .as_deref()
            .ok_or_else(|| anyhow!("Herdr spawn requires workspace_ref for pane placement"))?;
        let tab_label = req
            .tab_label
            .as_deref()
            .ok_or_else(|| anyhow!("Herdr spawn requires tab_label for pane placement"))?;
        let env: BTreeMap<String, String> = req.env.iter().cloned().collect();
        let tab_key = (
            selected_socket.clone(),
            workspace_id.to_string(),
            tab_label.to_string(),
        );
        let allocation_lock = self.card_tabs.allocation_lock(&tab_key)?;
        let _allocation_guard = allocation_lock
            .lock()
            .map_err(|_| anyhow!("card-tab allocation lock poisoned"))?;

        // A bootstrap hint is this daemon's own exact record of a workspace it
        // just created (`resolve_space` -> `workspace.create`). Remember its
        // exact tab/root under the per-card lock BEFORE allocating: if the
        // adoption split or the launch fails, the next placement attempt — or a
        // later retry in this same daemon — still finds the adopted tab/root by
        // exact id and recovers it instead of creating a second `card-<id>`
        // tab. This memory is never ownership on its own: the allocator
        // revalidates the exact ids against the live session before every
        // split, so a stale or foreign id can never be adopted by label.
        if tab_label.starts_with("card-") {
            if let Some(bootstrap) = req.bootstrap.as_ref() {
                self.card_tabs.remember(
                    tab_key.clone(),
                    bootstrap.tab_id.clone(),
                    bootstrap.root_pane_id.clone(),
                )?;
            }
        }

        let mut last_placement_race = None;
        for attempt in 0..2 {
            let remembered = self.card_tabs.remembered(&tab_key)?;
            let remembered_tab_id = req
                .owned_tab_id
                .clone()
                .or_else(|| remembered.as_ref().map(|owned| owned.tab_id.clone()));
            let remembered_anchor_id = remembered
                .as_ref()
                .map(|owned| owned.anchor_pane_id.as_str());
            let owned = match allocate_owned_pane(
                &mut client,
                workspace_id,
                tab_label,
                req.cwd.as_deref(),
                &env,
                CardOwnership {
                    owned_tab_id: remembered_tab_id.as_deref(),
                    bootstrap: req.bootstrap.as_ref(),
                    durable_pane_ids: &req.durable_pane_ids,
                    reclaimable_pane_ids: &req.reclaimable_pane_ids,
                    durable_anchor_pane_ids: &req.durable_anchor_pane_ids,
                    remembered_anchor_id,
                    reuse_pane_id: req.reuse_pane_id.as_deref(),
                    reuse_agent_kind: req.agent_kind.as_deref(),
                },
            )
            .with_context(|| format!("placing pane in tab '{tab_label}' for {}", req.name))
            {
                Ok(owned) => owned,
                Err(error) if attempt == 0 && is_retryable_placement_race(&error) => {
                    last_placement_race = Some(error);
                    continue;
                }
                Err(error) => return Err(error),
            };

            if tab_label.starts_with("card-") {
                if let Some(anchor_pane_id) = owned.anchor_pane_id.clone() {
                    self.card_tabs.remember(
                        tab_key.clone(),
                        owned.tab_id.clone(),
                        anchor_pane_id,
                    )?;
                }
            }

            // Placement adopted the prior run's pane (`req.reuse_pane_id`) iff
            // the owned pane id is exactly that candidate. The launch then skips
            // `agent.start` and only re-prompts the live agent.
            let reused = req.reuse_pane_id.as_deref() == Some(owned.pane_id.as_str());

            let launch_result: anyhow::Result<Option<String>> = match req.agent_kind.as_deref() {
                Some(kind) => launch_managed(
                    &mut client,
                    req,
                    kind,
                    &owned.pane_id,
                    reused,
                    self.agent_start_delay.as_ref(),
                    &selected_socket,
                ),
                None => launch_configured(
                    &mut client,
                    self.pane_runner.as_ref(),
                    &selected_socket,
                    req,
                    &owned.pane_id,
                )
                .map(|()| None),
            };

            match launch_result {
                Ok(captured_session_id) => {
                    // Managed card tabs converge to exactly one harness pane:
                    // once the fresh launch (or the reuse re-prompt) succeeded,
                    // close the anchor so no shell strip is left beside the
                    // agent. Closing the split parent is live-verified safe —
                    // the child keeps its process and environment — and the
                    // handle persists anchor_pane_id as None so the registry
                    // and the durable run never treat the closed anchor as
                    // live. Configured harnesses keep their persistent anchor
                    // unchanged: `pane run` exits close their child, so the
                    // anchor is what the next run splits from. A failed anchor
                    // close must not fail an already-successful launch: keep
                    // the anchor live (and persisted) so the next allocation
                    // still finds it by identity.
                    let mut anchor_pane_id = owned.anchor_pane_id.clone();
                    if req.agent_kind.is_some() {
                        if let Some(anchor) = owned.anchor_pane_id.as_deref() {
                            match close_owned_for_retry(&mut client, anchor) {
                                Ok(()) => anchor_pane_id = None,
                                Err(error) => {
                                    tracing::warn!(
                                        pane_id = anchor,
                                        error_category = "herdr",
                                        error = %format!("{error:#}"),
                                        "managed launch succeeded but closing the tab anchor failed; keeping it"
                                    );
                                }
                            }
                        }
                    }
                    return Ok(RuntimeHandle {
                        pane_id: Some(owned.pane_id),
                        workspace_id: Some(owned.workspace_id),
                        anchor_pane_id,
                        pid: None,
                        herdr_socket: req.herdr_socket.clone(),
                        // The launch captured the integration-reported
                        // conversation id (codex/opencode self-mint their
                        // own); dispatch persists it atomically with the
                        // promotion.
                        captured_session_id,
                    });
                }
                Err(error) if attempt == 0 && is_retryable_placement_race(&error) => {
                    if let Err(cleanup_error) = close_owned_for_retry(&mut client, &owned.pane_id) {
                        return Err(error.context(format!(
                            "additionally failed to clean up board-owned pane {} before placement retry: {cleanup_error:#}",
                            owned.pane_id
                        )));
                    }
                    last_placement_race = Some(error);
                }
                Err(error) => {
                    return Err(close_owned_after_error(&mut client, &owned.pane_id, error));
                }
            }
        }

        Err(last_placement_race
            .unwrap_or_else(|| anyhow!("pane placement retry exhausted without a terminal result")))
    }
}

impl Spawner for HerdrSpawner {
    fn spawn(&self, req: &HerdrLaunchPlan) -> Result<RuntimeHandle, SpawnError> {
        self.spawn_inner(req).map_err(SpawnError::from)
    }

    fn kill(&self, h: &RuntimeHandle) -> anyhow::Result<()> {
        if let Some(pane) = &h.pane_id {
            let mut client = self.client_for(h.herdr_socket.as_deref())?;
            client
                .pane_close(pane)
                .with_context(|| format!("herdr pane.close {pane}"))?;
        }
        Ok(())
    }

    fn card_tabs(&self) -> Option<Arc<CardTabRegistry>> {
        Some(self.card_tab_registry())
    }

    fn is_alive(&self, h: &RuntimeHandle) -> anyhow::Result<bool> {
        let Some(pane) = &h.pane_id else {
            return Ok(false);
        };
        let mut client = self.client_for(h.herdr_socket.as_deref())?;
        let snap = client
            .session_snapshot()
            .context("herdr session.snapshot")?;
        Ok(snap.pane_exists(pane))
    }
}
