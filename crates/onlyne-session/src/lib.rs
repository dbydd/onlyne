//! Session kernel: pure lifecycle reducer, process backends, ledger bridge.

pub mod backend;
pub mod lifecycle;
pub mod reconcile;

pub use backend::{
    Capabilities, CloseReason, CommandOutput, ProcessRunner, ResourceProbe, Runner, SessionBackend,
    SessionRef, SpawnSpec, backend_by_name, backend_for, default_backend, select_backend,
};
pub use lifecycle::{
    AgentState, DeliveryState, IgnoredReason, LifecycleEvent, Observation, Outcome,
    PublicLifecycle, RecoveryState, RejectReason, ResourceState, Verdict, Version, apply,
    event_version, is_legal, project,
};
pub use reconcile::{
    Bridge, DEFAULT_ISOLATE_AFTER, DEFAULT_TERMINATE_AFTER, FaultOutcome, FaultRecord,
    MemoryLedger, ProbeVerdict, SessionLedger, SessionRecord, VersionedSession, apply_at_next,
    apply_persist, feed_agent_gone, feed_cancel, feed_created, feed_delivered, feed_dispatched,
    feed_fail, feed_mismatch, feed_ready, feed_reconcile_ok, feed_resource_attached,
    feed_resource_closed, feed_turn_ended, feed_turn_started, next_version, probe_target,
    reconcile_dead, reconcile_mismatch, reconcile_probe, record_fault, settle, stored_observation,
    to_versioned, try_feed,
};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired_into_the_workspace() {
        assert!(env!("CARGO_PKG_NAME").ends_with("onlyne-session"));
    }
}
