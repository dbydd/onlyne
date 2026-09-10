use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use onlyne_adapter::AdapterIo;
use onlyne_proto::Capability;
use onlyne_client::{
    adapter_socket::AdapterSocket,
    dispatch::{DispatchState, dispatch, on_ready, on_recycled, missing_capability, plugin_gap},
    init::{InitArgs, init, legacy_error_code},
    intent::{IntentMachine, IntentResult, permanent_error},
    local_cli::LocalCli,
    runloop::{ClientInit, RunState, run_connection},
};
use onlyne_frame::{read_frame, write_frame};
use onlyne_proto::{
    AdapterMsg, ClientOp, Envelope, ErrorCode, Frame, HostOp, MsgKind,
    QueryRolesArgs, Receipt, ResBody, Welcome,
    new_envelope, new_task_id,
};
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
    })).unwrap();

    assert!(fragment.starts_with("[[client]]\n"));
    assert!(fragment.contains("role = \"planner\""));
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
        prose: "prose".into(),
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
    let state = DispatchState::new("planner", dir.path(), vec!["echo".into()], 2, true, backend, store);

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
    assert_eq!(s1.task_id, s2.task_id);
    assert_eq!(state.session_count(), 1);

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

    on_ready(&state, &task_id, &session.task_id, 1, io_server, vec![Capability::Inject], &env, "prose").await.unwrap();

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
async fn scripted_server_duplex_welcome_and_prose_caching() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let init = ClientInit::new(dir.path(), "planner", "127.0.0.1:7899", "key", "sha256/pin");
    let state = RunState::new(&init, store.clone()).unwrap();

    let (client, mut server) = tokio::io::duplex(64 * 1024);

    let server_task = tokio::spawn(async move {
        // Read client Hello
        let frame: Option<Frame<ClientOp>> = read_frame(&mut server).await.unwrap();
        let Frame::Req { id, op } = frame.unwrap() else { panic!("expected req"); };
        assert!(matches!(op, ClientOp::Hello(_)));

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
            intent_attempts: None,
            intent_backoff_ms: None,
            seq: 10,
        };
        write_frame(&mut server, &Frame::ok(id, serde_json::to_value(&welcome).unwrap())).await.unwrap();

        // Send bye to close connection cleanly
        write_frame(&mut server, &Frame::<ClientOp>::bye("test complete")).await.unwrap();
    });

    let welcome = run_connection(init, client, state).await.unwrap();
    assert_eq!(welcome.prose, "cluster b exposes planner");

    server_task.await.unwrap();

    // Verify stored prose
    assert_eq!(store.prose("planner").unwrap(), Some(("cluster b exposes planner".into(), "hash-spec-b".into())));

    // Verify local query roles answers with prose
    let machine = IntentMachine::new(store.clone(), 3, vec![100]);
    let cli = LocalCli::with_role(machine, "planner");
    let res = cli.query_roles_local(&QueryRolesArgs { role: Some("planner".into()) }).unwrap();
    assert_eq!(res.data.unwrap()["roles"][0]["prose"], "cluster b exposes planner");
}
