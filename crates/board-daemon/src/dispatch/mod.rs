//! Run lifecycle: enqueue, promote (spawn), and finalize (done / fail / timeout
//! / lost / cancel), plus the transition + auto-chain logic. All effects the
//! pure engine only *decides* are executed here.

mod enqueue;
mod finalize;
mod pass;
mod space;
#[cfg(test)]
mod tests;

pub(crate) use enqueue::enqueue_run;
pub(crate) use finalize::{finalize_run, finalize_run_timeout};
pub(crate) use pass::dispatch_pass;

use board_core::db::EnqueueRun;
use board_core::harness::HarnessError;
use board_core::Error;

pub(crate) struct PreparedEnqueue {
    pub(crate) card_id: i64,
    pub(crate) column_id: i64,
    pub(crate) harness: String,
    pub(crate) argv_json: String,
    pub(crate) prompt: String,
    pub(crate) system_prompt: String,
    pub(crate) launch_spec_json: String,
    pub(crate) session_id: Option<String>,
    pub(crate) session: Option<String>,
}

impl PreparedEnqueue {
    pub(crate) fn borrowed(&self) -> EnqueueRun<'_> {
        EnqueueRun {
            card_id: self.card_id,
            column_id: self.column_id,
            harness: &self.harness,
            argv_json: &self.argv_json,
            prompt_snapshot: &self.prompt,
            system_prompt_snapshot: Some(&self.system_prompt),
            launch_spec_json: Some(&self.launch_spec_json),
            session_id: self.session_id.as_deref(),
            session: self.session.as_deref(),
        }
    }
}

pub(crate) const HERDR_PROTOCOL: u32 = 17;

pub(crate) fn map_harness_err(e: HarnessError) -> Error {
    match e {
        HarnessError::UnknownHarness(h) => Error::BadRequest(format!("unknown harness: {h}")),
        HarnessError::MissingMintedSession => {
            Error::BadRequest("mint session requested without a uuid".into())
        }
        HarnessError::MissingForkTargetSession => {
            Error::BadRequest("Pi fork requested without a new session uuid".into())
        }
        HarnessError::PiPermissionModeUnsupported => {
            Error::BadRequest("pi does not support permission modes".into())
        }
    }
}
