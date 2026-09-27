use super::*;
use crate::backend::fake::FakeBackend;
use onlyne_proto::new_task_id;
use tempfile::tempdir;

/// One dispatch lock and its role's state, empty but for the wiring a binding
/// case needs: a task's session staged on a fake backend.
fn state_with_slot(task: &str) -> (DispatchState, Arc<FakeBackend>) {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new("planner", dir.path(), Vec::new(), 8, backend.clone(), store);
    dispatch(
        &state,
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
    (state, backend)
}

/// One adapter connection over a socket nobody reads, with the peer held alive
/// so the channel stays open for the length of the case.
fn connection() -> (AdapterIo, tokio::io::DuplexStream) {
    let (stream, peer) = tokio::io::duplex(1024);
    (
        AdapterIo::new(stream, Duration::from_secs(5), Duration::from_secs(5)),
        peer,
    )
}

/// Two always-running agents wait, and the longer one waits first.
///
/// The park was one slot, so a second mount naming no session overwrote the
/// first and dropped its `AdapterIo` with no release, no log, and no word to the
/// plugin: the role quietly lost a worker that was sitting connected and
/// healthy. Two agents now hold two places in the queue, and the claim answers
/// them in the order they arrived — the agent that has waited longest is the one
/// that takes the session staged next, which is the promise §6 line 285 makes to
/// a plugin that attached before any work existed.
#[tokio::test]
async fn two_waiting_agents_hold_two_places_and_are_served_in_order() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    let (first, _first_peer) = connection();
    let (second, _second_peer) = connection();

    state.park_transport(first.clone(), vec![Capability::Report]);
    state.park_transport(second.clone(), vec![Capability::Report]);

    {
        let inner = state.inner.lock();
        assert_eq!(inner.parked.len(), 2, "both agents are still waiting");
        assert!(
            inner.parked[0].0.same_connection(&first) && inner.parked[1].0.same_connection(&second),
            "the queue keeps the order the mounts arrived in"
        );
    }

    let claimed = state
        .claim_parked_transport(&task)
        .expect("a waiting agent serves the staged session");
    assert!(
        claimed.0.same_connection(&first),
        "the agent that has waited longest takes the session"
    );
    {
        let inner = state.inner.lock();
        assert_eq!(inner.parked.len(), 1, "the other agent keeps waiting");
        assert!(
            inner.parked[0].0.same_connection(&second),
            "and it is the one that arrived second"
        );
        assert!(
            inner
                .transports
                .get(&task)
                .is_some_and(|(serving, _)| serving.same_connection(&first)),
            "the claim recorded the socket that took the session"
        );
    }
}

/// A claim the binding judgement refuses returns the waiting agent to the park
/// rather than spending it.
///
/// The refused agent used to leave the record entirely: `parked.take()` had
/// already spent the entry, and the connection was filed read-only under a
/// session it never mounted — so the role lost its waiting agent, and the
/// read-only entry it gained speaks of a mount that named nothing. A parked
/// agent that takes this session still answers the next one, and the sweep's
/// `held_read_only` reading is not handed a connection that is no agent's owner.
#[tokio::test]
async fn a_refused_claim_leaves_the_agent_waiting_and_records_nothing_read_only() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    let (serving, _serving_peer) = connection();
    let (waiting, _waiting_peer) = connection();
    state
        .inner
        .lock()
        .transports
        .insert(task.clone(), (serving.clone(), Vec::new()));
    state.park_transport(waiting.clone(), vec![Capability::Report]);

    assert!(
        state.claim_parked_transport(&task).is_none(),
        "a session another connection serves is not handed to a waiting agent"
    );

    let inner = state.inner.lock();
    assert_eq!(
        inner.parked.len(),
        1,
        "the refused agent is still this role's waiting agent"
    );
    assert!(
        inner.parked[0].0.same_connection(&waiting),
        "and it is the same connection, not a record of one"
    );
    assert!(
        inner.revived.is_empty(),
        "a claim refused for a session it never mounted records no read-only holder"
    );
    assert!(
        inner
            .transports
            .get(&task)
            .is_some_and(|(served, _)| served.same_connection(&serving)),
        "the connection that serves the session is left as the only transport"
    );
}

/// A binding that changes nothing renews nothing.
///
/// The judgement of §1 (b) runs on every hand-off as well as on every mount, and
/// `hand_staged` reaches it with the connection that ALREADY serves the session:
/// `on_ready` re-binds before it looks for a payload. The clear and the stamp
/// that follow a taken binding were therefore rewinding a session's death window
/// whenever the client itself re-ran the judgement — one liveness stamp for a
/// frame the agent never sent. A silent arm of the reconnect sweep reads that
/// stamp, so a zombie that keeps redialling and pushing a re-delivery through
/// could keep a session whose own agent stopped beating alive for good.
///
/// The returning agent's own mount is the other shape, and it keeps its answer:
/// a connection that was not this session's transport clears the grace clock it
/// came back inside and stamps the liveness the sweep now reads.
#[tokio::test]
async fn a_connection_already_serving_a_session_renews_nothing_about_it() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    let (serving, _serving_peer) = connection();
    let (returning, _returning_peer) = connection();
    let silent = Instant::now()
        .checked_sub(HEARTBEAT_INTERVAL * (HEARTBEAT_SILENCE_MARGIN + 1))
        .expect("a stamp past the silence window");
    state
        .inner
        .lock()
        .transports
        .insert(task.clone(), (serving.clone(), vec![Capability::Report]));
    state
        .inner
        .lock()
        .sessions
        .get_mut(&task)
        .expect("the slot")
        .last_beat = Some(silent);

    // The re-delivery path: same socket, already the session's transport.
    {
        let mut inner = state.inner.lock();
        assert!(
            note_binding_locked(&mut inner, &task, &serving),
            "the connection serving a session keeps it"
        );
        assert_eq!(
            inner.sessions[&task].last_beat,
            Some(silent),
            "a judgement that took nothing renews no liveness: the sweep still \
             reads this session as past its silence window"
        );
    }

    // The returning agent: a connection that was not the session's transport.
    {
        let mut inner = state.inner.lock();
        // The socket that served it went away: the transport is off the map and
        // the grace clock is running, exactly as `release_connection` leaves the
        // slot for an agent that redials.
        inner.transports.remove(&task);
        inner.sessions.get_mut(&task).expect("the slot").dropped_at = Some(silent);
        assert!(note_binding_locked(&mut inner, &task, &returning));
        let slot = &inner.sessions[&task];
        assert_eq!(
            slot.dropped_at, None,
            "the agent that came back inside the grace takes the clock off its session"
        );
        assert_ne!(
            slot.last_beat,
            Some(silent),
            "and the mount that took the session is the frame that proves it is here"
        );
    }
}

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
        causality: Causality::root(task.to_string()),
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
