use super::super::retire::release_locked;
use super::*;
use crate::backend::fake::FakeBackend;
use onlyne_proto::new_task_id;
use tempfile::{TempDir, tempdir};

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
/// A session owes its downstream edges, never the role it answers.
///
/// A completion is itself a delivery to the role that handed the task over, so
/// an entry naming that role — the self-addressed line `onlyne-client init`
/// prints and about twenty fixtures restate, and a ring's return edge — is not
/// an obligation the session could ever discharge. The exclusion is the origin
/// alone: a downstream edge beside it still owes its delivery.
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

/// One role whose resident session has a row, with two connections parked under
/// the names the caller declares.
///
/// The directory comes back with the state: the exit feeds the case judges land
/// in that store, and a closed one would refuse the writes.
fn resident_with_parked(
    session: &str,
    declared: [Option<&str>; 2],
) -> (DispatchState, TempDir, AdapterIo, AdapterIo) {
    let dir = tempdir().expect("tempdir");
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let state = DispatchState::new(
        "caerulea",
        dir.path(),
        Vec::new(),
        8,
        Arc::new(FakeBackend::new()),
        store,
    );
    let (first, _) = connection();
    let (second, _) = connection();
    state.park_transport(first.clone(), Vec::new(), declared[0]);
    state.park_transport(second.clone(), Vec::new(), declared[1]);
    assert_eq!(
        state.seed_resident_parked_session(Some(session)).as_deref(),
        Some(session),
        "the resident session's row is written before its exit is judged"
    );
    (state, dir, first, second)
}

/// One stored session row as the wire reads it, with the task state a resident
/// session's row is judged at.
fn row_projection(state: &DispatchState, session: &str) -> SessionProjection {
    let inner = state.inner.lock();
    let row = inner
        .store
        .get_session(session)
        .expect("the store answers")
        .expect("the session has a row");
    projection_of(&row, TaskState::Pending)
}

/// A parked connection ending is not the end of the session it declared.
///
/// The incident this case is drawn from: a task sub-session mounted naming the
/// session its parent was still serving, and its own detach wrote that
/// session's exit, because the park kept no record of the name the mount had
/// carried. Two connections declare the same session here, and the one that
/// leaves is not the session's owner.
#[tokio::test]
async fn a_leaving_parked_connection_does_not_end_the_session_another_one_declares() {
    let session = "caerulea-main";
    let (state, _dir, first, second) =
        resident_with_parked(session, [Some(session), Some(session)]);

    let retired = state.release_connection(Some(session), &first, false);

    assert!(
        retired.is_empty(),
        "a session another parked connection declares is not retired: {retired:?}"
    );
    let held = row_projection(&state, session);
    assert_eq!(
        held.agent,
        AgentPhase::Idle,
        "the agent is the one the other declarer speaks for"
    );
    assert_eq!(
        held.resource,
        ResourcePhase::Attached,
        "and its resource was never closed"
    );
    assert_eq!(
        held.lifecycle,
        Lifecycle::Idle,
        "so the row stays idle rather than exited"
    );
    let inner = state.inner.lock();
    assert_eq!(
        inner.parked.len(),
        1,
        "the other declarer keeps its place in the park"
    );
    assert!(inner.parked[0].io.same_connection(&second));
    assert_eq!(
        inner.parked[0].session.as_deref(),
        Some(session),
        "with the declaration it parked with"
    );
}

/// The last declaring connection leaving still writes the exit.
///
/// Suppressing the feed is not skipping it: the session ends when its last
/// declarer goes and no slot serves it, which is the judgement the park's
/// declaration narrows and never replaces.
#[tokio::test]
async fn the_last_declaring_connection_leaving_writes_the_resident_sessions_exit() {
    let session = "caerulea-main";
    let (state, _dir, first, second) =
        resident_with_parked(session, [Some(session), Some(session)]);

    assert!(
        state
            .release_connection(Some(session), &first, false)
            .is_empty(),
        "the session is still declared by the connection behind it"
    );
    let retired = state.release_connection(Some(session), &second, false);

    assert_eq!(
        retired,
        vec![session.to_string()],
        "the exit of the session its last declarer left is published"
    );
    let gone = row_projection(&state, session);
    assert_eq!(gone.agent, AgentPhase::Gone, "the resident agent is gone");
    assert_eq!(
        gone.lifecycle,
        Lifecycle::Exited,
        "and the row projects exited"
    );
}

/// A parked connection that declares another session does not hold this one.
#[tokio::test]
async fn a_parked_connection_declaring_another_session_does_not_hold_the_resident() {
    let session = "caerulea-main";
    let (state, _dir, first, second) =
        resident_with_parked(session, [Some(session), Some("task-other")]);

    let retired = state.release_connection(Some(session), &first, false);

    assert_eq!(
        retired,
        vec![session.to_string()],
        "the only declarer of this session left"
    );
    assert_eq!(
        row_projection(&state, session).lifecycle,
        Lifecycle::Exited,
        "a declaration of a different session keeps nothing alive"
    );
    let inner = state.inner.lock();
    assert_eq!(
        inner.parked.len(),
        1,
        "the unrelated connection is untouched"
    );
    assert!(inner.parked[0].io.same_connection(&second));
}

/// The owner of a resident row ends it even while it is serving a delivery.
///
/// The row is seeded at mount and has no slot, so no sweep reaches it, and a
/// claim has put the owner in `transports` under the delivery's name: this
/// release is the only door its exit comes through.
#[tokio::test]
async fn a_claimed_resident_owner_leaving_writes_the_session_its_mount_declared() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    let session = "caerulea-main";
    let (io, _peer) = connection();
    state.park_transport(io.clone(), Vec::new(), Some(session));
    assert_eq!(
        state.seed_resident_parked_session(Some(session)).as_deref(),
        Some(session),
        "the resident session's row is written before the delivery arrives"
    );
    assert!(
        state.claim_parked_transport(&task).is_some(),
        "the staged delivery takes the parked owner"
    );

    let retired = state.release_connection(Some(session), &io, false);

    assert_eq!(
        retired,
        vec![session.to_string()],
        "the session the mount declared is the one this release ends"
    );
    assert_eq!(
        row_projection(&state, session).lifecycle,
        Lifecycle::Exited,
        "and the exit is what its row projects"
    );
}

/// A settled delivery returns the owner to the park still declaring the alias.
///
/// The claim bound the connection under the delivery's name, so the park it
/// comes back to must keep the name its mount carried: a transient delivery key
/// recorded as a declaration would misread the next judgement and lose the
/// alias the role is known by.
#[tokio::test]
async fn a_re_parked_owner_keeps_the_alias_its_mount_declared() {
    let task = new_task_id();
    let (state, _backend) = state_with_slot(&task);
    let session = "caerulea-main";
    let (owner, _owner_peer) = connection();
    state.park_transport(owner.clone(), Vec::new(), Some(session));
    state
        .seed_resident_parked_session(Some(session))
        .expect("the resident session's row");
    assert!(
        state.claim_parked_transport(&task).is_some(),
        "the delivery takes the parked owner"
    );
    {
        let mut inner = state.inner.lock();
        let slot = inner.sessions.get_mut(&task).expect("the staged slot");
        slot.session.backend = "external".into();
        release_locked(&mut inner, &task, None).expect("the settled delivery retires its slot");
    }
    {
        let inner = state.inner.lock();
        assert_eq!(inner.parked.len(), 1, "the owner is back in the park");
        assert!(inner.parked[0].io.same_connection(&owner));
        assert_eq!(
            inner.parked[0].session.as_deref(),
            Some(session),
            "with the session its mount declared, not the delivery it served"
        );
    }

    // A second connection declaring the same session outlives the owner, and the
    // session ends with the last declarer.
    let (child, _child_peer) = connection();
    state.park_transport(child.clone(), Vec::new(), Some(session));
    assert!(
        state
            .release_connection(Some(session), &child, false)
            .is_empty(),
        "the parked owner still declares the session"
    );
    assert_eq!(
        row_projection(&state, session).lifecycle,
        Lifecycle::Idle,
        "so the row stays idle"
    );
    assert_eq!(
        state.release_connection(Some(session), &owner, false),
        vec![session.to_string()],
        "and the last connection that declared it ends the session"
    );
    assert_eq!(
        row_projection(&state, session).lifecycle,
        Lifecycle::Exited,
        "the exit lands once no declarer is left"
    );
}
