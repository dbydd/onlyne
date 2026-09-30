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
        keeps_idle: false,
        family: None,
        idle_since: None,
        suspended: false,
        opened_at: Instant::now(),
        command: Vec::new(),
        resume_handle: None,
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
        tools_token: String::new(),
        delivered_roles: BTreeSet::new(),
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

/// The token a session mints is the whole binding of its `tools` mount: it
/// names that session and nothing else does, the session it names holds the
/// delivery the mount's frames are stamped from, and the token dies with the
/// slot (`docs/v2-CONTRACT.md` §3b).
#[tokio::test]
async fn a_tools_token_names_the_session_its_slot_serves_and_dies_with_it() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    let token = state
        .tools_token(&task)
        .expect("the session minted its token");
    assert_ne!(token, task, "the token is a capability, not the session id");
    assert_eq!(
        state.tools_token(&new_task_id()),
        None,
        "a session id nobody holds mints nothing",
    );

    let session = state
        .tools_mount_for(&token)
        .expect("the token names the session that minted it");
    assert_eq!(session.session_id, task);
    assert_eq!(
        state.tools_mount_for("token-nobody-minted"),
        None,
        "an unknown token names no session",
    );

    let (io, _peer) = connection();
    assert_eq!(
        state
            .bind_tools_mount(&token, io.clone())
            .map(|s| s.session_id),
        Some(task.clone()),
        "the live session takes the binding",
    );
    assert!(state.tools_connection_live(&io));
    let scope = state
        .tools_scope(&io)
        .expect("the bound connection speaks for one session");
    assert_eq!(scope.session_id, task);
    assert_eq!(
        scope.task_id.as_deref(),
        Some(task.as_str()),
        "the delivery the session serves is what a frame is stamped from",
    );

    state.release_tools_connection(&io);
    assert!(
        !state.tools_connection_live(&io),
        "a connection that let its binding go speaks for nothing",
    );
    state.bind_tools_mount(&token, io.clone()).expect("rebind");

    state.inner.lock().sessions.clear();
    assert!(
        state.tools_mount_for(&token).is_none(),
        "the token dies with the slot that minted it",
    );
    assert!(
        !state.tools_connection_live(&io),
        "and the connection bound to it stops speaking",
    );
    let refusal = DispatchState::tools_gone();
    let error = refusal.error.expect("the refusal carries its error");
    assert_eq!(error.code, onlyne_proto::ErrorCode::Unauthorized);
    assert_eq!(
        error.field.as_deref(),
        Some("token"),
        "the refusal names the field the caller fixes",
    );
}

/// A tools mount names nothing session-scoped, so the client stamps what the
/// frame speaks for from its own record: the task its `report` and `handoff`
/// frames settle, the sender its frames leave as, and the child a `task`-kind
/// send opens — which is the hop the family's ceiling measures
/// (`docs/v2-CONTRACT.md` §3b).
#[tokio::test]
async fn a_tools_frame_is_stamped_from_the_session_the_token_names() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    let token = state
        .tools_token(&task)
        .expect("the session minted its token");
    let (io, _peer) = connection();
    state.bind_tools_mount(&token, io.clone()).expect("bind");

    let mut named = String::new();
    state
        .stamp_tools_task(&io, &mut named)
        .expect("an empty task_id spells the session's own open task");
    assert_eq!(named, task);
    let mut foreign = new_task_id();
    let claimed = foreign.clone();
    let refusal = state
        .stamp_tools_task(&io, &mut foreign)
        .expect_err("a caller wrong about its own session is refused");
    assert_eq!(
        refusal.error.expect("the refusal carries its error").code,
        onlyne_proto::ErrorCode::Forbidden,
    );
    assert_eq!(
        foreign, claimed,
        "the refused frame's task_id is left as it arrived",
    );

    let mut envelope = new_envelope(
        MsgKind::Task,
        Principal::role("builder"),
        Principal::role("reviewer"),
        Body::text("carry it on"),
        Some(Causality::root(new_task_id())),
    )
    .expect("a task envelope the protocol accepts");
    // The session's own delivery carries a spent budget, which is what gives
    // the assertions below their teeth: a root takes none of it.
    {
        let mut inner = state.inner.lock();
        let key = inner
            .sessions
            .keys()
            .next()
            .cloned()
            .expect("the staged session");
        inner
            .sessions
            .get_mut(&key)
            .expect("the slot the token names")
            .causality
            .hop_budget = Some(0);
    }
    // The bridge supplies the recipient, the text, and the kind: nothing else.
    envelope.from = Principal::role("builder");
    envelope.op_id = None;
    envelope.causality = None;
    state
        .stamp_tools_send(&io, &mut envelope)
        .expect("the session's own record stamps the frame");
    assert_eq!(
        envelope.from,
        Principal::role("planner"),
        "the frame leaves as this client's own role, whatever the caller claimed",
    );
    // `handoff` continues a family; `send` starts one. This is the envelope the
    // plugin's own `sendEnvelope` mints for the same tool: a root with a fresh
    // task id and its own key, and not one figure of the server's family.
    let root = envelope.causality.expect("a task send opens a task");
    assert_ne!(root.task, task, "the recipient's task is a task of its own");
    assert_eq!(root.parent_task, None, "a new family has no parent");
    assert_eq!(root.hop, 0, "a new family starts at hop 0");
    assert_eq!(root.attempt, 0);
    assert_eq!(
        root.hop_budget, None,
        "no budget of the session's rides out"
    );
    assert!(
        envelope
            .op_id
            .as_deref()
            .is_some_and(|op_id| op_id.starts_with("o-")),
        "a task send carries the idempotency key the shape requires: {:?}",
        envelope.op_id,
    );

    // A note joins no family: no chain and no key, whatever the caller claimed.
    let mut note = new_envelope(
        MsgKind::Note,
        Principal::role("builder"),
        Principal::role("reviewer"),
        Body::text("a word"),
        None,
    )
    .expect("a note the protocol accepts");
    note.op_id = Some(onlyne_proto::new_op_id());
    note.causality = Some(Causality::root(new_task_id()));
    state
        .stamp_tools_send(&io, &mut note)
        .expect("a note is stamped by the same door");
    assert!(note.causality.is_none(), "{note:?}");
    assert!(note.op_id.is_none(), "{note:?}");

    // A spent budget stops a forward and never new work: the ceiling answers the
    // `handoff` door, and the task send above walked straight past it.
    let refusal = state
        .handoff_refusal(
            &io,
            &onlyne_proto::HandoffArgs {
                task_id: task.clone(),
                to: "reviewer".into(),
                text: "carry it on".into(),
                image: None,
            },
        )
        .expect("a child over the family's ceiling is refused");
    let error = refusal.error.expect("the refusal carries its error");
    assert_eq!(error.code, onlyne_proto::ErrorCode::Invalid);
    assert!(
        error.message.contains("hop budget"),
        "the refusal names the bound it would break: {}",
        error.message,
    );
}

/// The obligation is the role's `allowed_targets`, and the refusal names every
/// role still owed and the set the session actually delivered to.
///
/// This is the door the two drives reach: the client's own constraints answer
/// before a completion is applied, so the sentence below is what a model reads.
/// The live cluster case the contract's acceptance asks for — a real client and
/// a real server — is not this case; what is pinned here is the sentence's shape
/// and the transition from refused to accepted as deliveries land.
#[tokio::test]
async fn the_relay_guard_names_what_is_owed_and_lets_the_completion_through_once_it_is_paid() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    // The role's own edges: the same list the server gates the ACL on.
    {
        let mut inner = state.inner.lock();
        inner.required_targets = vec!["writer".to_string(), "auditor".to_string()];
    }
    let token = state
        .tools_token(&task)
        .expect("the session minted its token");
    let (io, _peer) = connection();
    state.bind_tools_mount(&token, io.clone()).expect("bind");

    let refusal = state
        .completion_refusal(Some(&io), &completion(&task))
        .expect("a session that owes a delivery may not report a terminal outcome");
    assert_eq!(
        refusal
            .error
            .expect("the refusal carries its error")
            .message,
        "relay guard: missing handoff to: writer, auditor (this session delivered to: none)",
    );

    deliver(&state, &io, "writer");
    let refusal = state
        .completion_refusal(Some(&io), &completion(&task))
        .expect("auditor is still owed, so the completion is still refused");
    assert_eq!(
        refusal
            .error
            .expect("the refusal carries its error")
            .message,
        "relay guard: missing handoff to: auditor (this session delivered to: writer)",
    );

    deliver(&state, &io, "auditor");
    assert!(
        state
            .completion_refusal(Some(&io), &completion(&task))
            .is_none(),
        "a session that delivered to every role it owes meets no refusal",
    );
}

/// A role that declares no target owes nothing: the empty list is the
/// empty-policy case, and its session's first completion is not refused.
#[tokio::test]
async fn a_role_that_declares_no_target_owes_nothing() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    {
        let inner = state.inner.lock();
        assert!(
            inner.required_targets.is_empty(),
            "a role with no declared target owes nothing"
        );
    }
    let token = state
        .tools_token(&task)
        .expect("the session minted its token");
    let (io, _peer) = connection();
    state.bind_tools_mount(&token, io.clone()).expect("bind");
    assert!(
        state
            .completion_refusal(Some(&io), &completion(&task))
            .is_none(),
        "no declared target is no obligation",
    );
}

/// One terminal completion for this task, in the shape the door measures.
fn completion(task: &str) -> onlyne_proto::Report {
    onlyne_proto::Report::Complete {
        task_id: task.to_string(),
        outcome: onlyne_proto::Outcome::Done,
        head: Some("done".to_string()),
        details: None,
        files: Vec::new(),
        reply_to: None,
        cluster_ref: None,
    }
}

/// One carried delivery to `role`, which is the guard's evidence.
fn deliver(state: &DispatchState, io: &AdapterIo, role: &str) {
    let mut envelope = new_envelope(
        MsgKind::Task,
        Principal::role("builder"),
        Principal::role(role),
        Body::text("carry it on"),
        Some(Causality::root(new_task_id())),
    )
    .expect("a task envelope the protocol accepts");
    state
        .stamp_tools_send(io, &mut envelope)
        .expect("the session's own record stamps the frame");
    state
        .plugin_send(io, &envelope)
        .expect("the client carries the delivery");
}

/// A session owes its downstream edges, never the role it answers.
///
/// A completion is itself a delivery to the role that handed the task over, so
/// an entry naming that role — the self-addressed line `onlyne-client init`
/// prints and about twenty fixtures restate, and a ring's return edge — is not
/// an obligation the session could ever discharge. The exclusion is the origin
/// alone: a downstream edge beside it still owes its delivery.
#[tokio::test]
async fn a_session_owes_its_downstream_edges_and_never_the_role_it_answers() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    // The session serves a task that arrived from `sender`, so that edge is the
    // origin: named on the list, and never owed.
    {
        let mut inner = state.inner.lock();
        inner.required_targets = vec!["sender".to_string()];
    }
    let token = state
        .tools_token(&task)
        .expect("the session minted its token");
    let (io, _peer) = connection();
    state.bind_tools_mount(&token, io.clone()).expect("bind");
    assert!(
        state
            .completion_refusal(Some(&io), &completion(&task))
            .is_none(),
        "the role that handed this session its task is not owed a second delivery",
    );

    // The same list with a downstream edge beside it: that one is owed, and the
    // origin is still not named as though it were.
    {
        let mut inner = state.inner.lock();
        inner.required_targets = vec!["sender".to_string(), "downstream".to_string()];
    }
    let refusal = state
        .completion_refusal(Some(&io), &completion(&task))
        .expect("a downstream edge is still owed");
    assert_eq!(
        refusal
            .error
            .expect("the refusal carries its error")
            .message,
        "relay guard: missing handoff to: downstream (this session delivered to: none)",
    );

    deliver(&state, &io, "downstream");
    assert!(
        state
            .completion_refusal(Some(&io), &completion(&task))
            .is_none(),
        "the downstream delivery settles the obligation",
    );
}

/// The self-addressed entry `onlyne-client init` prints owes nothing.
///
/// The task came from this client's own role and the list names it, which is the
/// shape a one-role workspace starts from: without the origin excluded, its
/// session could never report a terminal outcome at all.
#[tokio::test]
async fn a_self_addressed_entry_owes_nothing() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    {
        let mut inner = state.inner.lock();
        let key = inner
            .sessions
            .keys()
            .next()
            .cloned()
            .expect("the staged session");
        inner
            .sessions
            .get_mut(&key)
            .expect("the slot the task opened")
            .origin = Some(Principal::role("planner"));
        inner.required_targets = vec!["planner".to_string()];
    }
    let token = state
        .tools_token(&task)
        .expect("the session minted its token");
    let (io, _peer) = connection();
    state.bind_tools_mount(&token, io.clone()).expect("bind");
    assert!(
        state
            .completion_refusal(Some(&io), &completion(&task))
            .is_none(),
        "a one-role cluster's self-addressed list is not an obligation it could discharge",
    );
}
