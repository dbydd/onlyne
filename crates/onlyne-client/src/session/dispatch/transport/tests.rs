use super::*;
use onlyne_proto::new_task_id;
use onlyne_session::backend::fake::FakeBackend;
use tempfile::tempdir;

/// A task's delivery handle belongs to the session serving it, never to one
/// that came back for it.
///
/// Two slots can name one task: the session that took the task while the
/// older one's connection still stands. The lookup the assignment path uses
/// has to prefer the slot that is not read-only, because the handle it stores
/// is what settles the server's delivery row. The unfixed lookup took
/// whichever slot the hash map yielded first, so with eight read-only slots
/// and one live it named a read-only handle in eight runs of nine — the retry
/// kept waiting for an ack that had already been written for another session,
/// and the server re-delivered the task to a role that had finished it. The
/// fixed rule names the live slot every run.
#[test]
fn a_read_only_slot_never_holds_the_handle_of_the_task_it_lost() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let state = DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        8,
        Arc::new(FakeBackend::new()),
        store,
    );
    let task = new_task_id();
    let serving = |read_only: bool| SessionSlot {
        session: SessionRef {
            task_id: task.clone(),
            backend: "fake".into(),
            backend_ref: serde_json::Value::Null,
            generation: 1,
        },
        task_id: Some(task.clone()),
        ready: true,
        payload: None,
        msg_id: None,
        origin: None,
        hop: 0,
        dropped_at: None,
        last_beat: Some(Instant::now()),
        read_only,
    };
    state
        .inner
        .lock()
        .sessions
        .insert("serving".into(), serving(false));
    for index in 0..8 {
        state
            .inner
            .lock()
            .sessions
            .insert(format!("revived-{index}"), serving(true));
    }

    state.attach_msg_id(&task, "msg-serving");

    let inner = state.inner.lock();
    assert_eq!(
        inner.sessions["serving"].msg_id.as_deref(),
        Some("msg-serving"),
        "the session serving the task carries its delivery handle"
    );
    for index in 0..8 {
        let key = format!("revived-{index}");
        assert_eq!(
            inner.sessions[&key].msg_id, None,
            "the read-only slot {key} is handed no handle for a task it no longer serves"
        );
    }
}
