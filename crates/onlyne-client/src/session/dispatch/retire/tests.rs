use super::*;
use onlyne_proto::new_task_id;
use onlyne_session::backend::fake::FakeBackend;
use tempfile::tempdir;

/// The resource one client-held session names on this role's backend.
fn session_ref(task: &str) -> SessionRef {
    SessionRef {
        task_id: task.to_string(),
        backend: "fake".into(),
        backend_ref: serde_json::Value::Null,
        generation: 1,
    }
}

/// One slot as the two binding paths leave it: the session it holds, and how it
/// stands with the task it answers for.
fn slot(task: &str, read_only: bool, dropped_at: Option<Instant>) -> SessionSlot {
    SessionSlot {
        session: session_ref(task),
        task_id: Some(task.to_string()),
        ready: !read_only,
        payload: None,
        msg_id: None,
        origin: None,
        causality: Causality::root(task.to_string()),
        dropped_at,
        last_beat: Some(Instant::now()),
        read_only,
    }
}

/// One slot as the sweep finds it past the window: a session dispatched for its
/// task, on a backend resource, whose plugin connection never came back.
fn dispatched_ghost(state: &DispatchState, task: &str, dropped_at: Instant) {
    dispatch(
        state,
        &new_envelope(
            MsgKind::Task,
            Principal::role("sender"),
            Principal::role("planner"),
            Body::text("repair the failing widget"),
            Some(Causality::root(task.to_string())),
        )
        .expect("task envelope"),
    )
    .expect("the delivery takes a session");
    let mut inner = state.inner.lock();
    let slot = inner.sessions.get_mut(task).expect("the dispatched slot");
    slot.dropped_at = Some(dropped_at);
}

/// The reconnect grace cannot take a slot the client holds read-only.
///
/// A slot demoted by the `moved_on` binding — a session that came back for a task
/// a newer session already serves — keeps the drop stamp it had, because the
/// connection waiting behind the live one is not the session losing its agent.
/// Judged by `transports` alone it looked exactly like a ghost whose agent never
/// returned, and the sweep feeds its agent-gone event by session id, which for a
/// client-held session is the task id: closing the ghost pushed the live
/// session's own mirror to `Exited` and closed the resource its agent is still
/// running in. That retirement belongs to `retire_revived`, which runs when the
/// completion that answers the held connection merges. The sweep leaves the live
/// session's binding and its row alone.
#[tokio::test]
async fn the_reconnect_grace_does_not_take_a_slot_the_client_holds_read_only() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        8,
        backend.clone(),
        store.clone(),
    );
    let task = new_task_id();
    let (live_stream, _live_peer) = tokio::io::duplex(1024);
    let live = AdapterIo::new(live_stream, Duration::from_secs(5), Duration::from_secs(5));
    let (held_stream, _held_peer) = tokio::io::duplex(1024);
    let held = AdapterIo::new(held_stream, Duration::from_secs(5), Duration::from_secs(5));

    {
        let inner = state.inner.lock();
        inner.bridge.track_live(session_ref(&task));
        feed_created(&inner.bridge, &inner.store, &task).expect("seed the session row");
        feed_ready(&inner.bridge, &inner.store, &task).expect("ready");
    }
    {
        let mut inner = state.inner.lock();
        // The session serving the task, on the connection that mounted it.
        inner
            .sessions
            .insert("live".into(), slot(&task, false, None));
        inner
            .transports
            .insert("live".into(), (live.clone(), Vec::new()));
        // The ghost: its own session answered for this task once, so the newer
        // session took it, and only the connection that came back for it is left.
        // Past the window, exactly as the sweep reads it.
        let dropped = Instant::now()
            .checked_sub(Duration::from_secs(61))
            .expect("an instant a minute back");
        inner
            .sessions
            .insert("ghost".into(), slot(&task, true, Some(dropped)));
        inner
            .revived
            .push(("ghost".into(), held.clone(), Vec::new()));
    }
    let before = store.get_session(&task).unwrap().expect("the seeded row");

    assert_eq!(
        state.retire_dropped_ghosts(Instant::now(), 60).len(),
        0,
        "a slot the client holds read-only is not the reconnect grace's to take"
    );

    let inner = state.inner.lock();
    assert_eq!(
        inner.sessions["live"].task_id.as_deref(),
        Some(task.as_str()),
        "the session serving the task keeps its binding"
    );
    assert!(
        inner.sessions.contains_key("ghost"),
        "the held slot stays: retire_revived retires it with the completion"
    );
    drop(inner);
    let after = store.get_session(&task).unwrap().expect("the seeded row");
    assert_eq!(
        after, before,
        "the live session's stored tuple is where the serving connection left it"
    );
    assert_ne!(
        projection_of(&after, TaskState::Pending).lifecycle,
        Lifecycle::Exited,
        "and the mirror that tuple projects is not the ghost's exit: {after:?}"
    );
}

/// A session whose agent left inside the reconnect grace settles the work it
/// still owed.
///
/// The sweep is the only writer left for this case: the plugin connection that
/// would have reported the ending is the one that dropped, and a task nobody
/// answers stays `settled_at IS NULL` forever — `open_tasks` keeps reading it and
/// the server keeps re-offering a delivery no client can take. The verdict is
/// the one the close reason already carries for the same slot, the agent-gone
/// feed beside it is what puts the session's own tuple at `Exited`, and the
/// resource closes behind both.
///
/// The sweep's `failed` is an inference about work nobody answered, so an answer
/// that arrived first outranks it: `settle_task` updates only where `settled_at
/// IS NULL`, and the second slot below is the proof that a completion reported
/// before the window expired is not overwritten by the death verdict.
#[test]
fn a_session_that_died_at_the_grace_window_settles_the_task_it_owed() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        8,
        backend.clone(),
        store.clone(),
    );
    let task = new_task_id();
    // The same sweep, one window later, over a task its own completion already
    // answered.
    let answered = new_task_id();
    let dropped = Instant::now()
        .checked_sub(Duration::from_secs(61))
        .expect("an instant a minute back");
    dispatched_ghost(&state, &task, dropped);
    dispatched_ghost(&state, &answered, dropped);
    store
        .settle_task(&answered, TaskState::Done)
        .expect("the completion landed first");
    assert!(
        !store.open_tasks(10).unwrap().is_empty(),
        "the work is open while its session is alive"
    );

    assert_eq!(
        state.retire_dropped_ghosts(Instant::now(), 60).len(),
        2,
        "the sweep takes the ghost and its resource"
    );

    assert_eq!(
        store
            .task(&answered)
            .expect("read the answered task")
            .expect("the answered task has a record")
            .task_state,
        TaskState::Done,
        "a verdict that already landed is the one that stands"
    );
    let record = store
        .task(&task)
        .expect("read the task record")
        .expect("the task has a record");
    assert_eq!(
        record.task_state,
        TaskState::Failed,
        "the work owed when the agent left ends failed: {record:?}"
    );
    assert!(
        record.settled_at.is_some(),
        "and the row reads settled: {record:?}"
    );
    assert!(
        store.open_tasks(10).unwrap().is_empty(),
        "nothing is left open for the server to re-offer"
    );
    let row = store.get_session(&task).unwrap().expect("the seeded row");
    assert_eq!(row.agent_state, "gone", "the agent left");
    assert_eq!(row.resource_state, "closed", "and its resource with it");
    assert_eq!(
        projection_of(&row, TaskState::Failed).lifecycle,
        Lifecycle::Exited,
        "the session's projection reads exited: {row:?}"
    );
}

/// Each arm of the window signs the retirement it decides.
///
/// The two readings reach the same verdict through different facts, and an operator telling a
/// vanished agent apart from one that merely stopped reporting has to be able to see which fact
/// closed the window: the reconnect grace answers a connection that ended, the silence window
/// answers one still attached while nothing the client accepts arrives over it. The ages come
/// off the slot as the sweep read it, before the retirement took the binding away.
#[tokio::test]
async fn each_arm_of_the_window_names_itself_on_the_retirement() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        8,
        backend.clone(),
        store.clone(),
    );

    // The drop arm: a connection that ended a minute back and never came back.
    let dropped_task = new_task_id();
    let gone = Instant::now()
        .checked_sub(Duration::from_secs(61))
        .expect("an instant a minute back");
    dispatched_ghost(&state, &dropped_task, gone);

    // The silence arm: the connection is attached, its session's task still owed, and
    // the last frame this client accepted over it is older than three heartbeats.
    let quiet_task = new_task_id();
    dispatched_ghost(&state, &quiet_task, Instant::now());
    let (stream, _peer) = tokio::io::duplex(1024);
    let io = AdapterIo::new(stream, Duration::from_secs(5), Duration::from_secs(5));
    {
        let mut inner = state.inner.lock();
        inner
            .transports
            .insert(quiet_task.clone(), (io, Vec::new()));
        let slot = inner.sessions.get_mut(&quiet_task).expect("the slot");
        slot.dropped_at = None;
        slot.last_beat =
            Some(Instant::now() - Duration::from_secs(31) - Duration::from_millis(200));
    }

    let retired = state.retire_dropped_ghosts(Instant::now(), 60);
    let dropped = retired
        .iter()
        .find(|one| one.session_id == dropped_task)
        .expect("the grace arm retired the session whose connection ended");
    assert_eq!(dropped.arm.word(), "reconnect_grace");
    assert_eq!(dropped.away_secs, Some(61), "the away time it read");

    let silent = retired
        .iter()
        .find(|one| one.session_id == quiet_task)
        .expect("the silence arm retired the session whose connection held");
    assert_eq!(silent.arm.word(), "heartbeat_silence");
    assert_eq!(silent.away_secs, None, "no connection ended on this arm");
    assert!(
        silent.quiet_secs >= 31,
        "the quiet age the sweep read: {}",
        silent.quiet_secs
    );
}

/// A control close ends the session's own row.
///
/// `release_locked` closes the resource and drops the slot in the same breath, and the agent
/// goes with them. The tuple's agent phase is what `project` reads: a begun task
/// (`Done`/`Failed`/`Cancelled`) answers `working` while the agent is not `Gone`, so a close
/// that fed the resource and skipped the agent left the mirrored row reading `working` beside a
/// ledger row that had already settled. Two live sessions showed exactly that — cancelled and
/// faulted, both mirrored as `working`, while three others closed through the reconnect sweep
/// flipped to `exited`.
#[tokio::test]
async fn a_control_close_ends_the_sessions_own_row() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        8,
        backend.clone(),
        store.clone(),
    );
    let task = new_task_id();
    let (stream, _peer) = tokio::io::duplex(1024);
    let io = AdapterIo::new(stream, Duration::from_secs(5), Duration::from_secs(5));

    {
        let inner = state.inner.lock();
        inner.bridge.track_live(session_ref(&task));
        feed_created(&inner.bridge, &inner.store, &task).expect("seed the session row");
        feed_ready(&inner.bridge, &inner.store, &task).expect("ready");
    }
    {
        let mut inner = state.inner.lock();
        inner
            .sessions
            .insert("live".into(), slot(&task, false, None));
        inner
            .transports
            .insert("live".into(), (io.clone(), Vec::new()));
    }
    store
        .settle_task(&task, TaskState::Cancelled)
        .expect("the operator's cancel landed");

    // The verdict on its own leaves the session working: a begun task is open work while the
    // agent may still be running in its resource.
    {
        let inner = state.inner.lock();
        let row = inner
            .store
            .get_session(&task)
            .unwrap()
            .expect("the session row");
        assert_eq!(
            crate::session::dispatch::projection_of(&row, TaskState::Cancelled).lifecycle,
            onlyne_proto::Lifecycle::Working,
            "a cancelled task with a live agent is still working"
        );
    }

    on_recycled(&state, &task, onlyne_session::CloseReason::Cancelled).expect("the close runs");

    let row = store.get_session(&task).unwrap().expect("the session row");
    assert_eq!(
        crate::session::dispatch::projection_of(&row, TaskState::Cancelled).lifecycle,
        onlyne_proto::Lifecycle::Exited,
        "a closed session reads exited on the row its client holds"
    );
}

/// Both commands' closes answer the row their session was still holding, each
/// with the word the operator gave.
///
/// The close drops the slot, and the delivery handle goes with it. A row this
/// client never answered is a row the server still reads as owed, so the pull
/// that would have taken it passes and the release of a session the server
/// judges gone hands it back to the queue — which dispatches the task again, as
/// the live run showed, every time the task was already settled. `cancel` and
/// `recycle` are the two commands whose close reaches this branch, and each
/// refusal reads the same word the settle fallback writes for the same command.
#[tokio::test]
async fn a_control_close_refuses_the_held_delivery_with_the_operators_word() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let state = DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        8,
        Arc::new(FakeBackend::new()),
        store.clone(),
    );

    for (key, reason, word) in [
        (
            "cancelled",
            onlyne_session::CloseReason::Cancelled,
            "operator cancel",
        ),
        (
            "recycled",
            onlyne_session::CloseReason::Operator,
            "operator recycle",
        ),
    ] {
        let task = new_task_id();
        let msg_id = format!("msg-{key}");
        {
            let mut inner = state.inner.lock();
            let mut held = slot(&task, false, None);
            held.msg_id = Some(msg_id.clone());
            inner.sessions.insert(key.to_string(), held);
        }

        on_recycled(&state, &task, reason).expect("the close runs");

        let acks: Vec<(bool, Option<String>)> = store
            .flush_order()
            .expect("read the intent queue")
            .iter()
            .map(|row| crate::runtime::intent::op_for_intent(row).expect("queued frame"))
            .filter_map(|op| match op {
                onlyne_proto::ClientOp::Ack(ack) if ack.msg_id == msg_id => {
                    Some((ack.accepted, ack.reason))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            acks,
            vec![(false, Some(word.to_string()))],
            "the {key} close refuses the row it still held with the word it was given"
        );
    }
}
