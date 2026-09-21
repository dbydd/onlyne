use super::*;
use crate::runtime::runloop::test_support::test_state;
use onlyne_proto::{Body, Causality, Lifecycle, Principal, Report, new_envelope, new_task_id};
use onlyne_session::{
    Bridge, LifecycleEvent, SessionLedger, TaskState, apply_persist, next_version,
};

/// The plugin beat shape: the three dimensions a plugin can witness, with the
/// client's own dimensions as placeholders.
fn plugin_beat(agent: &str, delivery: &str, resource: &str) -> serde_json::Value {
    serde_json::json!({
        "version": { "generation": 1, "seq": 90 },
        "generation_live": true,
        "isolate_after": 1,
        "terminate_after": 3,
        "mismatch_count": 0,
        "agent": agent,
        "delivery": delivery,
        "resource": resource,
        "recovery": "none",
    })
}

/// A completion the server accepts closes the session's own drain, and the pair
/// that leaves behind is the exit the design names.
///
/// The receipt is the one fact this client cannot observe for itself: a
/// completion envelope may be on the wire, still queued, or already taken, and
/// only the server's answer says which. The flusher is the only place that
/// answer is seen, so routing it into the reducer is what turns
/// `DeliveryState::Accepted` from a claim the client made when it settled the
/// task into a fact it witnessed. With the verdict already `done`, the projection
/// reads `exited` through the done-and-accepted arm — no resource has to close
/// for it, and the agent is still sitting in the pane it was given.
#[tokio::test]
async fn an_accepted_completion_intent_exits_the_session_without_closing_its_resource() {
    let (state, store) = test_state(1, Vec::new());
    let task = new_task_id();
    dispatch::dispatch(
        &state.dispatch,
        &new_envelope(
            MsgKind::Task,
            Principal::role("sender"),
            Principal::role("planner"),
            Body::text("repair the failing widget"),
            Some(Causality::root(task.clone())),
        )
        .expect("task envelope"),
    )
    .expect("the delivery takes a session");
    dispatch::on_plugin_report(
        &state.dispatch,
        None,
        Report::Ready {
            task_id: task.clone(),
            session_id: task.clone(),
            generation: 1,
            seq: 0,
            cluster_ref: None,
        },
    )
    .await
    .expect("the ready report lands");

    // The completion is in flight: the drain is open, the verdict has landed, and
    // the resource the agent runs in is still attached. The drain is opened at
    // the reducer directly because no client path feeds `Complete` today —
    // `on_out` settles the task and writes the drain closed in the same breath,
    // so a session in this shape is the state the reducer defines rather than
    // one this client reaches on its own.
    let bridge = Bridge::new();
    let open = next_version(&store, &task).expect("the session's watermark");
    apply_persist(
        &bridge,
        &store,
        &task,
        &LifecycleEvent::Complete { v: open },
    )
    .expect("the completion drain opens");
    store
        .settle_task(&task, TaskState::Done)
        .expect("the verdict lands");
    let row = store
        .get_session(&task)
        .expect("read the session row")
        .expect("the seeded row");
    assert_eq!(row.delivery_state, "pending", "the drain is open");
    assert_eq!(row.resource_state, "attached", "the resource is attached");
    assert_eq!(
        dispatch::projection_of(&row, TaskState::Done).lifecycle,
        Lifecycle::Working,
        "a done task whose completion has not been receipted is still open work"
    );

    // The completion rides the durable queue, exactly as `on_out` leaves it while
    // the link is down, and the flusher's answer is the receipt.
    let receipt = new_envelope(
        MsgKind::Completion,
        Principal::role("planner"),
        Principal::role("sender"),
        Body::text("repaired"),
        Some(Causality::root(task.clone())),
    )
    .expect("completion envelope");
    state
        .dispatch
        .enqueue_outbound(&receipt)
        .expect("the completion rides the durable queue");
    let queued = state
        .intents
        .lock()
        .pending()
        .expect("read the durable queue")
        .into_iter()
        .find(|row| {
            crate::runtime::intent::completion_task_id(row).as_deref() == Some(task.as_str())
        })
        .expect("the completion is queued");

    note_intent_receipt(&state, &queued);

    let row = store
        .get_session(&task)
        .expect("read the session row")
        .expect("the seeded row");
    assert_eq!(
        row.delivery_state, "accepted",
        "the receipt closed the drain"
    );
    assert_eq!(row.agent_state, "ready", "the agent never left");
    assert_eq!(
        row.resource_state, "attached",
        "and neither did its resource"
    );
    assert_eq!(
        dispatch::projection_of(&row, TaskState::Done).lifecycle,
        Lifecycle::Exited,
        "done and accepted is the exit the design names: {row:?}"
    );

    // The next beat is authority about the agent and the resource and nothing
    // else, so the receipt the reducer accepted has to come through it: the
    // dimension is the client's own, read back out of the row this reducer wrote.
    dispatch::on_plugin_report(
        &state.dispatch,
        None,
        Report::Heartbeat {
            task_id: task.clone(),
            session_id: task.clone(),
            generation: 1,
            seq: 90,
            observed: plugin_beat("running", "none", "attached"),
            projection: None,
            cluster_ref: None,
        },
    )
    .await
    .expect("the beat is handled");
    let row = store
        .get_session(&task)
        .expect("read the session row")
        .expect("the seeded row");
    assert_eq!(
        row.delivery_state, "accepted",
        "a plugin claiming no intent does not close the client's drain"
    );
    assert_eq!(row.agent_state, "running", "the agent is the plugin's");
    assert_eq!(row.resource_state, "attached", "and so is the resource");
    assert_eq!(
        dispatch::projection_of(&row, TaskState::Done).lifecycle,
        Lifecycle::Exited,
        "the receipt survives the beat: {row:?}"
    );
}
