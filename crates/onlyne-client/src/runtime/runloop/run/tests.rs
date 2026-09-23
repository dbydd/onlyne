use super::{run, settle_control};
use crate::runtime::runloop::config::{ClientInit, RunState};
use crate::runtime::runloop::test_support::test_state;
use crate::session::dispatch::{self, ReadyNotice};
use onlyne_layout::RoleWorkspace;
use onlyne_proto::{
    Body, Capability, ClientOp, ControlOp, Delivery, Lifecycle, MsgKind, Outcome, Principal,
    Report, SessionProjection, new_envelope, new_task_id,
};
use onlyne_session::{
    Capabilities, CloseReason, ResourceProbe, SessionBackend, SessionRef, SpawnSpec, TaskState,
};
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

#[derive(Clone, Default)]
struct CloseFailingBackend {
    inner: onlyne_session::backend::fake::FakeBackend,
}

impl SessionBackend for CloseFailingBackend {
    fn name(&self) -> &'static str {
        "close-failing"
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn available(&self) -> anyhow::Result<bool> {
        self.inner.available()
    }

    fn spawn(&self, spec: SpawnSpec) -> anyhow::Result<SessionRef> {
        self.inner.spawn(spec)
    }

    fn attach(&self, session: &SessionRef) -> anyhow::Result<SessionRef> {
        self.inner.attach(session)
    }

    fn probe(&self, session: &SessionRef) -> anyhow::Result<ResourceProbe> {
        self.inner.probe(session)
    }

    fn close(
        &self,
        _session: &SessionRef,
        _reason: CloseReason,
        _force: bool,
    ) -> anyhow::Result<()> {
        anyhow::bail!("close refused")
    }
}

fn state_with_backend(backend: Arc<dyn SessionBackend>) -> (RunState, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    let store =
        onlyne_store::ClientStore::open(dir.path().join("client.db")).expect("client store");
    let dispatch =
        dispatch::DispatchState::new("planner", dir.path(), Vec::new(), 2, backend, store.clone());
    let intents = crate::runtime::intent::IntentMachine::new(
        store.clone(),
        crate::runtime::runloop::DEFAULT_INTENT_ATTEMPTS,
        crate::runtime::runloop::default_intent_backoff(),
    );
    let state = RunState {
        accept_new: dispatch.accept_new(),
        store,
        intents: Arc::new(parking_lot::Mutex::new(intents)),
        dispatch,
        welcome: Arc::new(tokio::sync::Mutex::new(None)),
        stall_report_secs: 0,
        reconnect_grace_secs: 0,
    };
    (state, dir)
}

async fn staged_control(state: &RunState, task_id: &str, control: ControlOp) -> (Delivery, usize) {
    let task = new_envelope(
        MsgKind::Task,
        Principal::role("sender"),
        Principal::role("planner"),
        Body::text("work"),
        Some(onlyne_proto::Causality::root(task_id.to_string())),
    )
    .expect("task envelope");
    let session = dispatch::dispatch(&state.dispatch, &task).expect("dispatch task");
    state.dispatch.attach_msg_id(task_id, "msg-task");
    let _ = dispatch::on_ready(
        &state.dispatch,
        ReadyNotice {
            task_id: task_id.to_string(),
            session_id: session.task_id.clone(),
            generation: session.generation,
            io: None,
            capabilities: Vec::<Capability>::new(),
        },
        "",
    )
    .await;
    state
        .store
        .settle_task(task_id, TaskState::Done)
        .expect("task outcome");
    let mut envelope = new_envelope(
        MsgKind::Task,
        Principal::role("operator"),
        Principal::role("planner"),
        Body::text(""),
        Some(onlyne_proto::Causality::root(task_id.to_string())),
    )
    .expect("control envelope base");
    envelope.kind = MsgKind::Control;
    envelope.control = Some(control);
    envelope.validate().expect("control envelope");
    let before = state.store.flush_order().expect("intent queue").len();
    (
        Delivery {
            msg_id: "msg-control".into(),
            envelope: Box::new(envelope),
        },
        before,
    )
}

fn queued_ops(state: &RunState, from: usize) -> Vec<ClientOp> {
    state
        .store
        .flush_order()
        .expect("intent queue")
        .into_iter()
        .skip(from)
        .map(|row| crate::runtime::intent::op_for_intent(&row).expect("queued op"))
        .collect()
}

fn published_projection(op: &ClientOp, task_id: &str) -> Option<SessionProjection> {
    match op {
        ClientOp::Report(Report::Heartbeat {
            task_id: published_task,
            projection,
            ..
        }) if published_task == task_id => projection.clone(),
        _ => None,
    }
}

/// A workspace socket that cannot be bound ends the run with an error.
///
/// The silent 0.5s restart this replaces kept a process alive that held a
/// server link while the local surface stayed shut, so `onlyne` verbs from the
/// workspace failed and the server still counted the role connected. The
/// message is the bind context naming the canonical spelling, and the cause
/// carries the served path with each length.
#[tokio::test]
async fn an_unbindable_socket_ends_the_run_with_an_error() {
    let dir = tempdir().unwrap();
    let workspace = RoleWorkspace::resolve(dir.path());
    workspace.bootstrap().unwrap();
    #[cfg(unix)]
    std::fs::write(
        workspace.run_dir().join("socket"),
        "/nonexistent-dir-onlyne-for-this-test/sock\n",
    )
    .unwrap();
    // Windows resolves the natural path regardless of the Unix endpoint
    // marker. Hold the production NPFS listener instead: a second bind to
    // that live name is the platform's EADDRINUSE equivalent.
    #[cfg(windows)]
    let (_held_listener, _endpoint) =
        onlyne_layout::bind_socket(workspace.root(), &workspace.run_dir()).unwrap();
    let init = ClientInit::new(
        dir.path(),
        "planner",
        "127.0.0.1:1",
        workspace.key_path(),
        "sha256/0000000000000000000000000000000000000000000000000000000000000000",
    )
    .with_backend("fake");
    let outcome = tokio::time::timeout(Duration::from_secs(10), run(init))
        .await
        .expect("the bind failure ends the run well inside the timeout");
    let error = outcome.expect_err("an unbindable socket is an error");
    assert!(
        error.to_string().contains("bind the workspace socket"),
        "{error}"
    );
}

/// A closing control answers its own delivery before publishing the exit.
#[tokio::test]
async fn a_cancel_close_queues_the_control_ack_before_the_exited_publish() {
    let (state, _dir) = test_state(2, Vec::new());
    let task = new_task_id();
    let (delivery, before) = staged_control(
        &state,
        &task,
        ControlOp::Cancel {
            task_id: task.clone(),
            reason: "operator cancel".into(),
        },
    )
    .await;

    settle_control(&state, &delivery).await;

    let ops = queued_ops(&state, before);
    let ack = ops
        .iter()
        .position(|op| matches!(op, ClientOp::Ack(ack) if ack.msg_id == "msg-control"))
        .expect("control ack");
    let publish = ops
        .iter()
        .position(|op| published_projection(op, &task).is_some())
        .expect("exit publish");
    assert!(ack < publish, "ack and publish queue order: {ops:?}");
}

/// The post-ack publish still carries the closed session's existing outcome.
#[tokio::test]
async fn a_cancel_close_publishes_the_exited_projection_with_the_existing_outcome() {
    let (state, _dir) = test_state(2, Vec::new());
    let task = new_task_id();
    let (delivery, before) = staged_control(
        &state,
        &task,
        ControlOp::Cancel {
            task_id: task.clone(),
            reason: "operator cancel".into(),
        },
    )
    .await;

    settle_control(&state, &delivery).await;

    let published = queued_ops(&state, before)
        .iter()
        .find_map(|op| published_projection(op, &task))
        .expect("exit publish");
    assert_eq!(published.lifecycle, Lifecycle::Exited);
    assert_eq!(published.outcome, Some(Outcome::Done));
}

/// A refused closing command settles its row and leaves the mirror alone.
#[tokio::test]
async fn a_refused_control_command_queues_no_publish() {
    let (state, _dir) = state_with_backend(Arc::new(CloseFailingBackend::default()));
    let task = new_task_id();
    let (delivery, before) = staged_control(
        &state,
        &task,
        ControlOp::Cancel {
            task_id: task.clone(),
            reason: "operator cancel".into(),
        },
    )
    .await;

    settle_control(&state, &delivery).await;

    let ops = queued_ops(&state, before);
    assert!(ops.iter().any(|op| matches!(
        op,
        ClientOp::Ack(ack) if ack.msg_id == "msg-control" && !ack.accepted
    )));
    assert!(
        !ops.iter()
            .any(|op| published_projection(op, &task).is_some()),
        "a refused command queues no publish: {ops:?}"
    );
}
