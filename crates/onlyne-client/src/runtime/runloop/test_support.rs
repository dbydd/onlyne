use super::config::{DEFAULT_INTENT_ATTEMPTS, RunState, default_intent_backoff};
use crate::backend::fake::FakeBackend;
use crate::backend::{AcpOptions, SessionPlacement, WorktreePolicy};
use crate::runtime::intent::{IntentMachine, op_for_intent};
use crate::runtime::runloop::config::BackendSelector;
use crate::session::dispatch::DispatchState;
use onlyne_proto::{ClientOp, Presence, RoleInfo};
use onlyne_store::ClientStore;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::Mutex;

pub(super) fn test_state(max_sessions: u32, command: Vec<String>) -> (RunState, ClientStore) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("client.db");
    let store = ClientStore::open(path).expect("client store");
    let dispatch = DispatchState::new(
        "planner",
        dir.path(),
        command,
        max_sessions,
        Arc::new(FakeBackend::new()),
        store.clone(),
    );
    let intents = IntentMachine::new(
        store.clone(),
        DEFAULT_INTENT_ATTEMPTS,
        default_intent_backoff(),
    );
    let state = RunState {
        accept_new: dispatch.accept_new(),
        store: store.clone(),
        intents: Arc::new(parking_lot::Mutex::new(intents)),
        dispatch,
        welcome: Arc::new(Mutex::new(None)),
        stall_report_secs: 1,
        // This fixture exercises the stall and intent surfaces, not the
        // reconnect sweep, so the window stays closed here.
        reconnect_grace_secs: 0,
        selector: BackendSelector {
            placement: SessionPlacement::Fake,
            runner: Arc::new(crate::backend::ProcessRunner),
            worktree: WorktreePolicy::Host,
            acp: AcpOptions::default(),
        },
    };
    (state, store)
}

pub(super) fn role_info(max_sessions: u32, command: Vec<String>) -> RoleInfo {
    RoleInfo {
        name: "planner".into(),
        admin: false,
        max_sessions,
        runtime: onlyne_proto::RoleRuntime {
            drive: onlyne_proto::Drive::Plugin,
            command,
        },
        spec_hash: "hash".into(),
        prose: None,
        state: Presence::Online,
        sessions: 0,
        queued: 0,
        detail: None,
        edges: Vec::new(),
        aggregate: None,
    }
}

/// The frames the durable intent queue still holds, decoded.
///
/// A case reads this as "what the client owes the server": the queue is where an
/// ack, a report, or a completion waits for the flusher, so a row with no entry
/// here is a row this client has said nothing about.
pub(super) fn pending_intent_ops(state: &RunState) -> anyhow::Result<Vec<ClientOp>> {
    let rows = state.intents.lock().pending()?;
    rows.iter().map(op_for_intent).collect()
}
