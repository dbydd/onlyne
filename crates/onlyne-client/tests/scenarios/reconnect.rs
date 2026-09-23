//! A connection that comes back: the reconnect grace retiring a ghost, a returning agent
//! keeping its session, the spawn whose plugin never dials at all, and the read-only
//! connection whose sends merge into the completion.

use crate::common::{
    ReasonBackend, RecordingOutbox, assert_settled, complete_plugin, complete_raw_plugin, deliver,
    eventually, mount_plugin, mount_raw_plugin, plugin_beat, published_projection, sample_envelope,
    serve_role_socket, task_delivery,
};
use onlyne_adapter::{AdapterIo, WireMessage};
use onlyne_client::session::dispatch::{DispatchState, on_out};
use onlyne_frame::{read_frame, write_frame};
use onlyne_proto::{
    AdapterMsg, AgentMount, Capability, ClientOp, Handoff, HelloArgs, HostOp, Lifecycle, Mount,
    MountKind, MsgKind, Outcome, PROTOCOL_VERSION, PluginOp, Report,
};
use onlyne_session::SessionLedger;
use onlyne_store::ClientStore;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Mount one plugin on the role socket and record every host frame it receives.
///
/// `mount_plugin` keeps the assignments alone. The reconnect cases below also
/// have to see a `bye`, because which frames a returning agent is handed — and
/// which end it — is the fact the grace window and the merge are judged on.
async fn witnessed_plugin(
    socket: &Path,
    session: &str,
) -> (AdapterIo, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let stream = onlyne_layout::connect_local(socket)
        .await
        .expect("the role socket accepts a plugin");
    let (io, mut inbound) =
        AdapterIo::new_with_inbound(stream, Duration::from_secs(5), Duration::from_secs(5));
    let (frames_tx, frames_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(frame) = inbound.recv().await {
            match frame.msg {
                AdapterMsg::Host(HostOp::Assign(assign)) => {
                    let _ = frames_tx.send(format!("assign:{}", assign.task_id));
                }
                AdapterMsg::Host(HostOp::Bye(bye)) => {
                    let _ = frames_tx.send(format!("bye:{}", bye.reason));
                }
                _ => {}
            }
        }
    });
    let hello = HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: "onlyne-agent-test".into(),
        version: "1.0.0".into(),
        kind: MountKind::Agent,
        capabilities: vec![Capability::Report, Capability::Inject, Capability::Recycle],
        mount: Some(Mount::Agent(AgentMount {
            role: "planner".into(),
            session: Some(session.to_string()),
            task_id: Some(session.to_string()),
            pid: None,
        })),
    };
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Hello(hello)))
        .await
        .expect("the mount answers");
    assert!(body.ok, "the role socket admits this plugin: {body:?}");
    (io, frames_rx)
}

/// The reconnect grace retires a ghost whose agent never came back.
///
/// A connection that ends without a `detach` frame keeps its session and its
/// resource so a restarting agent finds the work it was doing. That promise has
/// to expire: the field left roles holding a slot, a projected `idle` row, and a
/// live pane for a process that was simply gone, and on `max_sessions = 1` one
/// such ghost stops every later delivery on that role. The sweep past
/// `[client] reconnect_grace_secs` is what ends it, and it closes the resource
/// with the reason the settled task already earned.
#[tokio::test]
async fn the_reconnect_grace_retires_a_ghost_whose_agent_never_returns() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store,
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let task_id = deliver(&state, &task_delivery("task A")).await;
    let mut stream = mount_raw_plugin(&socket, &task_id).await;
    complete_raw_plugin(&mut stream, &task_id, Outcome::Done).await;
    drop(stream);
    eventually(
        || state.session_transport(&task_id).is_none(),
        "the dropped connection binding to clear",
    )
    .await;

    // Inside the window nothing leaves: this is the reconnect an always-running
    // agent lives in, and retiring here would drop the resource it returns to.
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(30), 60)
            .len(),
        0,
        "a ghost inside the reconnect grace is kept"
    );
    assert_eq!(state.session_count(), 1, "the session stays tracked");
    assert!(
        backend.closed_sessions.lock().is_empty(),
        "no resource closes inside the window"
    );

    // Past the window the ghost is retired and its capacity spent.
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(61), 60)
            .len(),
        1,
        "a ghost past the reconnect grace retires"
    );
    assert_eq!(backend.closed_sessions.lock()[0].task_id, task_id);
    assert_eq!(
        backend.reasons.lock().as_slice(),
        [onlyne_session::CloseReason::Completed],
        "the close carries the reason the settled task earned"
    );
    assert_eq!(
        state.session_count(),
        0,
        "the retired ghost spends the role's only capacity slot"
    );
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(120), 60)
            .len(),
        0,
        "a retired ghost is not swept twice"
    );
    // A closed window disables the sweep entirely, the same spelling as every
    // other `[client] *_secs` knob.
    assert_eq!(state.retire_dropped_ghosts(Instant::now(), 0).len(), 0);
    host.abort();
}

/// The reconnect grace takes a session whose task is still open.
///
/// A plugin that dies mid-task leaves work owed, and the retry that would have
/// answered it never arrives, so this window is the only thing left to notice.
/// The sweep feeds the agent-gone event: the session reaches `Exited` through
/// `AgentState::Gone`, not through a task result, and the resource closes with
/// the reason the open task earns — a `Fault`, because the work was owed when its
/// agent went. What the work ended as is this sweep's to write: the agent that
/// would have reported the ending is the one that left, so the task settles
/// `failed` — the same word the close reason carries — and a task nobody answers
/// would otherwise stay open for the server to re-offer forever. A verdict that
/// already landed is not overwritten: `settle_task` moves only an unsettled row.
#[tokio::test]
async fn the_reconnect_grace_takes_a_ghost_whose_task_is_still_open() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    // The connection dies mid-task, without a `detach` frame and without a
    // verdict: the session keeps its slot and its task binding inside the window.
    let task_id = deliver(&state, &task_delivery("task A")).await;
    let stream = mount_raw_plugin(&socket, &task_id).await;
    drop(stream);
    eventually(
        || state.session_transport(&task_id).is_none(),
        "the dropped connection binding to clear",
    )
    .await;
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(30), 60)
            .len(),
        0,
        "a bound ghost inside the reconnect grace is kept"
    );
    assert_eq!(state.session_count(), 1, "the bound session stays tracked");

    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(61), 60)
            .len(),
        1,
        "a bound ghost past the reconnect grace is this sweep's to take"
    );

    let row = store
        .get_session(&task_id)
        .unwrap()
        .expect("the retired ghost keeps its row");
    let observed = onlyne_session::stored_observation(&store, Some(&row));
    assert_eq!(
        observed.agent,
        onlyne_session::AgentState::Gone,
        "the tuple's agent dimension is gone: {row:?}"
    );
    assert_eq!(
        published_projection(&store, &task_id).lifecycle,
        Lifecycle::Exited,
        "the session of an owed task reads exited"
    );
    // Which of the two routes put the agent there is read off the ledger's trail:
    // the resource close alone also kills the agent fact it hosted, so the tuple
    // cannot tell them apart, and the event the sweep feeds is what says the
    // process, not the close, was the observation.
    let trail: Vec<String> = store
        .events_since(0, 100)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "lifecycle")
        .filter(|event| event.data["task_id"] == serde_json::json!(task_id))
        .map(|event| event.data["event"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        trail.ends_with(&["agent_gone".to_string(), "resource_closed".to_string()]),
        "the sweep records the agent gone before it closes the resource: {trail:?}"
    );
    let record = store
        .task(&task_id)
        .unwrap()
        .expect("the task keeps its own record");
    assert_eq!(
        record.task_state,
        onlyne_session::TaskState::Failed,
        "the sweep settles the work its agent left owed: {record:?}"
    );
    assert!(
        record.settled_at.is_some(),
        "and the row reads settled: {record:?}"
    );
    assert_eq!(
        backend.closed_sessions.lock()[0].task_id,
        task_id,
        "the ghost's resource is the one closed"
    );
    assert_eq!(
        backend.closed_sessions.lock()[0]
            .backend_ref
            .get("refreshed"),
        Some(&serde_json::Value::Bool(true)),
        "the retirement closes the handle its attach refreshed, not the stored one"
    );
    assert_eq!(
        backend.reasons.lock().as_slice(),
        [onlyne_session::CloseReason::Fault],
        "the close carries the fault the owed work earns"
    );
    assert_eq!(
        state.session_count(),
        0,
        "the retired ghost spends the role's only capacity slot"
    );
    host.abort();
}

/// An agent that reconnects inside the grace window keeps its session and clears
/// the clock.
///
/// The regression this guards is the sweep mistaking a returning agent for a
/// ghost: a role whose always-running plugin restarts (the shape an agent upgrade
/// leaves behind) must not lose the session it held. The mount that arrives
/// inside the window clears the stamp, so no later sweep — however far past the
/// window it reads the clock — has anything to retire.
#[tokio::test]
async fn an_agent_that_reconnects_inside_the_window_keeps_its_session() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store,
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    // The agent's first connection dies without a detach: a raw stream drop is
    // real EOF, where dropping an `AdapterIo` only closes one sender.
    let first_task = deliver(&state, &task_delivery("task A")).await;
    let mut stream = mount_raw_plugin(&socket, &first_task).await;
    complete_raw_plugin(&mut stream, &first_task, Outcome::Done).await;
    drop(stream);
    eventually(
        || state.session_transport(&first_task).is_none(),
        "the dropped connection binding to clear",
    )
    .await;

    // Inside the window nothing leaves: this is the reconnect an always-running
    // agent lives in, and retiring here would drop the resource it returns to.
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(30), 60)
            .len(),
        0,
        "a ghost inside the reconnect grace is kept"
    );

    // The same agent comes back before the window ends, on the session it had.
    let (io, mut reconnected) = mount_plugin(&socket, Some(&first_task)).await;
    // The returning agent cleared the clock: even a sweep that reads the clock an
    // hour ahead finds nothing to retire.
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(3600), 60)
            .len(),
        0,
        "a reconnect inside the window leaves no ghost behind"
    );
    assert_eq!(state.session_count(), 1, "the session was not retired");
    assert!(
        backend.closed_sessions.lock().is_empty(),
        "its resource stays open"
    );

    let second_task = deliver(&state, &task_delivery("task B")).await;
    assert_ne!(second_task, first_task);
    assert!(
        backend.inner.sessions().contains_key(&second_task),
        "the next task spawns a session of its own"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), reconnected.recv())
            .await
            .is_err(),
        "the reconnected session is handed no later task"
    );
    drop(io);
    host.abort();
}

/// A connection that returns for a session another connection already serves is
/// held read-only, and what it sends travels with the completion that answered the
/// task.
///
/// The shape is a plugin that redialed while the client still holds the socket
/// behind it, or a restarted process that mounts under the id its session was born
/// with. Handing it the assignment would put two answers on one task, so the
/// returning connection gets nothing, and its `send` is held rather than put on the
/// wire. When the task settles, the two accounts leave as one relay per downstream
/// role, each line marked with the session that wrote it, and the returning
/// connection is told to leave.
#[tokio::test]
async fn a_connection_that_returns_for_a_taken_session_is_held_and_merged() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        2,
        backend.clone(),
        store.clone(),
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    // Task A runs on the connection its own plugin mounted with.
    let first_task = deliver(&state, &task_delivery("task A")).await;
    let stream = mount_raw_plugin(&socket, &first_task).await;

    // The same session id mounts a second time.
    let (io_zombie, mut witnessed) = witnessed_plugin(&socket, &first_task).await;
    assert!(
        witnessed.try_recv().is_err(),
        "a returning connection for a served session is handed no assignment"
    );
    assert!(
        state.session_transport(&first_task).is_some(),
        "the session stays served by the connection that came first"
    );
    let queued_before = store.flush_order().unwrap().len();
    let body = io_zombie
        .request(AdapterMsg::Plugin(PluginOp::Send(Box::new(
            sample_envelope("reviewer", "the part I had already written"),
        ))))
        .await
        .expect("the held send is answered");
    assert!(body.ok, "the held send is not refused: {body:?}");
    assert_eq!(
        body.data.as_ref().and_then(|data| data.get("held")),
        Some(&serde_json::Value::Bool(true)),
        "the answer says the frame was held, not sent: {body:?}"
    );
    assert_eq!(
        store.flush_order().unwrap().len(),
        queued_before,
        "a held send leaves nothing in the outbound queue"
    );
    let relays = |frames: Vec<ClientOp>| -> Vec<String> {
        frames
            .into_iter()
            .filter_map(|op| match op {
                ClientOp::Send(envelope) => Some(envelope),
                _ => None,
            })
            .filter(|envelope| {
                envelope.kind == MsgKind::Task
                    && envelope.to == onlyne_proto::Principal::role("reviewer")
            })
            .map(|envelope| envelope.body.text.unwrap_or_default())
            .collect()
    };
    assert!(
        relays(outbox.frames().await).is_empty(),
        "nothing reaches the downstream role before the task is answered"
    );
    assert!(
        state.session_transport(&first_task).is_some(),
        "the session the returning connection named is still served"
    );

    outbox.clear().await;
    // The connection that owns the task answers it, so the held lines travel
    // beside the ones the session wrote.
    let handoffs = [Handoff {
        to_role: "reviewer".into(),
        text: Some("the part the task wrote".into()),
    }];
    on_out(
        &state,
        &first_task,
        Outcome::Done,
        Some("the part the task wrote".into()),
        None,
        &handoffs,
    )
    .await
    .expect("the task settles");

    let relayed = relays(outbox.frames().await);
    assert_eq!(
        relayed.as_slice(),
        ["handoff: [retry] the part the task wrote\n[zombie] the part I had already written"],
        "the two accounts leave as one relay for the downstream role"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), witnessed.recv())
            .await
            .expect("the merged handoff retires the returning connection")
            .as_deref()
            .map(|line| line.starts_with("bye:")),
        Some(true),
        "the read-only connection is told to leave, and was never assigned"
    );
    assert!(
        backend.closed_sessions.lock().is_empty(),
        "the session that answered the task is left standing"
    );
    assert_eq!(state.session_count(), 1, "one session serves the role");
    assert_settled(&store, &first_task);
    drop(stream);
    host.abort();
}

/// A read-only connection's own completion is answered before any bye reaches it.
///
/// `adapter_socket` runs the report handler to completion and only then answers the
/// frame, so a bye written inside that handler left ahead of the response. The pi
/// plugin's bye handler drops the socket and rejects every request still awaiting
/// an answer, which turned completions the ledger already held into failures the
/// agent reported again: three terminal writes for one task on the live ring, with
/// a `heartbeat_after_complete` fault beside them.
#[tokio::test]
async fn a_read_only_completion_is_answered_before_any_bye() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        2,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    // One session, served by the connection that mounted it.
    let task_id = deliver(&state, &task_delivery("task A")).await;
    let (_serving_io, mut assigns) = mount_plugin(&socket, Some(&task_id)).await;
    assert_eq!(assigns.recv().await.as_deref(), Some(task_id.as_str()));

    // A second connection comes back for the session the first one serves: it is
    // held read-only, and it files the completion anyway.
    let (io_returning, mut witnessed) = witnessed_plugin(&socket, &task_id).await;
    assert!(
        witnessed.try_recv().is_err(),
        "the returning connection is handed nothing at mount"
    );
    let body = io_returning
        .request(AdapterMsg::Plugin(PluginOp::Report(Report::Complete {
            task_id: task_id.clone(),
            outcome: Outcome::Done,
            head: Some("the returning half".into()),
            reply_to: None,
            cluster_ref: None,
        })))
        .await
        .expect("the completion report is answered");
    assert!(body.ok, "the completion is accepted: {body:?}");
    assert_settled(&store, &task_id);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), witnessed.recv())
            .await
            .is_err(),
        "no bye reaches the connection that just answered for this task: its own agent \
         treats one as the socket dying mid-request and reports the completion again"
    );
    host.abort();
}

/// A state frame is the serving connection's, and a read-only one's is refused.
///
/// The beat names the session and nothing else, so a connection that mounted the
/// same id after a newer one had taken the task used to have its observation
/// applied verbatim against the session the live connection owns: the agent
/// dimension, the resource, and the liveness watermark of a session it is not the
/// transport of. Provenance is what decides it now — the frame carries its sender
/// — and a connection the client holds read-only is served no state. Its beat is
/// still answered, because a plugin reads a failed report as a link that died and
/// sends the same thing again, and it leaves the tuple exactly where the serving
/// connection left it.
#[tokio::test]
async fn a_state_frame_from_a_connection_held_read_only_leaves_the_tuple_alone() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        2,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let task_id = deliver(&state, &task_delivery("task A")).await;
    let (io_serving, mut assigns) = witnessed_plugin(&socket, &task_id).await;
    assert_eq!(
        assigns.recv().await.as_deref(),
        Some(format!("assign:{task_id}").as_str()),
        "the connection that mounted first serves the session"
    );

    // The same session id mounts a second time: held read-only, handed nothing.
    let (io_held, mut witnessed) = witnessed_plugin(&socket, &task_id).await;
    assert!(
        witnessed.try_recv().is_err(),
        "a returning connection for a served session is handed no assignment"
    );
    let before = store
        .get_session(&task_id)
        .unwrap()
        .expect("the staged session keeps its row");

    // One frame, sent from each connection in turn: the two dimensions only the
    // plugin can witness, under a version the row has never seen.
    let beat = || {
        plugin_beat(
            &task_id,
            1,
            1005,
            serde_json::json!({
                "version": { "generation": 1, "seq": 1005 },
                "generation_live": true,
                "isolate_after": 1,
                "terminate_after": 3,
                "mismatch_count": 0,
                "agent": "idle",
                "delivery": "none",
                "resource": "attached",
                "recovery": "none",
            }),
        )
    };
    let body = io_held
        .request(AdapterMsg::Plugin(PluginOp::Report(beat())))
        .await
        .expect("the refused beat is answered");
    assert!(body.ok, "a refused beat is not a failed frame: {body:?}");
    assert_eq!(
        store.get_session(&task_id).unwrap().expect("row"),
        before,
        "a beat from a connection held read-only leaves the stored tuple as it was"
    );

    let body = io_serving
        .request(AdapterMsg::Plugin(PluginOp::Report(beat())))
        .await
        .expect("the serving connection's beat is answered");
    assert!(
        body.ok,
        "the serving connection's beat is applied: {body:?}"
    );
    let moved = store.get_session(&task_id).unwrap().expect("row");
    assert_ne!(
        moved, before,
        "the same frame from the session's own connection moves the tuple"
    );
    assert_eq!(
        moved.agent_state, "idle",
        "the dimension only the plugin can witness landed: {moved:?}"
    );
    host.abort();
}

/// A session whose plugin never mounted is a ghost from the moment it is staged.
///
/// `dispatch` stages a slot and spawns the resource, and the plugin spawned for
/// it is the only thing that will ever send a heartbeat. A spawn whose plugin
/// never dials — a command that died before it spoke, an agent whose mount never
/// came — leaves a slot no mount will clear, and the connection that would have
/// reported the ending never existed, so the drop path cannot start its clock
/// either. The same window covers it: the clock starts when there is no heartbeat
/// to read, which for a session not yet mounted is the moment it is born.
#[tokio::test]
async fn a_session_whose_plugin_never_mounts_retires_past_the_grace() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store,
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));

    // Staged and spawned; no connection ever names this session.
    let task_id = deliver(&state, &task_delivery("task A")).await;
    assert!(
        state.session_transport(&task_id).is_none(),
        "no plugin mounted for the staged session"
    );

    // Inside the window nothing leaves: a mount that is merely late still finds
    // the slot and the resource it was spawned for.
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(30), 60)
            .len(),
        0,
        "a session inside the reconnect grace is kept"
    );
    assert_eq!(state.session_count(), 1, "the staged session stays tracked");
    assert!(
        backend.closed_sessions.lock().is_empty(),
        "no resource closes inside the window"
    );

    // Past the window the plugin that never came is an agent that is gone, and
    // the slot is swept exactly as a dropped connection's is.
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(61), 60)
            .len(),
        1,
        "a session whose plugin never mounted is due when the window expires"
    );
    assert_eq!(
        backend.closed_sessions.lock()[0].task_id,
        task_id,
        "the ghost's resource is the one closed"
    );
    assert_eq!(
        backend.reasons.lock().as_slice(),
        [onlyne_session::CloseReason::Fault],
        "the close carries the fault the task no agent ever answered earns"
    );
    assert_eq!(
        state.session_count(),
        0,
        "the retired session spends the role's only capacity slot"
    );
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(120), 60)
            .len(),
        0,
        "a retired session is not swept twice"
    );
}

/// The three dimensions a plugin can witness, as the shape it puts on the wire.
fn beat_body(seq: u64, agent: &str) -> serde_json::Value {
    serde_json::json!({
        "version": { "generation": 1, "seq": seq },
        "generation_live": true,
        "isolate_after": 1,
        "terminate_after": 3,
        "mismatch_count": 0,
        "agent": agent,
        "delivery": "none",
        "resource": "attached",
        "recovery": "none",
    })
}

/// Open one raw plugin connection and stop at the welcome.
///
/// `mount_raw_plugin` waits for an assignment, and a connection that is held
/// read-only — or one that returns to a session whose payload has already been
/// handed over — is handed none, so a test that waited here would wait forever.
async fn raw_mount(socket: &Path, session: &str) -> onlyne_layout::LocalStream {
    let mut stream = onlyne_layout::connect_local(socket)
        .await
        .expect("the role socket accepts a raw plugin");
    let hello = HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: "onlyne-agent-raw-test".into(),
        version: "1.0.0".into(),
        kind: MountKind::Agent,
        capabilities: vec![Capability::Report, Capability::Inject, Capability::Recycle],
        mount: Some(Mount::Agent(AgentMount {
            role: "planner".into(),
            session: Some(session.to_string()),
            task_id: Some(session.to_string()),
            pid: None,
        })),
    };
    write_frame(
        &mut stream,
        &WireMessage {
            id: Some(1),
            reply_to: None,
            msg: AdapterMsg::Plugin(PluginOp::Hello(hello)),
        },
    )
    .await
    .unwrap();
    let welcome = read_frame::<_, WireMessage>(&mut stream)
        .await
        .unwrap()
        .expect("the raw plugin receives welcome");
    assert!(
        matches!(&welcome.msg, AdapterMsg::Res(body) if body.ok),
        "the raw plugin is admitted: {:?}",
        welcome.msg
    );
    stream
}

/// One beat on a raw plugin stream, and the answer the role socket gave it.
async fn raw_beat(
    stream: &mut onlyne_layout::LocalStream,
    task_id: &str,
    seq: u64,
    agent: &str,
) -> onlyne_proto::ResBody {
    write_frame(
        stream,
        &WireMessage {
            id: Some(3),
            reply_to: None,
            msg: AdapterMsg::Plugin(PluginOp::Report(plugin_beat(
                task_id,
                1,
                seq,
                beat_body(seq, agent),
            ))),
        },
    )
    .await
    .unwrap();
    loop {
        let frame = read_frame::<_, WireMessage>(stream)
            .await
            .unwrap()
            .expect("the beat is answered");
        if frame.reply_to == Some(3) {
            let AdapterMsg::Res(body) = frame.msg else {
                panic!("a beat is answered with a response: {:?}", frame.msg);
            };
            return body;
        }
    }
}

/// A plugin that re-mounts inside the grace window has its next heartbeat
/// applied, and a reporter from a connection the client holds read-only still
/// does not.
///
/// The regression: a plugin restarts its own sequence at its base and reports a
/// constant generation, while the watermark the row holds is the last sequence
/// the process that left reached. Nothing rebased it, so every frame the
/// returning reporter sent read at or below that watermark and was dropped as a
/// stale duplicate — for a session that had been running a while, the whole rest
/// of its work, reported into a tuple that never moved and a projection that
/// never refreshed.
///
/// The re-mount now rebases the watermark onto a new generation, and the beat's
/// version takes the generation the session's tuple holds rather than the
/// plugin's constant, so the returning reporter's next frame lands at once.
///
/// The other half is the constraint that decides the mechanism: lowering the
/// watermark must not admit a reporter the client holds read-only. That
/// connection never reaches the reducer at all — provenance is checked first —
/// and the frame below sits above every watermark the row has ever held, so the
/// provenance check is the only thing that can be stopping it.
#[tokio::test]
async fn a_re_mounting_plugin_inside_the_grace_has_its_next_heartbeat_applied() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        2,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let task_id = deliver(&state, &task_delivery("task A")).await;
    let mut first = mount_raw_plugin(&socket, &task_id).await;
    // Two beats, so the watermark the first process reaches stands strictly past
    // the base a restarted reporter starts from.
    assert!(
        raw_beat(&mut first, &task_id, 1001, "running").await.ok,
        "the serving plugin's beat is applied"
    );
    assert!(
        raw_beat(&mut first, &task_id, 1002, "idle").await.ok,
        "and the next one after it"
    );
    let row = store.get_session(&task_id).unwrap().expect("row");
    assert_eq!(
        (row.generation, row.seq),
        (1, 1002),
        "the watermark is the reporter's own: {row:?}"
    );

    // The agent's process goes away without a detach frame, and comes back
    // inside the window with its sequence starting over at its base.
    drop(first);
    eventually(
        || state.session_transport(&task_id).is_none(),
        "the dropped connection binding to clear",
    )
    .await;
    let mut returning = raw_mount(&socket, &task_id).await;
    assert!(
        state.session_transport(&task_id).is_some(),
        "the returning mount took the session rather than being held read-only"
    );

    let body = raw_beat(&mut returning, &task_id, 1001, "running").await;
    assert!(
        body.ok,
        "the returning reporter's beat is answered: {body:?}"
    );
    let row = store.get_session(&task_id).unwrap().expect("row");
    assert_eq!(
        (row.generation, row.seq),
        (2, 1001),
        "the beat landed on the rebased watermark: {row:?}"
    );
    assert_eq!(
        row.agent_state, "running",
        "the dimension only the plugin can witness landed: {row:?}"
    );

    // The same session id mounts a third time, while the returning connection
    // serves it: held read-only, and its beat is above every watermark the row
    // has held, so only provenance can be refusing it.
    let mut held = raw_mount(&socket, &task_id).await;
    let body = raw_beat(&mut held, &task_id, 2000, "gone").await;
    assert!(body.ok, "a refused beat is not a failed frame: {body:?}");
    let after = store.get_session(&task_id).unwrap().expect("row");
    assert_eq!(
        after, row,
        "a reporter from a connection the client holds read-only is still refused"
    );
    drop(returning);
    drop(held);
    host.abort();
}

/// A session whose plugin stops beating is declared dead when the window
/// expires, and a session with no task bound is not.
///
/// The death window only ever opened where a socket ended, so a plugin whose
/// event loop is blocked — holding its socket, sending nothing — was a session
/// no clock could see. It kept its slot, its projected row and its host resource
/// for as long as the client ran, and on a role at `max_sessions = 1` that
/// stopped every later delivery. The sweep now reads a stamp instead of a socket
/// and opens the same window on it.
///
/// The guard is the other half of the case: the plugin's heartbeat loop ends
/// with the last task it was given, so a session with no task bound has stopped
/// beating by design and is an agent waiting for work. Sweeping it would retire
/// the very connection the next delivery is staged onto.
#[tokio::test]
async fn a_session_whose_plugin_stops_beating_dies_when_the_window_expires() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    // A session bound to work that has not settled, whose plugin beats once and
    // then goes quiet while its socket stays up.
    let task_id = deliver(&state, &task_delivery("task A")).await;
    let (io, _assigns) = mount_plugin(&socket, Some(&task_id)).await;
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Report(plugin_beat(
            &task_id,
            1,
            1001,
            beat_body(1001, "running"),
        ))))
        .await
        .expect("the beat is answered");
    assert!(body.ok, "the beat is applied: {body:?}");

    // Inside the silence window the session is kept: this is a plugin that is
    // merely between beats, and retiring it would drop the resource its agent is
    // still holding.
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(20), 60)
            .len(),
        0,
        "a live socket inside the silence window is kept"
    );
    assert_eq!(state.session_count(), 1, "the session stays tracked");
    assert!(
        backend.closed_sessions.lock().is_empty(),
        "no resource closes inside the window"
    );

    // Past it the silence is the agent's death, and the same window answers for
    // it: the tuple reaches `Exited` through `AgentState::Gone`, and the work the
    // agent left owed ends `failed`.
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(31), 60)
            .len(),
        1,
        "a plugin that stopped beating on a live socket is declared dead"
    );
    let row = store.get_session(&task_id).unwrap().expect("row");
    assert_eq!(
        onlyne_session::stored_observation(&store, Some(&row)).agent,
        onlyne_session::AgentState::Gone,
        "the tuple's agent dimension is gone: {row:?}"
    );
    assert_eq!(
        published_projection(&store, &task_id).lifecycle,
        Lifecycle::Exited,
        "the session of an owed task reads exited"
    );
    let record = store
        .task(&task_id)
        .unwrap()
        .expect("the task keeps its record");
    assert_eq!(
        record.task_state,
        onlyne_session::TaskState::Failed,
        "the work the silent agent left owed ends failed: {record:?}"
    );
    assert_eq!(
        backend.reasons.lock().as_slice(),
        [onlyne_session::CloseReason::Fault],
        "the close carries the fault the owed work earns"
    );

    // A session with no task bound is never swept for silence: the plugin stops
    // beating between tasks by design, and its connection is the one the next
    // delivery is staged onto.
    let second = deliver(&state, &task_delivery("task B")).await;
    assert_ne!(second, task_id);
    let (io_second, _assigns) = mount_plugin(&socket, Some(&second)).await;
    complete_plugin(&io_second, &second, Outcome::Done).await;
    assert!(
        state.session_transport(&second).is_some(),
        "the settled session keeps the agent that answered it"
    );
    assert_eq!(
        state
            .retire_dropped_ghosts(Instant::now() + Duration::from_secs(3600), 60)
            .len(),
        0,
        "a session with no task bound is not swept for silence"
    );
    assert_eq!(
        state.session_count(),
        1,
        "the settled session stays tracked while its agent is attached"
    );
    assert_eq!(
        backend.closed_sessions.lock().len(),
        1,
        "only the silent session's resource closed"
    );
    assert_settled(&store, &second);
    drop(io);
    drop(io_second);
    host.abort();
}
