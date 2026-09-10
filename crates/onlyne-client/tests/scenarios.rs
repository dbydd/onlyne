use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use onlyne_adapter::AdapterIo;
use onlyne_client::{
    adapter_socket::AdapterSocket,
    dispatch::{ClientLink, DispatchState, ReadyNotice, dispatch, on_plugin_report, on_ready, on_recycled, missing_capability, plugin_gap},
    init::{InitArgs, init, legacy_error_code},
    intent::{IntentMachine, IntentResult, op_for_intent, permanent_error},
    local_cli::LocalCli,
    runloop::ClientInit,
};
use onlyne_frame::{read_frame, write_frame};
use onlyne_net::{KeyPair, TcpListen, TlsConn, accept as accept_handshake, gen_self_signed, server_config, table_from};
use onlyne_proto::{
    AdapterMsg, Capability, ClientOp, Envelope, ErrorCode, Frame, HostOp, MsgKind, PROTOCOL_VERSION,
    QueryRolesArgs, Receipt, ResBody, Welcome,
    new_envelope, new_task_id,
};
use onlyne_session::SessionLedger;
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use tempfile::tempdir;

fn sample_envelope(role: &str, text: &str) -> Envelope {
    new_envelope(
        MsgKind::Task,
        onlyne_proto::Principal::role("planner"),
        onlyne_proto::Principal::role(role),
        onlyne_proto::Body::text(text),
        Some(onlyne_proto::Causality::root(new_task_id())),
    )
    .unwrap()
}

#[test]
fn legacy_refusal_exits_2_and_creates_no_files() {
    let dir = tempdir().unwrap();
    let channels = dir.path().join(".onlyne/channels");
    std::fs::create_dir_all(&channels).unwrap();

    let server_dir = tempdir().unwrap();
    let server_spec = server_dir.path().join(".onlyne/spec.toml");
    std::fs::create_dir_all(server_spec.parent().unwrap()).unwrap();
    std::fs::write(&server_spec, "[server]\nname = \"srv\"\nlisten = \"127.0.0.1:7899\"\ncert_pin = \"sha256/0000000000000000000000000000000000000000000000000000000000000000\"\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let res = rt.block_on(init(InitArgs {
        workspace: dir.path().to_path_buf(),
        role: "planner".into(),
        server_root: server_dir.path().to_path_buf(),
        prose: String::new(),
    }));

    assert!(res.is_err());
    assert_eq!(legacy_error_code(), 2);
    // Assert no files created in workspace beyond channels
    let entries: Vec<_> = std::fs::read_dir(dir.path().join(".onlyne")).unwrap().collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].as_ref().unwrap().file_name(), "channels");
}

#[test]
fn permissions_mode_600_for_role_key_and_socket() {
    let ws_dir = tempdir().unwrap();
    let server_dir = tempdir().unwrap();
    let server_spec = server_dir.path().join(".onlyne/spec.toml");
    std::fs::create_dir_all(server_spec.parent().unwrap()).unwrap();
    std::fs::write(&server_spec, "[server]\nname = \"srv\"\nlisten = \"127.0.0.1:7899\"\ncert_pin = \"sha256/0000000000000000000000000000000000000000000000000000000000000000\"\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let fragment = rt.block_on(init(InitArgs {
        workspace: ws_dir.path().to_path_buf(),
        role: "planner".into(),
        server_root: server_dir.path().to_path_buf(),
        prose: "v1 smoke prose".into(),
    })).unwrap();
    assert!(fragment.starts_with("[[client]]\n"));
    let lines: Vec<&str> = fragment.lines().collect();
    assert_eq!(lines.len(), 9, "fragment shape is fixed: {fragment:?}");
    assert_eq!(lines[0], "[[client]]");
    assert_eq!(lines[1], "role = \"planner\"");
    assert!(lines[2].starts_with("key = \"ed25519/"), "key line is {}", lines[2]);
    assert_eq!(&lines[3..], &[
        "admin = false",
        "max_sessions = 1",
        "allowed_senders = [\"*\", \"planner\"]",
        "allowed_targets = [\"planner\"]",
        "prose = \"v1 smoke prose\"",
        "reuse = true",
    ]);
    assert!(fragment.ends_with("reuse = true\n"));
    assert!(fragment.contains("key = \"ed25519/"));

    let key_path = ws_dir.path().join(".onlyne/keys/role.key");
    assert!(key_path.exists());
    let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    // Stale socket cleanup and socket mode 0600
    let db_path = ws_dir.path().join(".onlyne/client.db");
    let store = ClientStore::open(&db_path).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let dispatch = DispatchState::new("planner", ws_dir.path(), vec!["agent".into()], 2, true, backend, store);
    let adapter = AdapterSocket {
        workspace: ws_dir.path().to_path_buf(),
        role: "planner".into(),
        cluster: "c".into(),
        server: "s".into(),
        dispatch,
    };

    // Create a stale socket file
    let sock_path = adapter.path();
    std::fs::create_dir_all(sock_path.parent().unwrap()).unwrap();
    std::fs::write(&sock_path, "stale").unwrap();
    assert!(sock_path.exists());

    let listener = rt.block_on(adapter.bind()).unwrap();
    let sock_mode = std::fs::metadata(&sock_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(sock_mode, 0o600);
    drop(listener);
}

#[test]
fn intent_acl_denial_drops_row_without_retry() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let machine = IntentMachine::new(store.clone(), 3, vec![1000, 2000]);

    let envelope = sample_envelope("planner", "hello");
    machine.enqueue(&envelope).unwrap();
    assert_eq!(store.pending_intent_count().unwrap(), 1);

    let rows = store.flush_order().unwrap();
    assert_eq!(rows.len(), 1);

    let res = machine.attempt(&rows[0], Some(&ResBody::err(ErrorCode::AclDenied, "acl denied", None))).unwrap();
    assert!(matches!(res, IntentResult::Dropped(ErrorCode::AclDenied, _)));
    assert_eq!(store.pending_intent_count().unwrap(), 0);
}

#[test]
fn intent_idempotent_duplicate_settles_accepted() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let machine = IntentMachine::new(store.clone(), 3, vec![1000, 2000]);

    let envelope = sample_envelope("planner", "hello");
    machine.enqueue(&envelope).unwrap();
    let rows = store.flush_order().unwrap();

    let receipt = Receipt {
        msg_id: "msg-1".into(),
        op_id: envelope.op_id.clone(),
        kind: MsgKind::Task,
        task: Some("task-1".into()),
        state: onlyne_proto::LedgerState::Queued,
        enqueued_at: chrono::Utc::now(),
        duplicate: true,
    };
    let body = ResBody::err_with_data(
        ErrorCode::Duplicate,
        "duplicate",
        None,
        serde_json::to_value(&receipt).unwrap(),
    );

    let res = machine.attempt(&rows[0], Some(&body)).unwrap();
    assert!(matches!(res, IntentResult::Accepted(Some(r)) if r.duplicate));
    assert_eq!(store.pending_intent_count().unwrap(), 0);
}

#[test]
fn intent_exhaustion_drives_fault_after_ceiling() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let machine = IntentMachine::new(store.clone(), 3, vec![100, 200]);

    let envelope = sample_envelope("planner", "hello");
    machine.enqueue(&envelope).unwrap();
    let rows = store.flush_order().unwrap();
    let row = &rows[0];

    // Attempt 1: internal -> Retryable
    let res1 = machine.attempt(row, Some(&ResBody::err(ErrorCode::Internal, "internal error", None))).unwrap();
    assert!(matches!(res1, IntentResult::Retryable(ErrorCode::Internal, _)));

    // Update row attempt to 2
    let rows = store.flush_order().unwrap();
    let res2 = machine.attempt(&rows[0], Some(&ResBody::err(ErrorCode::Internal, "internal error", None))).unwrap();
    assert!(matches!(res2, IntentResult::Retryable(ErrorCode::Internal, _)));

    // Attempt 3 reaches attempts ceiling (3) -> Exhausted
    let rows = store.flush_order().unwrap();
    let res3 = machine.attempt(&rows[0], Some(&ResBody::err(ErrorCode::Internal, "internal error", None))).unwrap();
    assert!(matches!(res3, IntentResult::Exhausted));
    assert_eq!(store.pending_intent_count().unwrap(), 0);

    let task_id = envelope.task_id().unwrap();
    let faults = onlyne_session::SessionLedger::list_faults(&store, task_id).unwrap();
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0].kind, "intent_exhausted");
}

#[test]
fn session_reuse_and_capacity_capping() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new("planner", dir.path(), vec!["echo".into()], 2, true, backend, store.clone());

    let mut env1 = sample_envelope("planner", "task 1");
    let family1 = new_task_id();
    env1.causality = Some(onlyne_proto::Causality {
        task: new_task_id(),
        parent_task: Some(family1.clone()),
        reply_to: None,
        hop: 0,
        attempt: 0,
    });

    let s1 = dispatch(&state, &env1).unwrap();
    assert_eq!(state.session_count(), 1);

    // Recycle task 1 so session becomes idle
    on_recycled(&state, env1.task_id().unwrap(), "done").unwrap();

    // Task 2 with same family reuses session 1
    let mut env2 = sample_envelope("planner", "task 2");
    env2.causality = Some(onlyne_proto::Causality {
        task: new_task_id(),
        parent_task: Some(family1),
        reply_to: None,
        hop: 0,
        attempt: 0,
    });
    let s2 = dispatch(&state, &env2).unwrap();
    assert_eq!(s1.backend_ref, s2.backend_ref, "the same backend resource carries the next task");
    assert_eq!(state.session_count(), 1);
    let reused_row = store.get_session(env2.task_id().unwrap()).unwrap().expect("the reused task has a ledger row");
    assert_ne!(reused_row.backend_ref.trim(), "{}", "a reused session must resolve to its backend resource");

    // Task 3 with different family spawns session 2
    let mut env3 = sample_envelope("planner", "task 3");
    let family2 = new_task_id();
    env3.causality = Some(onlyne_proto::Causality {
        task: new_task_id(),
        parent_task: Some(family2),
        reply_to: None,
        hop: 0,
        attempt: 0,
    });
    let _s3 = dispatch(&state, &env3).unwrap();
    assert_eq!(state.session_count(), 2);

    // Task 4 with different family exceeds max_sessions (2)
    let mut env4 = sample_envelope("planner", "task 4");
    env4.causality = Some(onlyne_proto::Causality {
        task: new_task_id(),
        parent_task: Some(new_task_id()),
        reply_to: None,
        hop: 0,
        attempt: 0,
    });
    let err = dispatch(&state, &env4);
    assert!(err.is_err());
    assert_eq!(err.unwrap_err().to_string(), "max_sessions reached");
}

#[test]
fn redelivered_task_keeps_its_one_session() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new("planner", dir.path(), vec!["echo".into()], 2, true, backend.clone(), store);

    let envelope = sample_envelope("planner", "task 1");
    let first = dispatch(&state, &envelope).unwrap();
    let again = dispatch(&state, &envelope).unwrap();

    assert_eq!(first.task_id, again.task_id);
    assert_eq!(backend.sessions().len(), 1, "a redelivery must not spawn a second resource");
    assert_eq!(state.session_count(), 1);
}

#[tokio::test]
async fn ready_barrier_orders_assign_after_ready() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new("planner", dir.path(), vec!["echo".into()], 2, true, backend, store);

    let env = sample_envelope("planner", "task 1");
    let task_id = env.task_id().unwrap().to_string();
    let session = dispatch(&state, &env).unwrap();
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (io_client, mut client_inbound) = AdapterIo::new_with_inbound(client_io, Duration::from_secs(2), Duration::from_secs(2));
    let (io_server, _server_inbound) = AdapterIo::new_with_inbound(server_io, Duration::from_secs(2), Duration::from_secs(2));

    let (record_tx, mut record_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(frame) = client_inbound.recv().await {
            if let AdapterMsg::Host(HostOp::Assign(assign)) = frame.msg {
                let _ = record_tx.send(format!("assign:{}", assign.task_id));
            }
        }
    });

    on_ready(&state, ReadyNotice { task_id: task_id.clone(), session_id: session.task_id.clone(), generation: 1, io: io_server, capabilities: vec![Capability::Inject] }, "prose").await.unwrap();

    let recorded = tokio::time::timeout(Duration::from_secs(2), record_rx.recv()).await.unwrap().unwrap();
    assert_eq!(recorded, format!("assign:{}", task_id));
    drop(io_client);
    assert!(record_rx.try_recv().is_err());
}

#[test]
fn degradation_paths_cover_recycle_report_and_inject() {
    assert!(missing_capability(&[], Capability::Recycle));
    assert!(missing_capability(&[], Capability::Report));
    assert!(missing_capability(&[], Capability::Inject));

    let gaps = plugin_gap(&[]);
    assert_eq!(gaps.len(), 3);
    assert_eq!(gaps[0].capability, Capability::Recycle);
    assert_eq!(gaps[1].capability, Capability::Report);
    assert_eq!(gaps[2].capability, Capability::Inject);
}

#[test]
fn permanent_versus_retryable_error_split() {
    assert!(permanent_error(ErrorCode::AclDenied));
    assert!(permanent_error(ErrorCode::Invalid));
    assert!(permanent_error(ErrorCode::Conflict));
    assert!(permanent_error(ErrorCode::Forbidden));
    assert!(permanent_error(ErrorCode::UnknownRole));
    assert!(permanent_error(ErrorCode::NotAdmin));
    assert!(permanent_error(ErrorCode::BadFrame));
    assert!(permanent_error(ErrorCode::FrameTooLarge));
    assert!(permanent_error(ErrorCode::ProtocolVersion));

    assert!(!permanent_error(ErrorCode::Internal));
    assert!(!permanent_error(ErrorCode::Duplicate));
    assert!(!permanent_error(ErrorCode::RecipientOffline));
}

#[tokio::test]
async fn pinned_tls_link_fetches_welcome_and_caches_prose() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join(".onlyne/client.db")).unwrap();

    // A role key on disk, exactly the file `init` writes.
    let keypair = KeyPair::generate();
    let key_path = dir.path().join(".onlyne/keys/role.key");
    std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
    keypair.save(&key_path).unwrap();

    // A self-signed server whose pin the client is told in advance.
    let certificate = gen_self_signed("127.0.0.1", 1).unwrap();
    let config = server_config(&certificate).unwrap();
    let mut listener = TcpListen::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let table = table_from([("planner".to_string(), keypair.public_str(), false, Vec::new(), Vec::new())]).unwrap();

    let welcome = Welcome {
        cluster: "cluster-b".into(),
        server: "srv".into(),
        role: "planner".into(),
        admin: false,
        max_sessions: 2,
        reuse: true,
        prose: "cluster b exposes planner".into(),
        spec_hash: "hash-spec-b".into(),
        allowed_targets: vec![],
        allowed_senders: vec![],
        session_command: None,
        timeout_ready_ms: None,
        timeout_running_ms: None,
        timeout_idle_ms: None,
        intent_attempts: Some(4),
        intent_backoff_ms: Some(vec![10, 20]),
        seq: 10,
    };

    let server = tokio::spawn(async move {
        let accepted = listener.accept_next(&config).await.unwrap();
        let TlsConn::Server(mut stream) = accepted else { panic!("server stream expected") };
        // The pinned TLS handshake then the challenge signature, both real.
        let ok = accept_handshake(&mut stream, &table, PROTOCOL_VERSION).await.unwrap();
        assert_eq!(ok.role, "planner");
        loop {
            let frame: Option<Frame<ClientOp>> = read_frame(&mut stream).await.unwrap();
            let Some(frame) = frame else { break };
            match frame {
                Frame::Req { id, op } => match op {
                    ClientOp::Hello(_) => write_frame(&mut stream, &Frame::ok(id, serde_json::to_value(&welcome).unwrap())).await.unwrap(),
                    ClientOp::Subscribe(_) => write_frame(&mut stream, &Frame::ok(id, serde_json::json!({"subscribed": true}))).await.unwrap(),
                    ClientOp::Pull(_) => write_frame(&mut stream, &Frame::ok(id, serde_json::json!({"deliveries": [], "seq": 11}))).await.unwrap(),
                    _ => write_frame(&mut stream, &Frame::ok(id, serde_json::json!({}))).await.unwrap(),
                },
                Frame::Bye { .. } => break,
                _ => {}
            }
        }
    });

    let init = ClientInit::new(dir.path(), "planner", endpoint.clone(), key_path.clone(), certificate.spki_pin.clone());
    let client = tokio::spawn(onlyne_client::run(init));

    let mut cached = None;
    for _ in 0..60 {
        if let Some(prose) = store.prose("planner").unwrap() {
            cached = Some(prose);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (prose, hash) = cached.expect("welcome prose reached the cache");
    assert_eq!(prose, "cluster b exposes planner");
    assert_eq!(hash, "hash-spec-b");

    let machine = IntentMachine::new(store.clone(), 3, vec![100]);
    let cli = LocalCli::with_role(machine, "planner");
    let res = cli.query_roles_local(&QueryRolesArgs { role: Some("planner".into()) }).unwrap();
    assert_eq!(res.data.unwrap()["roles"][0]["prose"], "cluster b exposes planner");

    // A wrong pin must fail the dial, which proves the pin is checked.
    let bad_pin = format!("sha256/{}", "0".repeat(64));
    let bad = ClientInit::new(dir.path(), "planner", endpoint, key_path, bad_pin);
    assert!(ClientLink::connect(&bad).await.is_err());

    client.abort();
    server.abort();
}

/// Records every lifecycle frame in the order the dispatcher sent it.
#[derive(Clone, Default)]
struct RecordingOutbox {
    frames: Arc<tokio::sync::Mutex<Vec<ClientOp>>>,
}

impl RecordingOutbox {
    async fn frames(&self) -> Vec<ClientOp> {
        self.frames.lock().await.clone()
    }

    async fn kinds(&self) -> Vec<&'static str> {
        self.frames().await.iter().map(|op| op.name()).collect()
    }
}

impl onlyne_client::dispatch::Outbox for RecordingOutbox {
    fn send(&self, op: ClientOp) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), onlyne_net::NetError>> + Send + '_>> {
        Box::pin(async move {
            self.frames.lock().await.push(op);
            Ok(())
        })
    }
}

/// Spawn one task and settle its adapter transport; the caller reads the order.
async fn spawn_ready(state: &DispatchState, text: &str) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let env = sample_envelope("planner", text);
    let task_id = env.task_id().unwrap().to_string();
    let session = dispatch(state, &env).unwrap();
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (io_client, mut client_inbound) = AdapterIo::new_with_inbound(client_io, Duration::from_secs(2), Duration::from_secs(2));
    let (io_server, _server_inbound) = AdapterIo::new_with_inbound(server_io, Duration::from_secs(2), Duration::from_secs(2));
    let (record_tx, record_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(frame) = client_inbound.recv().await {
            if let AdapterMsg::Host(HostOp::Assign(assign)) = frame.msg {
                let _ = record_tx.send(format!("assign:{}", assign.task_id));
            }
        }
        drop(io_client);
    });
    on_ready(state, ReadyNotice { task_id: task_id.clone(), session_id: session.task_id.clone(), generation: 1, io: io_server, capabilities: vec![Capability::Inject] }, "prose").await.unwrap();
    (task_id, record_rx)
}

#[tokio::test]
async fn ready_is_reported_before_assign() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new("planner", dir.path(), vec!["echo".into()], 2, true, backend, store);
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());

    let (task_id, mut record_rx) = spawn_ready(&state, "task 1").await;
    let recorded = tokio::time::timeout(Duration::from_secs(2), record_rx.recv()).await.unwrap().unwrap();
    assert_eq!(recorded, format!("assign:{}", task_id));

    let kinds = outbox.kinds().await;
    assert_eq!(kinds.first(), Some(&"report"), "the ready report leaves first: {kinds:?}");
    assert!(kinds.contains(&"session_sync"), "the projection follows the report: {kinds:?}");
}

#[tokio::test]
async fn lifecycle_write_emits_session_sync() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new("planner", dir.path(), vec!["echo".into()], 2, true, backend, store.clone());
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    let (task_id, _assigns) = spawn_ready(&state, "task 1").await;

    // The turn ended: the tuple moves to idle, which is a public state change.
    let body = onlyne_session::Observation::build(
        onlyne_session::Version::new(1, 9),
        true,
        0,
        0,
        0,
        onlyne_session::AgentState::Idle,
        onlyne_session::DeliveryState::None,
        onlyne_session::ResourceState::Attached,
        onlyne_session::RecoveryState::None,
        onlyne_session::Outcome::Pending,
    );
    on_plugin_report(&state, onlyne_proto::Report::Heartbeat { task_id: task_id.clone(), generation: 1, seq: 9, observed: serde_json::to_value(&body).unwrap(), cluster_ref: None }).await.unwrap();

    let frames = outbox.frames().await;
    let syncs: Vec<&ClientOp> = frames.iter().filter(|op| matches!(op, ClientOp::SessionSync(_))).collect();
    let Some(ClientOp::SessionSync(args)) = syncs.last() else { panic!("a lifecycle write must publish its projection") };
    assert_eq!(args.task_id, task_id);
    assert_eq!(args.projection.agent, onlyne_proto::AgentPhase::Idle);
    assert_eq!(store.get_session(&task_id).unwrap().unwrap().agent_state, "idle");
}

#[tokio::test]
async fn report_when_link_down_lands_in_intents() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    // No outbox is installed, so the report takes the durable path.
    let state = DispatchState::new("planner", dir.path(), vec!["echo".into()], 2, true, backend, store.clone());

    let (task_id, _assigns) = spawn_ready(&state, "task 1").await;

    assert!(!state.accept_new().load(std::sync::atomic::Ordering::SeqCst), "a frame that could not be sent parks intake");
    assert_eq!(store.get_session(&task_id).unwrap().unwrap().agent_state, "ready", "the local tuple still advanced");
    let queued: Vec<ClientOp> = store.flush_order().unwrap().iter().map(|row| op_for_intent(row).unwrap()).collect();
    assert!(queued.iter().any(|op| matches!(op, ClientOp::Report(_))), "the frame reached the outbox: {:?}", queued.len());
}
