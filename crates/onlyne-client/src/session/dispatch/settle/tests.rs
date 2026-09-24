use super::*;
use onlyne_proto::{
    Body, Causality, Envelope, Handoff, MsgKind, Outcome, Principal, new_envelope, new_task_id,
};
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use tempfile::{TempDir, tempdir};

fn task_envelope(task: &str) -> Envelope {
    new_envelope(
        MsgKind::Task,
        Principal::role("sender"),
        Principal::role("planner"),
        Body::text("settle this task"),
        Some(Causality::root(task.to_string())),
    )
    .expect("task envelope")
}

fn staged_state(dir: &TempDir, task: &str) -> (DispatchState, ClientStore, Arc<FakeBackend>) {
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        1,
        backend.clone(),
        store.clone(),
    );
    let envelope = task_envelope(task);
    let session = dispatch(&state, &envelope).expect("stage the task");
    assert_eq!(session.task_id, task);
    {
        let inner = state.inner.lock();
        feed_ready(&inner.bridge, &inner.store, task).expect("ready the task");
    }
    (state, store, backend)
}

fn queued_ops(store: &ClientStore) -> Vec<ClientOp> {
    store
        .flush_order()
        .expect("intent queue")
        .iter()
        .map(|row| crate::runtime::intent::op_for_intent(row).expect("queued op"))
        .collect()
}

fn relays(ops: &[ClientOp]) -> Vec<Envelope> {
    ops.iter()
        .filter_map(|op| match op {
            ClientOp::Send(envelope)
                if envelope.kind == MsgKind::Task && envelope.to == Principal::role("reviewer") =>
            {
                Some((**envelope).clone())
            }
            _ => None,
        })
        .collect()
}

fn projection_reports(ops: &[ClientOp]) -> Vec<onlyne_proto::SessionProjection> {
    ops.iter()
        .filter_map(|op| match op {
            ClientOp::Report(Report::Heartbeat {
                projection: Some(projection),
                ..
            }) => Some(projection.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_refused_replay_releases_the_session_and_keeps_the_first_verdict() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let (state, store, backend) = staged_state(&dir, &task);
    state.attach_msg_id(&task, "msg-first");
    on_out(
        &state,
        &task,
        Outcome::Done,
        Some("first head".into()),
        None,
        &[],
        SettleAuthority::ClientOwned,
    )
    .await
    .expect("first verdict");
    assert_eq!(
        store
            .task(&task)
            .expect("task record")
            .expect("opened task")
            .task_state,
        TaskState::Done
    );
    assert_eq!(
        store.out_head(&task).expect("out head"),
        Some("first head".into())
    );
    assert!(state.session_count() == 0);
    assert!(backend.sessions().is_empty());

    let envelope = task_envelope(&task);
    dispatch(&state, &envelope).expect("stage the replay");
    state.attach_msg_id(&task, "msg-replay");
    on_out(
        &state,
        &task,
        Outcome::Failed,
        Some("replayed head".into()),
        None,
        &[],
        SettleAuthority::ClientOwned,
    )
    .await
    .expect("refused verdict");

    assert_eq!(state.session_count(), 0, "the replayed slot returns");
    assert!(
        backend.sessions().is_empty(),
        "the replayed resource closes"
    );
    assert_eq!(
        store
            .task(&task)
            .expect("task record")
            .expect("opened task")
            .task_state,
        TaskState::Done,
        "the first verdict remains standing"
    );
    assert_eq!(
        store.out_head(&task).expect("out head"),
        Some("first head".into())
    );
    let ops = queued_ops(&store);
    assert_eq!(
        ops.iter()
            .filter(
                |op| matches!(op, ClientOp::Ack(ack) if ack.msg_id == "msg-first" && ack.accepted)
            )
            .count(),
        1
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, ClientOp::Ack(ack) if ack.msg_id == "msg-replay")),
        "the refused replay stores no delivery answer"
    );
    assert_eq!(
        ops.iter()
            .filter(|op| matches!(
                op,
                ClientOp::Send(envelope)
                    if envelope.kind == MsgKind::Completion
                        && envelope.causality.as_ref().is_some_and(|causality| causality.task == task)
            ))
            .count(),
        1,
        "the refused replay stores no second completion receipt"
    );
    let reports = projection_reports(&ops);
    assert_eq!(
        reports.len(),
        2,
        "each settlement publishes its client tuple"
    );
    let publish = reports.last().expect("replay publish");
    assert_eq!(publish.lifecycle, Lifecycle::Exited);
    assert_eq!(publish.agent, AgentPhase::Gone);
    assert_eq!(publish.resource, ResourcePhase::Closed);
    assert_eq!(publish.outcome, Some(Outcome::Done));
}

#[tokio::test]
async fn a_refused_replay_does_not_relay_its_handoff_again() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let (state, store, _backend) = staged_state(&dir, &task);
    let handoff = [Handoff {
        to_role: "reviewer".into(),
        text: Some("one handoff".into()),
    }];
    on_out(
        &state,
        &task,
        Outcome::Done,
        Some("first head".into()),
        None,
        &handoff,
        SettleAuthority::ClientOwned,
    )
    .await
    .expect("first verdict");
    assert_eq!(relays(&queued_ops(&store)).len(), 1);

    let envelope = task_envelope(&task);
    dispatch(&state, &envelope).expect("stage the replay");
    on_out(
        &state,
        &task,
        Outcome::Failed,
        Some("replayed head".into()),
        None,
        &handoff,
        SettleAuthority::ClientOwned,
    )
    .await
    .expect("refused verdict");

    assert_eq!(
        relays(&queued_ops(&store)).len(),
        1,
        "the first settlement owns the handoff relay"
    );
}

#[tokio::test]
async fn the_first_verdict_keeps_its_receipt_and_handoff_relay() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let (state, store, _backend) = staged_state(&dir, &task);
    state.attach_msg_id(&task, "msg-first");
    let handoff = [Handoff {
        to_role: "reviewer".into(),
        text: Some("first handoff".into()),
    }];
    on_out(
        &state,
        &task,
        Outcome::Done,
        Some("first head".into()),
        None,
        &handoff,
        SettleAuthority::ClientOwned,
    )
    .await
    .expect("first verdict");

    let ops = queued_ops(&store);
    assert!(ops.iter().any(|op| matches!(
        op,
        ClientOp::Ack(ack) if ack.msg_id == "msg-first" && ack.accepted
    )));
    assert!(ops.iter().any(|op| matches!(
        op,
        ClientOp::Send(envelope)
            if envelope.kind == MsgKind::Completion
                && envelope.causality.as_ref().is_some_and(|causality| causality.task == task)
    )));
    let relayed = relays(&ops);
    assert_eq!(relayed.len(), 1);
    assert_eq!(
        relayed[0].body.text.as_deref(),
        Some("handoff: first handoff")
    );
    assert_eq!(
        store.out_head(&task).expect("out head"),
        Some("first head".into())
    );
}

/// Every settled task answers its sender, including the turn that left no
/// result line. The protocol requires a body, so the empty answer travels as
/// an empty text field, and the receipt survives validation. A dropped
/// receipt strands the origin: it waits on a task the role has already
/// retired, which is how a ring stops mid-circle.
#[test]
fn a_settled_task_without_a_result_line_still_files_its_receipt() {
    let task = new_task_id();
    let quiet = completion_envelope(
        "planner",
        Some(Principal::role("reviewer")),
        &task,
        None,
        None,
    )
    .expect("an answer with nothing to say is still an answer");
    assert_eq!(quiet.kind, MsgKind::Completion);
    assert_eq!(quiet.causality.as_ref().unwrap().task, task);
    assert_eq!(quiet.body.text.as_deref(), Some(""));
    assert_eq!(
        quiet.to,
        Principal::role("reviewer"),
        "the receipt is addressed to the sender"
    );

    // A blank head and an absent one are the same answer to the sender.
    let blank = completion_envelope(
        "planner",
        Some(Principal::role("reviewer")),
        &task,
        Some(""),
        None,
    )
    .expect("a blank result line files too");
    assert_eq!(blank.body.text, quiet.body.text);

    let said = completion_envelope(
        "planner",
        Some(Principal::role("reviewer")),
        &task,
        Some("done"),
        None,
    )
    .expect("a result line travels verbatim");
    assert_eq!(said.body.text.as_deref(), Some("done"));

    // The one case that stays silent is the one with no sender to answer.
    assert!(
        completion_envelope("planner", None, &task, Some("done"), None).is_none(),
        "an unaddressed task files no receipt"
    );
}

/// A settled task inside a family answers its sender with the family's own
/// figures, so one run reads as one arc in `onlyne ledger`: the task rows and
/// the completion rows print the same family and the same depth.
#[test]
fn a_completion_carries_the_family_figures_of_the_task_it_answers() {
    let task = new_task_id();
    let root = new_task_id();
    let causality = Causality {
        task: task.clone(),
        parent_task: Some(root.clone()),
        reply_to: None,
        hop: 2,
        attempt: 0,
        family: Some(root.clone()),
        hop_budget: Some(7),
        origin: Some("_supervisor".into()),
        deadline: None,
        labels: Some(BTreeMap::from([("run".to_string(), "brief".to_string())])),
    };
    let receipt = completion_envelope(
        "planner",
        Some(Principal::role("reviewer")),
        &task,
        Some("done"),
        Some(&causality),
    )
    .expect("a settled task answers its sender");

    let link = receipt
        .causality
        .expect("the receipt carries the figures of the task it answers");
    assert_eq!(link.task, task);
    assert_eq!(link.hop, 2, "the receipt sits at the task's own depth");
    assert_eq!(link.family.as_deref(), Some(root.as_str()));
    assert_eq!(link.hop_budget, Some(7));
    assert_eq!(link.origin.as_deref(), Some("_supervisor"));
    assert_eq!(link.labels, causality.labels);
    assert_eq!(link.parent_task, None, "a receipt is no link in the chain");
    assert_eq!(link.reply_to, None);
}
