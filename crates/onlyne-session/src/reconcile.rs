//! Session persistence bridge: the reducer facts in, one ledger trait out.
//!
//! The bridge reduces one [`LifecycleEvent`](crate::lifecycle::LifecycleEvent)
//! against the stored tuple for a task and persists the result. Writes happen
//! on `Applied` only, so a stale or illegal report cannot move the ledger
//! backwards. Every read and write of the session ledger goes through
//! [`SessionLedger`]; the store crate implements it against SQLite.

mod bridge;
mod fault;
mod feed;
mod ledger;
mod record;

pub use bridge::{Bridge, apply_at_next, apply_persist, next_version, try_feed};
pub use fault::{
    DEFAULT_ISOLATE_AFTER, DEFAULT_TERMINATE_AFTER, ProbeVerdict, probe_target, reconcile_dead,
    reconcile_mismatch, reconcile_probe, record_fault,
};
pub use feed::{
    feed_agent_gone, feed_cancel, feed_created, feed_delivered, feed_dispatched, feed_fail,
    feed_intent_receipt, feed_mismatch, feed_ready, feed_reconcile_ok, feed_resource_attached,
    feed_resource_closed, feed_turn_ended, feed_turn_started, settle,
};
pub use ledger::{MemoryLedger, SessionLedger};
pub use record::{
    FaultOutcome, FaultRecord, SessionRecord, VersionedSession, stored_observation, to_versioned,
};

#[cfg(test)]
mod tests;
