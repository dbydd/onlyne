use super::*;
use onlyne_proto::new_task_id;
use onlyne_session::backend::fake::FakeBackend;
use tempfile::tempdir;

/// One task-shaped delivery, the only envelope kind `dispatch` accepts.
fn delivery(task: &str) -> Envelope {
    new_envelope(
        MsgKind::Task,
        Principal::role("sender"),
        Principal::role("planner"),
        Body::text("repair the failing widget"),
        Some(Causality::root(task.to_string())),
    )
    .expect("task envelope")
}

/// A dispatch that cannot write its record gives the host resource back.
///
/// `dispatch` opens the resource first and lands it in the slot map last, with
/// the ledger writes in between — the order the ledger's causal chain asks for.
/// A database failure in that middle stretch used to return the error and keep
/// the resource: no slot named it, so `close_all`, both reconnect sweeps, and
/// `live_sessions` never saw it again, and the pane, tab, or child process the
/// backend opened ran for the rest of the client's life with nothing left able to
/// address it.
///
/// The database refuses here the way a real one does at the door — its own `task`
/// table is gone, so `open_task` answers with its own error rather than a
/// simulated one — and the assertion names what an operator would otherwise never
/// learn: this client holds no session it cannot reach.
#[test]
fn a_dispatch_that_cannot_write_its_record_gives_the_resource_back() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("client.db");
    let store = ClientStore::open(&database).expect("client store");
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        1,
        backend.clone(),
        store.clone(),
    );
    let task = new_task_id();
    let other = rusqlite::Connection::open(&database).expect("a second handle on the db");
    other
        .execute_batch("DROP TABLE task;")
        .expect("the ledger's own table is gone");

    assert!(
        dispatch(&state, &delivery(&task)).is_err(),
        "a delivery whose task record cannot be opened is refused"
    );

    assert!(
        backend.sessions().is_empty(),
        "the resource a refused dispatch spawned is not left running: {:?}",
        backend.sessions().keys().collect::<Vec<_>>()
    );
    assert_eq!(
        state.session_count(),
        0,
        "a dispatch that failed leaves no slot behind"
    );
    assert!(
        state.has_capacity(),
        "a role capped at one session can take the next delivery"
    );
}
