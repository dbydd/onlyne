use super::*;
use crate::backend::fake::FakeBackend;
use onlyne_proto::{
    Body, Causality, Envelope, Handoff, MsgKind, Outcome, Principal, new_envelope, new_task_id,
};
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

/// The never-ran guard reads the one word the row itself publishes.
///
/// A session row whose stored tuple bytes cannot be parsed rebuilds to `Booting`
/// while its `agent_state` column still carries the phase the last accepted write
/// left, and the two halves of the old reading disagreed at this door: the guard
/// refused the completion on the rebuild while the fault it wrote and the
/// projection it published both named the column — a session that demonstrably ran
/// a turn denied its own ending, and the refusal left the task open for a requeue
/// the answer had already come from.
#[tokio::test]
async fn a_guard_refuses_on_no_word_but_the_one_it_reports() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let (state, store, _backend) = staged_state(&dir, &task);
    store
        .upsert_session(
            &task,
            &onlyne_store::session::VersionedSession {
                agent_state: "running".into(),
                delivery_state: "pending".into(),
                resource_state: "attached".into(),
                recovery_substate: "none".into(),
                desired_json: "{}".into(),
                observed_json: "{ not an observation".into(),
                generation: 9,
                seq: 9,
                backend_ref: "{}".into(),
                mismatch_count: 0,
                updated_at: 0,
            },
        )
        .expect("the damaged row lands");

    on_out(
        &state,
        &task,
        Outcome::Done,
        Some("head".into()),
        None,
        &[],
        SettleAuthority::PluginReport,
    )
    .await
    .expect("the report is handled");

    assert_eq!(
        store
            .task(&task)
            .expect("task record")
            .expect("opened task")
            .task_state,
        TaskState::Done,
        "a row whose own column reads `running` has the turn the guard asks for"
    );
    assert!(
        store
            .list_faults(&task)
            .expect("the faults answer")
            .iter()
            .all(|fault| fault.kind != SETTLE_WITHOUT_TURN),
        "the door that reads the row it publishes files no refusal: {:?}",
        store.list_faults(&task).expect("the faults answer")
    );
}

/// A completion for a task this client holds no session row for is still a verdict.
///
/// `settle_task` writes the record of a task this process never dispatched for
/// exactly this reason, and the drain it used to run ahead of that write answers a
/// missing row with `unknown session` — so the report failed whole, the verdict
/// stayed unwritten, and the task was left open for a requeue that could never be
/// answered. The live case is a `client.db` replaced under a running role, or a
/// foreign task reported into one.
#[tokio::test]
async fn a_completion_with_no_session_row_still_files_its_verdict() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let (state, store, _backend) = staged_state(&dir, &task);
    let foreign = new_task_id();

    on_out(
        &state,
        &foreign,
        Outcome::Done,
        Some("a line".into()),
        None,
        &[],
        SettleAuthority::ClientOwned,
    )
    .await
    .expect("a report with nothing to drain is still settled");

    let record = store
        .task(&foreign)
        .expect("the store answers")
        .expect("the verdict wrote its own record");
    assert_eq!(record.task_state, TaskState::Done);
    assert!(record.settled_at.is_some(), "the task closed");
    assert_eq!(
        store.get_session(&foreign).expect("store answers"),
        None,
        "and no session row was invented to drain"
    );
}

/// Two held connections can mount under one session name, and the sweep that
/// answers them removes the connections it actually told to leave. A bye may not
/// overtake the response to a frame the plugin is still inside, and the entry of
/// the connection that keeps it is the one that stays in the buffer — dropping it
/// by name would leave a live socket that `release_connection` can no longer see,
/// held by neither the session nor the sweep.
#[tokio::test]
async fn a_held_connection_answering_its_own_frame_keeps_its_buffer_entry() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let (state, _store, _backend) = staged_state(&dir, &task);
    let (inside_stream, _inside_peer) = tokio::io::duplex(1024);
    let inside = AdapterIo::new(
        inside_stream,
        Duration::from_secs(5),
        Duration::from_secs(5),
    );
    let (outside_stream, _outside_peer) = tokio::io::duplex(1024);
    let outside = AdapterIo::new(
        outside_stream,
        Duration::from_secs(5),
        Duration::from_secs(5),
    );
    {
        let mut inner = state.inner.lock();
        inner.in_frame.push(inside.clone());
        for io in [&inside, &outside] {
            inner.revived.push((task.clone(), io.clone(), Vec::new()));
        }
    }

    on_out(
        &state,
        &task,
        Outcome::Done,
        Some("head".into()),
        None,
        &[],
        SettleAuthority::ClientOwned,
    )
    .await
    .expect("the completion answers both accounts");

    let inner = state.inner.lock();
    assert!(
        inner
            .revived
            .iter()
            .any(|(_, held, _)| held.same_connection(&inside)),
        "the connection still inside its own frame keeps its entry"
    );
    assert!(
        !inner
            .revived
            .iter()
            .any(|(_, held, _)| held.same_connection(&outside)),
        "the connection the sweep reached is the one it left the buffer for"
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
