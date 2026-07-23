//! The column engine: pure, synchronous, no I/O. Given the current world it
//! returns *decisions* (target column, new statuses, system-comment text,
//! validation verdicts). The daemon executes the resulting effects.

mod lifecycle;
mod signals;
mod transitions;
mod validation;

pub use lifecycle::{
    decide_lifecycle, FinalizePlan, LifecycleAction, LifecycleDecision, LifecycleFacts,
    LifecycleHarness, LifecycleRejection,
};
pub use signals::{decide_signal, AgentSignal, SignalDecision};
pub use transitions::{
    decide_auto_hop, decide_entry, decide_resumability, decide_transition, format_duration,
    AutoHopDecision, EntryDecision, ResumabilityDecision, TransitionDecision, MAX_AUTO_HOPS,
};
pub use validation::{
    merge_card_update, merge_column_update, validate_card_archive, validate_card_edit,
    validate_card_settings, validate_card_space, validate_card_values, validate_column_delete,
    validate_column_permission_override, validate_column_settings, validate_column_update,
    validate_column_values, validate_effective_settings, PermissionContext, ValidationError,
};
