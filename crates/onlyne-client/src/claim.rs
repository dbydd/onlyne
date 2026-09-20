//! Hello claim: task ids occupying dispatch live slots.
//!
//! Slots exist from assign until `release_locked`. A fresh process has an
//! empty map, so hello sends no `live_tasks` and the server requeues. A live
//! client whose link flaps still holds its slots, so those rows stay
//! `in_flight`. Heartbeat stale, stall reports, and operator repair cover a
//! pane that has already died.

use std::collections::BTreeSet;

/// Sorted, deduplicated task ids from dispatch live slots.
pub fn from_slots(ids: impl IntoIterator<Item = impl Into<String>>) -> Vec<String> {
    let tasks: BTreeSet<String> = ids.into_iter().map(Into::into).collect();
    tasks.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{DispatchState, dispatch, hello_with_live_tasks};
    use onlyne_proto::{
        Body, Causality, ClientOp, Envelope, HandshakeArgs, MsgKind, PROTOCOL_VERSION, Principal,
        new_envelope, new_task_id,
    };
    use onlyne_session::{SessionLedger, VersionedSession, backend::fake::FakeBackend};
    use onlyne_store::ClientStore;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn stored(lifecycle: &str, agent: &str, resource: &str) -> VersionedSession {
        VersionedSession {
            agent_state: agent.into(),
            delivery_state: "none".into(),
            resource_state: resource.into(),
            public_lifecycle: lifecycle.into(),
            recovery_substate: "none".into(),
            desired_json: "{}".into(),
            observed_json: "{}".into(),
            generation: 1,
            seq: 1,
            backend_ref: "{}".into(),
            mismatch_count: 0,
            updated_at: 0,
        }
    }

    fn hello_args(live_tasks: Vec<String>) -> HandshakeArgs {
        HandshakeArgs {
            protocol: PROTOCOL_VERSION,
            role: "planner".into(),
            key: "ed25519/AAA".into(),
            signature: "sig".into(),
            agent: "onlyne-client".into(),
            version: "1.0.9".into(),
            aggregate: false,
            live_tasks,
        }
    }

    fn task_envelope() -> Envelope {
        new_envelope(
            MsgKind::Task,
            Principal::role("planner"),
            Principal::role("planner"),
            Body::text("work"),
            Some(Causality::root(new_task_id())),
        )
        .expect("task envelope")
    }

    #[test]
    fn from_slots_sorts_and_dedups() {
        assert_eq!(
            from_slots(["t-b", "t-a", "t-b"]),
            vec!["t-a".to_string(), "t-b".to_string()]
        );
        assert!(from_slots(Vec::<String>::new()).is_empty());
    }

    #[test]
    fn hello_live_tasks_empty_when_store_has_rows_but_no_slots() {
        let dir = tempdir().unwrap();
        let store = ClientStore::open(dir.path().join("client.db")).unwrap();
        store
            .upsert_session("t-work", &stored("working", "running", "attached"))
            .unwrap();
        store
            .upsert_session("t-idle", &stored("idle", "idle", "attached"))
            .unwrap();
        let dispatch = DispatchState::new(
            "planner",
            dir.path(),
            vec!["echo".into()],
            2,
            Arc::new(FakeBackend::new()),
            store,
        );
        let tasks = dispatch.hello_live_tasks();
        assert!(tasks.is_empty(), "a fresh process has no slots: {tasks:?}");
        let hello = hello_with_live_tasks(&hello_args(Vec::new()), tasks);
        let value = serde_json::to_value(&hello).unwrap();
        assert!(value.get("live_tasks").is_none());
    }

    #[test]
    fn hello_live_tasks_only_slot_tasks() {
        let dir = tempdir().unwrap();
        let store = ClientStore::open(dir.path().join("client.db")).unwrap();
        store
            .upsert_session("t-orphan", &stored("working", "running", "attached"))
            .unwrap();
        let state = DispatchState::new(
            "planner",
            dir.path(),
            vec!["echo".into()],
            2,
            Arc::new(FakeBackend::new()),
            store,
        );
        let first = task_envelope();
        let first_id = first.task_id().unwrap().to_string();
        dispatch(&state, &first).unwrap();
        let second = task_envelope();
        let second_id = second.task_id().unwrap().to_string();
        dispatch(&state, &second).unwrap();

        let tasks = state.hello_live_tasks();
        let mut expected = vec![first_id.clone(), second_id.clone()];
        expected.sort();
        assert_eq!(tasks, expected);
        assert!(!tasks.contains(&"t-orphan".to_string()));

        let hello = hello_with_live_tasks(&hello_args(Vec::new()), tasks);
        let frame = serde_json::to_value(ClientOp::Hello(hello)).unwrap();
        let claimed = frame["args"]["live_tasks"]
            .as_array()
            .expect("hello carries live_tasks");
        let names: Vec<&str> = claimed.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(
            names,
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(!names.contains(&"t-orphan"));
    }

    #[test]
    fn empty_live_tasks_are_omitted_from_the_hello_frame() {
        let value = serde_json::to_value(hello_args(Vec::new())).unwrap();
        assert!(value.get("live_tasks").is_none());
    }
}
