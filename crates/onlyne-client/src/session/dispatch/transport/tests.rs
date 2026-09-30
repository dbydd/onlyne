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
