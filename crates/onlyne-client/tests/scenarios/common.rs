//! Shared fixtures and helpers for the client scenarios: the envelope and delivery
//! builders, the plugin-mount helpers, and the recording backends.

use onlyne_adapter::{AdapterIo, WireMessage};
use onlyne_client::session::{
    accept::AcceptPath,
    adapter_socket::AdapterSocket,
    dispatch::{DispatchState, ReadyNotice, dispatch, on_ready, projection_of},
};
use onlyne_frame::{read_frame, write_frame};
use onlyne_proto::{
    AdapterMsg, AgentMount, Capability, ClientOp, Delivery, Envelope, HelloArgs, HostOp, Lifecycle,
    Mount, MountKind, MsgKind, Outcome, PROTOCOL_VERSION, PluginOp, Report, new_envelope,
    new_task_id,
};
use onlyne_session::SessionLedger;
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(super) fn sample_envelope(role: &str, text: &str) -> Envelope {
    new_envelope(
        MsgKind::Task,
        onlyne_proto::Principal::role("planner"),
        onlyne_proto::Principal::role(role),
        onlyne_proto::Body::text(text),
        Some(onlyne_proto::Causality::root(new_task_id())),
    )
    .unwrap()
}

/// Records every lifecycle frame in the order the dispatcher sent it.
#[derive(Clone, Default)]
pub(super) struct RecordingOutbox {
    frames: Arc<tokio::sync::Mutex<Vec<ClientOp>>>,
}

impl RecordingOutbox {
    pub(super) async fn frames(&self) -> Vec<ClientOp> {
        self.frames.lock().await.clone()
    }

    pub(super) async fn kinds(&self) -> Vec<&'static str> {
        self.frames().await.iter().map(|op| op.name()).collect()
    }

    pub(super) async fn clear(&self) {
        self.frames.lock().await.clear();
    }

    /// The projection publishes, in the order they left the dispatcher: each one
    /// is a heartbeat report carrying the client's whole projection.
    pub(super) async fn projection_publishes(&self) -> Vec<Published> {
        projection_publishes_of(&self.frames().await)
    }
}

/// One projection publish pulled off the wire.
#[derive(Debug, Clone)]
pub(super) struct Published {
    pub(super) task_id: String,
    pub(super) session_id: String,
    pub(super) generation: u64,
    pub(super) seq: u64,
    pub(super) observed: serde_json::Value,
    pub(super) projection: onlyne_proto::SessionProjection,
    pub(super) cluster_ref: Option<String>,
}

/// The projection publishes in one recorded frame list.
pub(super) fn projection_publishes_of(frames: &[ClientOp]) -> Vec<Published> {
    frames
        .iter()
        .filter_map(|op| match op {
            ClientOp::Report(Report::Heartbeat {
                task_id,
                session_id,
                generation,
                seq,
                observed,
                projection: Some(projection),
                cluster_ref,
            }) => Some(Published {
                task_id: task_id.clone(),
                session_id: session_id.clone(),
                generation: *generation,
                seq: *seq,
                observed: observed.clone(),
                projection: projection.clone(),
                cluster_ref: cluster_ref.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// One plugin heartbeat on the adapter socket: liveness plus the reducer's own
/// tuple. It names no session and carries no projection — publishing state is
/// the host's job, and the host does it in `projection_publishes`.
pub(super) fn plugin_beat(
    task_id: &str,
    generation: u64,
    seq: u64,
    observed: serde_json::Value,
) -> Report {
    Report::Heartbeat {
        task_id: task_id.to_string(),
        session_id: String::new(),
        generation,
        seq,
        observed,
        projection: None,
        cluster_ref: None,
    }
}

impl onlyne_client::session::dispatch::Outbox for RecordingOutbox {
    fn send(
        &self,
        op: ClientOp,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), onlyne_net::NetError>> + Send + '_>,
    > {
        Box::pin(async move {
            self.frames.lock().await.push(op);
            Ok(())
        })
    }

    /// The recorder answers every request with an accepted body, so a caller
    /// that reads the server's verdict records the frame and moves on.
    fn request(
        &self,
        op: ClientOp,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<onlyne_proto::ResBody, onlyne_net::NetError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            self.frames.lock().await.push(op);
            Ok(onlyne_proto::ResBody::ok(serde_json::Value::Null))
        })
    }
}

/// Spawn one task and settle its adapter transport; the caller reads the order.
pub(super) async fn spawn_ready(
    state: &DispatchState,
    text: &str,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let env = sample_envelope("planner", text);
    let task_id = env.task_id().unwrap().to_string();
    let session = dispatch(state, &env).unwrap();
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (io_client, mut client_inbound) =
        AdapterIo::new_with_inbound(client_io, Duration::from_secs(2), Duration::from_secs(2));
    let (io_server, _server_inbound) =
        AdapterIo::new_with_inbound(server_io, Duration::from_secs(2), Duration::from_secs(2));
    let (record_tx, record_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(frame) = client_inbound.recv().await {
            if let AdapterMsg::Host(HostOp::Assign(assign)) = frame.msg {
                let _ = record_tx.send(format!("assign:{}", assign.task_id));
            }
        }
        drop(io_client);
    });
    on_ready(
        state,
        ReadyNotice {
            task_id: task_id.clone(),
            session_id: session.task_id.clone(),
            generation: 1,
            io: Some(io_server),
            capabilities: vec![Capability::Inject],
        },
        "prose",
    )
    .await
    .unwrap();
    (task_id, record_rx)
}

/// Reports the reason the dispatcher handed the backend.
#[derive(Clone, Default)]
pub(super) struct ReasonBackend {
    pub(super) inner: FakeBackend,
    pub(super) reasons: Arc<parking_lot::Mutex<Vec<onlyne_session::CloseReason>>>,
    pub(super) closed_sessions: Arc<parking_lot::Mutex<Vec<onlyne_session::SessionRef>>>,
    pub(super) fail_close: Arc<std::sync::atomic::AtomicBool>,
}

impl onlyne_session::SessionBackend for ReasonBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn capabilities(&self) -> onlyne_session::Capabilities {
        self.inner.capabilities()
    }
    fn available(&self) -> anyhow::Result<bool> {
        self.inner.available()
    }
    fn spawn(&self, spec: onlyne_session::SpawnSpec) -> anyhow::Result<onlyne_session::SessionRef> {
        self.inner.spawn(spec)
    }
    fn attach(
        &self,
        session: &onlyne_session::SessionRef,
    ) -> anyhow::Result<onlyne_session::SessionRef> {
        let mut refreshed = self.inner.attach(session)?;
        refreshed.backend_ref["refreshed"] = serde_json::Value::Bool(true);
        Ok(refreshed)
    }
    fn probe(
        &self,
        session: &onlyne_session::SessionRef,
    ) -> anyhow::Result<onlyne_session::ResourceProbe> {
        self.inner.probe(session)
    }
    fn close(
        &self,
        session: &onlyne_session::SessionRef,
        reason: onlyne_session::CloseReason,
        force: bool,
    ) -> anyhow::Result<()> {
        self.reasons.lock().push(reason);
        self.closed_sessions.lock().push(session.clone());
        if self.fail_close.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow::anyhow!("recorded close failure"));
        }
        self.inner.close(session, reason, force)
    }
}

/// One delivery of a fresh task to this role, as the pull loop receives it.
pub(super) fn task_delivery(text: &str) -> Delivery {
    Delivery {
        msg_id: format!("msg-{}", new_task_id()),
        envelope: Box::new(sample_envelope("planner", text)),
    }
}

/// Hand one delivery to this role the way the pull loop does: stage the
/// session, then route its payload to whichever connection serves it.
pub(super) async fn deliver(state: &DispatchState, delivery: &Delivery) -> String {
    let path = AcceptPath::new(state.clone(), String::new());
    let session = path
        .accept_new(delivery, true)
        .unwrap()
        .expect("a fresh task is accepted");
    let task_id = delivery.envelope.task_id().unwrap().to_string();
    state.attach_msg_id(&task_id, &delivery.msg_id);
    state.hand_staged(&session.task_id).await.unwrap();
    task_id
}

/// Mount one plugin on the role socket and record every `assign` it receives.
///
/// `session` is `ONLYNE_SESSION_ID` as the plugin reads it: a plugin the client
/// spawned names the session it was spawned for, and `None` is the
/// always-running agent that attached before any work existed. The returned
/// [`AdapterIo`] is the plugin's half of the connection.
pub(super) async fn mount_plugin(
    socket: &Path,
    session: Option<&str>,
) -> (AdapterIo, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let stream = onlyne_layout::connect_local(socket)
        .await
        .expect("the role socket accepts a plugin");
    let (io, mut inbound) =
        AdapterIo::new_with_inbound(stream, Duration::from_secs(5), Duration::from_secs(5));
    let (assigns_tx, assigns_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(frame) = inbound.recv().await {
            if let AdapterMsg::Host(HostOp::Assign(assign)) = frame.msg {
                let _ = assigns_tx.send(assign.task_id);
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
            session: session.map(str::to_string),
            task_id: session.map(str::to_string),
            pid: None,
        })),
    };
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Hello(hello)))
        .await
        .expect("the mount answers");
    assert!(body.ok, "the role socket admits this plugin: {body:?}");
    (io, assigns_rx)
}

pub(super) async fn complete_plugin(io: &AdapterIo, task_id: &str, outcome: Outcome) {
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Report(Report::Complete {
            task_id: task_id.to_string(),
            outcome,
            head: Some("done".into()),
            reply_to: None,
            cluster_ref: None,
        })))
        .await
        .expect("the completion report is answered");
    assert!(body.ok, "the completion report is accepted: {body:?}");
}

pub(super) async fn mount_raw_plugin(socket: &Path, session: &str) -> onlyne_layout::LocalStream {
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
    loop {
        let frame = read_frame::<_, WireMessage>(&mut stream)
            .await
            .unwrap()
            .expect("the raw plugin connection stays open for assign");
        if let AdapterMsg::Host(HostOp::Assign(assign)) = frame.msg {
            assert_eq!(assign.task_id, session);
            return stream;
        }
    }
}

pub(super) async fn complete_raw_plugin(
    stream: &mut onlyne_layout::LocalStream,
    task_id: &str,
    outcome: Outcome,
) {
    write_frame(
        &mut *stream,
        &WireMessage {
            id: Some(2),
            reply_to: None,
            msg: AdapterMsg::Plugin(PluginOp::Report(Report::Complete {
                task_id: task_id.to_string(),
                outcome,
                head: Some("done".into()),
                reply_to: None,
                cluster_ref: None,
            })),
        },
    )
    .await
    .unwrap();
    loop {
        let frame = read_frame::<_, WireMessage>(&mut *stream)
            .await
            .unwrap()
            .expect("the raw completion receives a response");
        if frame.reply_to == Some(2) {
            assert!(
                matches!(&frame.msg, AdapterMsg::Res(body) if body.ok),
                "the raw completion is accepted: {:?}",
                frame.msg
            );
            return;
        }
    }
}

/// Poll a state predicate so a socket-level hand-off is never raced.
pub(super) async fn eventually(mut predicate: impl FnMut() -> bool, what: &str) {
    for _ in 0..400 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Bind the role socket and answer every connection on it until this returns.
pub(super) async fn serve_role_socket(
    state: &DispatchState,
    dir: &Path,
) -> (PathBuf, tokio::task::JoinHandle<anyhow::Result<()>>) {
    let adapter = AdapterSocket {
        workspace: dir.to_path_buf(),
        role: "planner".into(),
        cluster: "c".into(),
        server: "s".into(),
        dispatch: state.clone(),
    };
    let socket = adapter.path();
    let host = tokio::spawn(adapter.serve());
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(socket.exists(), "the host bound {}", socket.display());
    (socket, host)
}

/// What the client's two durable rows say about one session: the stored tuple
/// beside the verdict in the task's own record, put together the way the client
/// puts a publish together. A test that asks "is this session exited" asks
/// through here, because no single row is the answer.
pub(super) fn published_projection(
    store: &ClientStore,
    task_id: &str,
) -> onlyne_proto::SessionProjection {
    let row = store
        .get_session(task_id)
        .unwrap()
        .expect("the session keeps its row");
    let task_state = store
        .task(task_id)
        .unwrap()
        .map(|record| record.task_state)
        .unwrap_or(onlyne_session::TaskState::Pending);
    projection_of(&row, task_state)
}

/// A settled task is `exited`/`done` in the projection the client publishes.
///
/// Two rows answer that: the session tuple and the task's own record. The check
/// reads both, because the point of the split is that neither row alone is the
/// account — and the session row holds no verdict to be misread as one.
pub(super) fn assert_settled(store: &ClientStore, task_id: &str) {
    let row = store
        .get_session(task_id)
        .unwrap()
        .expect("the settled session keeps its row");
    let record = store
        .task(task_id)
        .unwrap()
        .expect("the settled task keeps its own record");
    assert_eq!(
        record.task_state,
        onlyne_session::TaskState::Done,
        "the verdict lives in the task table"
    );
    assert!(
        record.settled_at.is_some(),
        "and the settle stamped its clock"
    );
    let projection = projection_of(&row, record.task_state);
    assert_eq!(
        projection.lifecycle,
        Lifecycle::Exited,
        "the settled session publishes exited"
    );
    assert_eq!(
        projection.outcome,
        Some(Outcome::Done),
        "the settled session publishes its outcome"
    );
    let observed: serde_json::Value =
        serde_json::from_str(&row.observed_json).expect("the stored tuple is JSON");
    for claim in ["outcome", "lifecycle", "public", "public_lifecycle"] {
        assert!(
            observed.get(claim).is_none(),
            "the session row cannot claim {claim}: {observed}"
        );
    }
    // The whole tuple survives the settle: the projection a supervisor reads
    // must carry the resource dimension beside the lifecycle, so a row can
    // never read `exited` with a missing resource leg.
    assert!(
        observed.get("resource").is_some(),
        "the stored tuple carries the resource dimension: {observed}"
    );
}
