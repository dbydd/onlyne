use super::*;
use onlyne_proto::MountKind;

#[cfg(unix)]
use crate::session::dispatch::{DispatchState, Outbox, dispatch};
#[cfg(unix)]
use onlyne_adapter::{AdapterIo, IncomingFrame};
#[cfg(unix)]
use onlyne_layout::RoleWorkspace;
#[cfg(unix)]
use onlyne_proto::adapter::HandoffArgs;
#[cfg(unix)]
use onlyne_proto::{
    AdapterMsg, AgentMount, Body, Capability, Causality, ClientOp, Envelope, ErrorCode, Frame,
    HelloArgs, Mount, MsgKind, Outcome, PROTOCOL_VERSION, PluginOp, Principal, Report, ResBody,
    new_envelope, new_task_id,
};
#[cfg(unix)]
use onlyne_net::NetError;
#[cfg(unix)]
use onlyne_session::TaskState;
#[cfg(unix)]
use onlyne_session::backend::fake::FakeBackend;
#[cfg(unix)]
use onlyne_store::ClientStore;
#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::future::Future;
#[cfg(unix)]
use std::pin::Pin;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tempfile::tempdir;

#[cfg(unix)]
fn dispatch_state(workspace: &Path) -> DispatchState {
    let layout = RoleWorkspace::resolve(workspace);
    layout.bootstrap().unwrap();
    let store = ClientStore::open(layout.client_db_path()).unwrap();
    DispatchState::new(
        "planner",
        workspace,
        vec!["agent".into()],
        1,
        std::sync::Arc::new(FakeBackend::new()),
        store,
    )
}

/// A workspace whose canonical socket spelling is over the unix bound binds
/// the short path, names it in the marker, and answers for it through the one
/// accessor the clients use.
///
/// The hand-joined canonical leaf is the shape this case replaces: past the
/// bound it fails to bind, and the client keeps a server link while its local
/// surface stays shut. Windows keeps the canonical spelling as the bound
/// spelling, so the premise lives on unix.
#[cfg(unix)]
#[tokio::test]
async fn a_deep_workspace_serves_the_short_endpoint() {
    use onlyne_layout::UNIX_SOCKET_PATH_MAX;
    let segment = "deep-workspace-segment-aaaaaaaaaaaaaaaaaaaaaa";
    let dir = tempdir().unwrap();
    let workspace = dir.path().join(segment).join(segment).join("leaf");
    std::fs::create_dir_all(&workspace).unwrap();
    let layout = RoleWorkspace::resolve(&workspace);
    let adapter = AdapterSocket {
        workspace: workspace.clone(),
        role: "planner".into(),
        cluster: "c".into(),
        server: "s".into(),
        dispatch: dispatch_state(&workspace),
    };
    assert!(
        layout.socket_path_natural().as_os_str().len() > UNIX_SOCKET_PATH_MAX,
        "the premise: {} bytes at {}",
        layout.socket_path_natural().as_os_str().len(),
        layout.socket_path_natural().display(),
    );

    let (listener, endpoint) = adapter.bind().await.unwrap();
    assert!(
        endpoint.short(),
        "a canonical path over the bound moves the socket: {}",
        endpoint.actual().display(),
    );
    assert!(
        endpoint.actual().as_os_str().len() <= UNIX_SOCKET_PATH_MAX,
        "the served path fits the bound: {} bytes at {}",
        endpoint.actual().as_os_str().len(),
        endpoint.actual().display(),
    );
    assert_eq!(
        adapter.path(),
        endpoint.actual().to_path_buf(),
        "the accessor answers the path that was bound",
    );
    assert_eq!(
        std::fs::read_to_string(endpoint.marker()).unwrap().trim(),
        endpoint.actual().to_string_lossy().as_ref(),
        "the marker names the served path",
    );
    assert!(
        !endpoint.natural().exists(),
        "the canonical leaf stays empty: {}",
        endpoint.natural().display(),
    );
    drop(listener);
    let _ = std::fs::remove_file(endpoint.actual());
    let _ = std::fs::remove_dir(endpoint.actual().parent().unwrap());
}

#[test]
fn admin_probe_mounts_without_a_marker() {
    let agent = onlyne_proto::Mount::Agent(onlyne_proto::AgentMount {
        role: "planner".to_string(),
        ..Default::default()
    });
    assert!(mount_allowed(Some(&agent), MountKind::Agent, "planner"));
    assert!(!mount_allowed(Some(&agent), MountKind::Agent, "reviewer"));
    assert!(mount_allowed(None, MountKind::Admin, "planner"));
    assert!(!mount_allowed(None, MountKind::Agent, "planner"));
}
#[test]
fn terminated_register_requests_bye() {
    assert!(should_bye_on_register("terminated"));
    assert!(!should_bye_on_register("live-session"));
}

/// A served socket, the store the client queues its intents into, and one
/// staged task that sits at the second hop of a budgeted family.
#[cfg(unix)]
struct Staged {
    socket: PathBuf,
    store: ClientStore,
    state: DispatchState,
    task: String,
    causality: Causality,
}

#[cfg(unix)]
async fn staged(workspace: &Path) -> Staged {
    let layout = RoleWorkspace::resolve(workspace);
    layout.bootstrap().unwrap();
    let store = ClientStore::open(layout.client_db_path()).unwrap();
    let state = DispatchState::new(
        "planner",
        workspace,
        vec!["agent".into()],
        1,
        std::sync::Arc::new(FakeBackend::new()),
        store.clone(),
    );
    let adapter = AdapterSocket {
        workspace: workspace.to_path_buf(),
        role: "planner".into(),
        cluster: "c".into(),
        server: "s".into(),
        dispatch: state.clone(),
    };
    let socket = adapter.path();
    tokio::spawn(adapter.serve());
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(socket.exists(), "the host bound {}", socket.display());

    // One task inside a family: a root of its own, so the family id a handoff
    // hands down is tellable from the task that carries it.
    let task = new_task_id();
    let root = new_task_id();
    let causality = Causality {
        task: task.clone(),
        parent_task: Some(root.clone()),
        reply_to: None,
        hop: 2,
        attempt: 0,
        family: Some(root),
        hop_budget: Some(7),
        origin: Some("_supervisor".into()),
        deadline: None,
        labels: Some(BTreeMap::from([("run".to_string(), "brief".to_string())])),
    };
    let envelope = new_envelope(
        MsgKind::Task,
        Principal::role("_supervisor"),
        Principal::role("planner"),
        Body::text("write the brief"),
        Some(causality.clone()),
    )
    .expect("a task envelope the protocol accepts");
    dispatch(&state, &envelope).expect("the delivery takes a session");

    Staged {
        socket,
        store,
        state,
        task,
        causality,
    }
}

#[cfg(unix)]
#[derive(Clone)]
struct CaptureOutbox {
    frames: Arc<parking_lot::Mutex<Vec<ClientOp>>>,
}

#[cfg(unix)]
impl Outbox for CaptureOutbox {
    fn send(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), NetError>> + Send + '_>> {
        let frames = Arc::clone(&self.frames);
        Box::pin(async move {
            frames.lock().push(op);
            Ok(())
        })
    }

    fn request(
        &self,
        _op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<ResBody, NetError>> + Send + '_>> {
        Box::pin(async { Ok(ResBody::ok(serde_json::Value::Null)) })
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_local_completion_settles_the_task_and_publishes_only_its_projection() {
    let dir = tempdir().unwrap();
    let staged = staged(dir.path()).await;
    staged.state.attach_msg_id(&staged.task, "msg-local");
    let frames = Arc::new(parking_lot::Mutex::new(Vec::new()));
    staged
        .state
        .attach_outbox(Arc::new(CaptureOutbox {
            frames: Arc::clone(&frames),
        }));

    let (io, _inbound) = mounted(&staged.socket, &staged.task).await;
    let beat = io
        .request(AdapterMsg::Plugin(PluginOp::Report(Report::Heartbeat {
            task_id: staged.task.clone(),
            session_id: String::new(),
            generation: 1,
            seq: 1005,
            observed: serde_json::json!({
                "version": {"generation": 1, "seq": 1005},
                "generation_live": true,
                "isolate_after": 1,
                "terminate_after": 3,
                "mismatch_count": 0,
                "agent": "running",
                "delivery": "none",
                "resource": "attached",
                "recovery": "none",
            }),
            projection: None,
            cluster_ref: None,
        })))
        .await
        .expect("the turn heartbeat is answered");
    assert!(beat.ok, "the turn heartbeat is accepted: {beat:?}");
    frames.lock().clear();

    let mut stream = onlyne_layout::connect_local(&staged.socket)
        .await
        .expect("the local surface accepts the completion");
    let request = Frame::<ClientOp>::req(
        "local-complete",
        ClientOp::Report(Report::Complete {
            task_id: staged.task.clone(),
            outcome: Outcome::Done,
            head: Some("done".into()),
            reply_to: Some("completion-message".into()),
            cluster_ref: Some("origin-cluster".into()),
        }),
    );
    onlyne_frame::write_frame(&mut stream, &request)
        .await
        .expect("write the local completion");
    let response: Frame<ClientOp> = onlyne_frame::read_frame(&mut stream)
        .await
        .expect("read the local completion answer")
        .expect("the local surface answers");
    let response = match response {
        Frame::Res { body, .. } => body,
        other => panic!("the local completion is answered by a response: {other:?}"),
    };
    assert!(response.ok, "the local completion is accepted: {response:?}");

    let task = staged
        .store
        .task(&staged.task)
        .expect("read the local task")
        .expect("the local task was opened");
    assert_eq!(task.task_state, TaskState::Done);

    let queued = staged.store.flush_order().expect("read the local intents");
    let queued: Vec<ClientOp> = queued
        .iter()
        .map(|row| crate::runtime::intent::op_for_intent(row).expect("decode local intent"))
        .collect();
    assert!(queued.iter().any(|op| matches!(
        op,
        ClientOp::Ack(ack) if ack.msg_id == "msg-local" && ack.accepted
    )));

    let sent = frames.lock().clone();
    assert!(
        !sent.iter().any(|op| matches!(op, ClientOp::Report(Report::Complete { .. }))),
        "the local surface never forwards a raw completion report: {sent:?}"
    );
    assert!(sent.iter().any(|op| matches!(
        op,
        ClientOp::Report(Report::Heartbeat {
            task_id,
            projection: Some(_),
            ..
        }) if task_id == &staged.task
    )));
}

/// Mount one plugin for one session, the way the client spawns one: the mount
/// names the session it was spawned for, and the client admits this role.
///
/// The inbound receiver travels back with the connection because the reader
/// loop ends when it is dropped. This fixture keeps it alive and reads nothing.
#[cfg(unix)]
async fn mounted(
    socket: &Path,
    session: &str,
) -> (AdapterIo, tokio::sync::mpsc::Receiver<IncomingFrame>) {
    let stream = onlyne_layout::connect_local(socket)
        .await
        .expect("the role socket accepts a plugin");
    let (io, inbound) =
        AdapterIo::new_with_inbound(stream, Duration::from_secs(5), Duration::from_secs(5));
    let hello = HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: "onlyne-agent-test".into(),
        version: "1.0.0".into(),
        kind: MountKind::Agent,
        capabilities: vec![Capability::Report],
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
    (io, inbound)
}

#[cfg(unix)]
fn handoff(task_id: &str, to: &str, text: &str) -> AdapterMsg {
    AdapterMsg::Plugin(PluginOp::Handoff(HandoffArgs {
        task_id: task_id.to_string(),
        to: to.to_string(),
        text: text.to_string(),
        image: None,
    }))
}

/// An agent's `handoff` frame mints one child of the family its session serves
/// and queues it for the named role.
///
/// The child names the task it continues as its parent, sits one hop below it,
/// and carries the family's own figures: the family id, the hop budget, the
/// origin, and the labels. The answer names the child, and the queued frame is
/// the envelope the recipient role will read.
#[cfg(unix)]
#[tokio::test]
async fn a_handoff_frame_queues_a_child_of_the_task_its_session_serves() {
    let dir = tempdir().unwrap();
    let staged = staged(dir.path()).await;
    let (io, _inbound) = mounted(&staged.socket, &staged.task).await;

    let body = io
        .request(handoff(&staged.task, "reviewer", "carry it on"))
        .await
        .expect("the handoff is answered");
    assert!(body.ok, "the frame is accepted: {body:?}");
    let answer = body.data.expect("the answer names the child");
    let child_task = answer["task_id"]
        .as_str()
        .expect("the answer carries the child task id")
        .to_string();
    assert_ne!(child_task, staged.task, "the child is a task of its own");
    assert_eq!(
        answer["hop"].as_u64(),
        Some(u64::from(staged.causality.hop) + 1),
        "the child sits one hop below the task it continues",
    );
    assert_eq!(answer["queued"], serde_json::json!(true));
    let op_id = answer["op_id"]
        .as_str()
        .expect("the answer names the queued frame");

    let queued = staged.store.flush_order().expect("the intent queue");
    let row = queued
        .iter()
        .find(|row| row.op_id == op_id)
        .expect("the child is queued under the op_id the answer names");
    let envelope: Envelope =
        serde_json::from_value(row.env_json.clone()).expect("the queued frame is an envelope");
    assert_eq!(envelope.kind, MsgKind::Task);
    assert_eq!(envelope.to, Principal::role("reviewer"));
    let child = envelope.causality.expect("the child carries its chain");
    assert_eq!(child.task, child_task);
    assert_eq!(child.parent_task.as_deref(), Some(staged.task.as_str()));
    assert_eq!(child.hop, staged.causality.hop + 1);
    assert_eq!(
        child.family, staged.causality.family,
        "the family id rides along untouched",
    );
    assert_ne!(
        child.family.as_deref(),
        Some(staged.task.as_str()),
        "the child inherits the family id of the run",
    );
    assert_eq!(child.hop_budget, staged.causality.hop_budget);
    assert_eq!(child.origin, staged.causality.origin);
    assert_eq!(child.labels, staged.causality.labels);
}

/// A `handoff` frame that names no task this client serves is refused with an
/// error body, and the connection stays live.
///
/// An unknown task and an empty `task_id` are the two spellings of the same
/// answer: the field that caused the refusal travels back named, and the session
/// the connection does serve is still handed on afterwards.
#[cfg(unix)]
#[tokio::test]
async fn a_handoff_naming_no_served_task_is_refused_with_an_error() {
    let dir = tempdir().unwrap();
    let staged = staged(dir.path()).await;
    let (io, _inbound) = mounted(&staged.socket, &staged.task).await;

    let unknown = new_task_id();
    let body = io
        .request(handoff(&unknown, "reviewer", "carry it on"))
        .await
        .expect("the refusal is answered");
    assert!(!body.ok, "a task no session serves is refused: {body:?}");
    let error = body.error.expect("the refusal carries an error");
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.field.as_deref(), Some("task_id"));
    assert!(
        error.message.contains(&unknown),
        "the refusal names the task it did not find: {error:?}",
    );

    let body = io
        .request(handoff("", "reviewer", "carry it on"))
        .await
        .expect("the refusal is answered");
    assert!(!body.ok, "an empty task id names no task: {body:?}");
    assert_eq!(
        body.error.map(|error| error.code),
        Some(ErrorCode::Invalid),
        "an empty task id earns the same refusal",
    );

    let body = io
        .request(handoff(&staged.task, "reviewer", "carry it on"))
        .await
        .expect("the connection still answers");
    assert!(
        body.ok,
        "the session's own task is still handed on: {body:?}",
    );
}
