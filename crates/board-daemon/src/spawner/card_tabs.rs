//! Shared card-tab allocation state.
//!
//! Two *different* callers place panes into a card's durable `card-<id>` tab:
//! dispatch (through [`HerdrSpawner::spawn`](super::HerdrSpawner)) and the
//! `run.focus` rescue (through [`super::rescue_run_pane`]). Board requests are
//! served concurrently — one task per connection — so they can race, and each
//! race would create a *second* `card-<id>` tab or a second pane in it.
//!
//! Both therefore share one registry rather than each keeping their own: the
//! per-key mutex serializes first allocation, and the ownership map remembers
//! the exact tab/anchor ids so the next allocation reuses them instead of
//! creating another tab. Labels are never ownership; only these exact ids are.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// `(session socket, workspace id, card tab label)` — the scope within which a
/// card tab is allocated at most once.
pub(crate) type CardTabKey = (PathBuf, String, String);

/// Exact ids of a board-owned card tab and its persistent shell anchor.
#[derive(Debug, Clone)]
pub(crate) struct OwnedCardTab {
    pub(crate) tab_id: String,
    pub(crate) anchor_pane_id: String,
}

/// Process-wide card-tab allocation state, shared by every placement caller.
///
/// This type is `pub` only because it appears in the public [`Spawner`](super::Spawner)
/// trait; its contents are an internal placement detail.
#[derive(Debug, Default)]
pub struct CardTabRegistry {
    owned: Mutex<BTreeMap<CardTabKey, OwnedCardTab>>,
    locks: Mutex<BTreeMap<CardTabKey, Arc<Mutex<()>>>>,
}

impl CardTabRegistry {
    pub(crate) fn new() -> Arc<CardTabRegistry> {
        Arc::new(CardTabRegistry::default())
    }

    /// The per-key mutex that serializes allocation for one card tab without
    /// serializing unrelated cards or workspaces. Callers hold its guard across
    /// the whole discover→create/split→launch sequence.
    pub(crate) fn allocation_lock(&self, key: &CardTabKey) -> anyhow::Result<Arc<Mutex<()>>> {
        Ok(self
            .locks
            .lock()
            .map_err(|_| anyhow::anyhow!("card-tab allocation lock poisoned"))?
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone())
    }

    pub(crate) fn remembered(&self, key: &CardTabKey) -> anyhow::Result<Option<OwnedCardTab>> {
        Ok(self
            .owned
            .lock()
            .map_err(|_| anyhow::anyhow!("card-tab ownership lock poisoned"))?
            .get(key)
            .cloned())
    }

    /// Record the exact tab/anchor a successful allocation produced, so the next
    /// allocation for this key reuses them. The rescue registers here too: a
    /// tab it had to create must not be re-created by the next dispatch.
    pub(crate) fn remember(
        &self,
        key: CardTabKey,
        tab_id: String,
        anchor_pane_id: String,
    ) -> anyhow::Result<()> {
        self.owned
            .lock()
            .map_err(|_| anyhow::anyhow!("card-tab ownership lock poisoned"))?
            .insert(
                key,
                OwnedCardTab {
                    tab_id,
                    anchor_pane_id,
                },
            );
        Ok(())
    }

    /// Forget a key whose tab could not be kept (e.g. a rescue that created a
    /// tab and then failed, and closed it again). Leaving a stale id behind
    /// would make the next allocation try to split from a pane that is gone.
    pub(crate) fn forget(&self, key: &CardTabKey) -> anyhow::Result<()> {
        self.owned
            .lock()
            .map_err(|_| anyhow::anyhow!("card-tab ownership lock poisoned"))?
            .remove(key);
        Ok(())
    }
}
