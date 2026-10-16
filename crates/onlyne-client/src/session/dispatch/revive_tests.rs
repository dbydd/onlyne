//! What a revived session runs, and what it ran when it was born.
//!
//! A released `task`-scope conversation is brought back by re-spawning its
//! runtime, and the argv that does it is the role's `[client.runtime] resume`
//! rather than the `command` the session opened with: `pi --session-id <id>`
//! creates a conversation and `pi --session <id>` opens one that already
//! exists, so running the first again would put a second conversation under a
//! family that already has one —
//! the leak the scope exists to prevent.

use super::delivery::dispatch;
use crate::backend::fake::FakeBackend;
use onlyne_proto::{Body, Causality, Envelope, MsgKind, Principal, new_envelope, new_task_id};
use onlyne_store::ClientStore;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

/// One delivery of a family, in the shape the client's own door measures.
fn delivery(family: &str) -> Envelope {
    let mut causality = Causality::root(new_task_id());
    causality.family = Some(family.to_string());
    new_envelope(
        MsgKind::Task,
        Principal::role("sender"),
        Principal::role("planner"),
        Body::text("work"),
        Some(causality),
    )
    .expect("task envelope")
}

/// A state whose role declares both argvs and whose scope keeps a session.
fn state_with_both_argvs() -> (crate::session::dispatch::DispatchState, Arc<FakeBackend>) {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let backend = Arc::new(FakeBackend::new());
    let state = crate::session::dispatch::DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        8,
        backend.clone(),
        store,
    );
    state.reconfigure(crate::session::slice::RoleSlice {
        drive: onlyne_config::Drive::Plugin,
        command: vec!["pi".into(), "--session-id".into(), "{session}".into()],
        resume: vec!["pi".into(), "--session".into(), "{session}".into()],
        max_sessions: 8,
        required_targets: Vec::new(),
    });
    state.inner.lock().session_policy = onlyne_config::SessionPolicy {
        scope: onlyne_config::SessionScope::Task,
        idle_close: Some(Duration::from_secs(60)),
    };
    (state, backend)
}

#[tokio::test]
async fn a_revived_session_runs_the_resume_argv() {
    let (state, backend) = state_with_both_argvs();

    dispatch(&state, &delivery("conversation")).expect("the first delivery opens a session");
    // The release an `idle_close` performs: the slot keeps the conversation and
    // gives its process back.
    {
        let mut inner = state.inner.lock();
        let key = inner.sessions.keys().next().cloned().expect("a session");
        inner.sessions.get_mut(&key).expect("the slot").suspended = true;
    }
    dispatch(&state, &delivery("conversation")).expect("the second delivery revives it");

    let spawns = backend.spawned_commands();
    assert_eq!(
        spawns.len(),
        2,
        "one spawn to open the conversation and one to bring it back: {spawns:?}"
    );
    assert_eq!(
        spawns[0][..2],
        ["pi".to_string(), "--session-id".to_string()],
        "a session is born with the role's command: {spawns:?}"
    );
    assert_eq!(
        spawns[1][..2],
        ["pi".to_string(), "--session".to_string()],
        "and comes back with the role's resume argv: {spawns:?}"
    );
    // `{session}` is the conversation's own id in both, so the revival asks for
    // the session that was released rather than a fresh one.
    assert_eq!(
        spawns[0][2], spawns[1][2],
        "both argvs name the same session id: {spawns:?}"
    );
}

#[tokio::test]
async fn a_role_with_no_resume_argv_revives_with_the_command_it_was_born_with() {
    // The behaviour every role had before the key existed: a runtime that
    // cannot continue a conversation is started again, and this client does not
    // invent a flag for it.
    let (state, backend) = state_with_both_argvs();
    state.reconfigure(crate::session::slice::RoleSlice {
        drive: onlyne_config::Drive::Plugin,
        command: vec!["pi".into(), "--session-id".into(), "{session}".into()],
        resume: Vec::new(),
        max_sessions: 8,
        required_targets: Vec::new(),
    });

    dispatch(&state, &delivery("conversation")).expect("the first delivery opens a session");
    {
        let mut inner = state.inner.lock();
        let key = inner.sessions.keys().next().cloned().expect("a session");
        inner.sessions.get_mut(&key).expect("the slot").suspended = true;
    }
    dispatch(&state, &delivery("conversation")).expect("the second delivery revives it");

    let spawns = backend.spawned_commands();
    assert_eq!(spawns.len(), 2, "{spawns:?}");
    assert_eq!(
        spawns[0], spawns[1],
        "with no resume argv the revival runs the command again: {spawns:?}"
    );
}
