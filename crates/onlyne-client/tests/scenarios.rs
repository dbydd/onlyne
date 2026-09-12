use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use onlyne_adapter::AdapterIo;
use onlyne_client::{
    accept::AcceptPath,
    adapter_socket::AdapterSocket,
    dispatch::{
        ClientLink, DispatchState, ReadyNotice, dispatch, missing_capability, on_plugin_report,
        on_ready, on_recycled, plugin_gap, projection_of, session_alive,
    },
    init::{InitArgs, init, legacy_error_code},
    intent::{IntentMachine, IntentResult, op_for_intent, permanent_error},
    local_cli::LocalCli,
    runloop::ClientInit,
};
use onlyne_frame::{read_frame, write_frame};
use onlyne_net::{
    KeyPair, TcpListen, TlsConn, accept as accept_handshake, gen_self_signed, server_config,
    table_from,
};
use onlyne_proto::{
    AdapterMsg, AgentMount, AssignAckArgs, Capability, ClientOp, Delivery, DetachArgs, Envelope,
    ErrorCode, Frame, HelloArgs, HostOp, Lifecycle, Mount, MountKind, MsgKind, Outcome,
    PROTOCOL_VERSION, PluginOp, QueryRolesArgs, Receipt, Report, ResBody, Welcome, new_envelope,
    new_task_id,
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
    let entries: Vec<_> = std::fs::read_dir(dir.path().join(".onlyne"))
        .unwrap()
        .collect();
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
    let fragment = rt
        .block_on(init(InitArgs {
            workspace: ws_dir.path().to_path_buf(),
            role: "planner".into(),
            server_root: server_dir.path().to_path_buf(),
            prose: "v1 smoke prose".into(),
        }))
        .unwrap();
    assert!(fragment.starts_with("[[client]]\n"));
    let lines: Vec<&str> = fragment.lines().collect();
    assert_eq!(lines.len(), 10, "fragment shape is fixed: {fragment:?}");
    assert_eq!(lines[0], "[[client]]");
    assert_eq!(lines[1], "role = \"planner\"");
    assert!(
        lines[2].starts_with("key = \"ed25519/"),
        "key line is {}",
        lines[2]
    );
    assert_eq!(
        &lines[3..],
        &[
            "admin = false",
            "max_sessions = 1",
            "allowed_senders = [\"*\", \"planner\"]",
            "allowed_targets = [\"planner\"]",
            "prose = \"v1 smoke prose\"",
            "reuse = true",
            "session_command = [\"pi\", \"--session-id\", \"{session}\", \"--session-dir\", \".pi/sessions\", \"-ns\"]",
        ]
    );
    assert!(fragment.ends_with(
        "session_command = [\"pi\", \"--session-id\", \"{session}\", \"--session-dir\", \".pi/sessions\", \"-ns\"]\n"
    ));
    assert!(fragment.contains("key = \"ed25519/"));

    let key_path = ws_dir.path().join(".onlyne/keys/role.key");
    assert!(key_path.exists());
    let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    // The fragment publishes the public half of the stored seed, and that
    // string is a curve point: a fragment carrying the seed instead publishes
    // the private half and fails `parse_public` for roughly half of all seeds.
    let stored = onlyne_net::KeyPair::load(&key_path).unwrap();
    let published = lines[2].strip_prefix("key = ").unwrap().trim_matches('"');
    assert_eq!(published, stored.public_str());
    assert!(onlyne_net::parse_public(published).is_ok());

    // Stale socket cleanup and socket mode 0600
    let db_path = ws_dir.path().join(".onlyne/client.db");
    let store = ClientStore::open(&db_path).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let dispatch = DispatchState::new(
        "planner",
        ws_dir.path(),
        vec!["agent".into()],
        2,
        true,
        backend,
        store,
    );
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

/// The printed fragment is a complete role entry: pasting it into `spec.toml`
/// and reloading yields a role whose `session_command` the client can spawn.
///
/// A fragment without that line parses and registers, and then leaves every
/// delivery staged with no process behind it: the box the operator assembled by
/// hand parks its tasks `in_flight` and no component says why. The seed is the
/// one `examples/supervisor/run.py` writes into its own ring entries.
#[test]
fn init_fragment_is_a_pasteable_spawnable_role() {
    let ws_dir = tempdir().unwrap();
    let server_dir = tempdir().unwrap();
    let server_spec = server_dir.path().join(".onlyne/spec.toml");
    std::fs::create_dir_all(server_spec.parent().unwrap()).unwrap();
    std::fs::write(&server_spec, "[server]\nname = \"srv\"\nlisten = \"127.0.0.1:7899\"\ncert_pin = \"sha256/0000000000000000000000000000000000000000000000000000000000000000\"\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let fragment = rt
        .block_on(init(InitArgs {
            workspace: ws_dir.path().to_path_buf(),
            role: "planner".into(),
            server_root: server_dir.path().to_path_buf(),
            prose: "v1 smoke prose".into(),
        }))
        .unwrap();

    let spec = format!(
        "[server]\nname = \"srv\"\nlisten = \"127.0.0.1:7899\"\ncert_pin = \"sha256/0000000000000000000000000000000000000000000000000000000000000000\"\n\n{fragment}"
    );
    let parsed = onlyne_config::Spec::parse_str(&spec).expect("the pasted fragment is a spec");
    let planner = parsed
        .client
        .iter()
        .find(|entry| entry.role == "planner")
        .expect("the fragment registers the role");
    assert_eq!(
        planner.session_command,
        vec![
            "pi",
            "--session-id",
            "{session}",
            "--session-dir",
            ".pi/sessions",
            "-ns"
        ],
        "a registered role carries the spawn command the client renders per task"
    );
}

/// An admin `hello` reaches the host as `mount: null`, because the untagged
/// `Mount` enum writes its unit variant that way and `Option<Mount>` reads
/// `null` back as an absent mount. Admission reads `HelloArgs::kind`, which is
/// the discriminant the split names, and this case crosses a real socket, so
/// the encoding itself is what the assertion covers.
#[tokio::test]
async fn an_admin_hello_survives_the_wire_and_is_admitted() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let dispatch = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        false,
        backend,
        store,
    );
    let adapter = AdapterSocket {
        workspace: dir.path().to_path_buf(),
        role: "planner".into(),
        cluster: "c".into(),
        server: "s".into(),
        dispatch,
    };
    let socket = adapter.path();
    let host = tokio::spawn(adapter.clone().serve());
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(socket.exists(), "the host bound {}", socket.display());

    let admin = HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: "onlyne-client-cli:demo".into(),
        version: "1.0.0".into(),
        kind: MountKind::Admin,
        capabilities: Vec::new(),
        mount: Some(Mount::Admin),
    };
    let encoded = serde_json::to_value(&admin).unwrap();
    assert_eq!(
        encoded["mount"],
        serde_json::Value::Null,
        "an untagged unit variant writes null"
    );
    let decoded: HelloArgs = serde_json::from_value(encoded).unwrap();
    assert!(decoded.mount.is_none(), "null decodes as an absent mount");
    assert_eq!(decoded.kind, MountKind::Admin);

    let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let io = AdapterIo::new(stream, Duration::from_secs(2), Duration::from_secs(2));
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Hello(admin)))
        .await
        .unwrap();
    assert!(
        body.ok,
        "an admin probe crosses the socket and is admitted: {body:?}"
    );
    let HostOp::Welcome(ack) =
        serde_json::from_value::<HostOp>(body.data.unwrap()).expect("welcome ack")
    else {
        panic!("the ack payload names no welcome");
    };
    assert_eq!(ack.role, "planner");

    let anonymous = HelloArgs {
        plugin: "onlyne-agent-anonymous".into(),
        kind: MountKind::Agent,
        mount: None,
        ..decoded
    };
    let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let io = AdapterIo::new(stream, Duration::from_secs(2), Duration::from_secs(2));
    let refused = io
        .request(AdapterMsg::Plugin(PluginOp::Hello(anonymous)))
        .await
        .unwrap();
    assert!(!refused.ok, "kind gates admission: {refused:?}");
    assert_eq!(
        refused.error.map(|error| error.code),
        Some(ErrorCode::Forbidden)
    );
    host.abort();
}

/// `onlyne ping --workspace <dir>` opens the role socket, sends a bare
/// `Frame::Ping`, and reads the pong. The host answers that probe in place, so
/// the connection the caller opened survives the exchange and a later request
/// on it still reaches the local vocabulary.
#[tokio::test]
async fn a_local_ping_is_answered_and_keeps_the_socket_open() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        false,
        backend,
        store,
    );
    let (socket, host) = serve_role_socket(&state, dir.path()).await;
    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();

    write_frame(&mut stream, &Frame::<ClientOp>::Ping { t: 4_242 })
        .await
        .unwrap();
    let answer: Frame<ClientOp> = read_frame(&mut stream)
        .await
        .unwrap()
        .expect("the probe is answered before the socket closes");
    assert_eq!(
        answer,
        Frame::<ClientOp>::Pong {
            t: 4_242,
            server_seq: 0
        },
        "the pong echoes the probe's clock"
    );

    // The same connection still serves the local surface, which is what the
    // probe's caller reads after its pong.
    write_frame(
        &mut stream,
        &Frame::<ClientOp>::req("r1", ClientOp::QueryRoles(QueryRolesArgs { role: None })),
    )
    .await
    .unwrap();
    let answer: Frame<ClientOp> = read_frame(&mut stream)
        .await
        .unwrap()
        .expect("the connection is still open");
    match answer {
        Frame::Res { id, body } => {
            assert_eq!(id, "r1");
            assert!(body.ok, "the roles query answers: {body:?}");
        }
        other => panic!("expected a res frame, got {other:?}"),
    }
    host.abort();
}

/// `status` asks the running client whether its server link is up, and that
/// answer is what its exit code reports.
///
/// The probe is an `admin` `hello` on the role socket: the client's own runtime
/// owns the connection flag, so the verb reads the fact from the process that
/// holds it instead of guessing from the pid file. A socket that answers
/// nothing is a client that is not serving.
#[tokio::test]
async fn the_link_probe_follows_the_clients_connection() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        false,
        backend,
        store,
    );
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    assert!(
        !onlyne_client::adapter_socket::server_link_up(&socket).await,
        "a client that holds no link is not connected"
    );
    state.set_link_up(true);
    assert!(
        onlyne_client::adapter_socket::server_link_up(&socket).await,
        "the probe reads the connection the runtime holds"
    );
    state.set_link_up(false);
    assert!(
        !onlyne_client::adapter_socket::server_link_up(&socket).await,
        "a dropped link is reported as not connected"
    );
    host.abort();

    let absent = dir.path().join(".onlyne/run/absent");
    assert!(
        !onlyne_client::adapter_socket::server_link_up(&absent).await,
        "nothing listening is not connected"
    );
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

    let res = machine
        .attempt(
            &rows[0],
            Some(&ResBody::err(ErrorCode::AclDenied, "acl denied", None)),
        )
        .unwrap();
    assert!(matches!(
        res,
        IntentResult::Dropped(ErrorCode::AclDenied, _)
    ));
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
    };
    let body = ResBody::err_with_data(
        ErrorCode::Duplicate,
        "duplicate",
        None,
        serde_json::to_value(&receipt).unwrap(),
    );

    let res = machine.attempt(&rows[0], Some(&body)).unwrap();
    assert!(matches!(res, IntentResult::Accepted(Some(r)) if r.msg_id == "msg-1"));
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
    let res1 = machine
        .attempt(
            row,
            Some(&ResBody::err(ErrorCode::Internal, "internal error", None)),
        )
        .unwrap();
    assert!(matches!(
        res1,
        IntentResult::Retryable(ErrorCode::Internal, _)
    ));

    // Update row attempt to 2
    let rows = store.flush_order().unwrap();
    let res2 = machine
        .attempt(
            &rows[0],
            Some(&ResBody::err(ErrorCode::Internal, "internal error", None)),
        )
        .unwrap();
    assert!(matches!(
        res2,
        IntentResult::Retryable(ErrorCode::Internal, _)
    ));

    // Attempt 3 reaches attempts ceiling (3) -> Exhausted
    let rows = store.flush_order().unwrap();
    let res3 = machine
        .attempt(
            &rows[0],
            Some(&ResBody::err(ErrorCode::Internal, "internal error", None)),
        )
        .unwrap();
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
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        true,
        backend,
        store.clone(),
    );

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
    on_recycled(
        &state,
        env1.task_id().unwrap(),
        onlyne_session::CloseReason::Completed,
    )
    .unwrap();

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
    assert_eq!(
        s1.backend_ref, s2.backend_ref,
        "the same backend resource carries the next task"
    );
    assert_eq!(state.session_count(), 1);
    let reused_row = store
        .get_session(env2.task_id().unwrap())
        .unwrap()
        .expect("the reused task has a ledger row");
    assert_ne!(
        reused_row.backend_ref.trim(),
        "{}",
        "a reused session must resolve to its backend resource"
    );

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

/// A settled session spends no concurrency.
///
/// §5's `max_sessions` caps the sessions a role has running, and the rows of
/// the sessions it has ended stay in `client.db` as the role's own history.
/// The live ring held six and seven exited rows per role against a cap of two,
/// and a role whose count reached the cap stops pulling: every later task parks
/// `in_flight` on the server with nothing on the client side saying why. Both
/// exit routes reach that state — the completion report, and the settled
/// observation the plugin sends as its last heartbeat — so neither holds
/// capacity, and the exited rows stay queryable.
#[tokio::test]
async fn exited_sessions_do_not_hold_the_capacity_cap() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        2,
        false,
        backend.clone(),
        store.clone(),
    );

    // Task 1 ends through the completion report, which gives its slot back.
    let first = deliver(&state, &task_delivery("task 1")).await;
    on_plugin_report(
        &state,
        Report::Complete {
            task_id: first.clone(),
            outcome: Outcome::Done,
            head: Some("done".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        store.get_session(&first).unwrap().unwrap().public_lifecycle,
        "exited"
    );

    // Tasks 2 and 3 end through the plugin's settled observation, the last
    // heartbeat of a session that finished: the row reads exited while the slot
    // the client staged stays where it is.
    for text in ["task 2", "task 3"] {
        let task_id = deliver(&state, &task_delivery(text)).await;
        let settled = onlyne_session::Observation::build(
            onlyne_session::Version::new(1, 3),
            true,
            onlyne_session::DEFAULT_ISOLATE_AFTER,
            onlyne_session::DEFAULT_TERMINATE_AFTER,
            0,
            onlyne_session::AgentState::Idle,
            onlyne_session::DeliveryState::Accepted,
            onlyne_session::ResourceState::Attached,
            onlyne_session::RecoveryState::Draining,
            onlyne_session::Outcome::Done,
        );
        on_plugin_report(
            &state,
            Report::Heartbeat {
                task_id: task_id.clone(),
                generation: 1,
                seq: 3,
                observed: serde_json::to_value(&settled).unwrap(),
                cluster_ref: None,
            },
        )
        .await
        .unwrap();
        let row = store
            .get_session(&task_id)
            .unwrap()
            .expect("the row is kept");
        assert_eq!(
            row.public_lifecycle, "exited",
            "{text} exits on its settled observation"
        );
    }

    // The two slots whose settled observation arrived last are still staged,
    // which is the role at its cap with every one of them exited.
    assert_eq!(state.session_count(), 2);
    assert!(state.has_capacity());
    let fourth = deliver(&state, &task_delivery("task 4")).await;
    assert_ne!(fourth, first);
    assert_eq!(
        backend.sessions().len(),
        4,
        "every task spawned its own resource"
    );
    // The rows behind the cap spend nothing and stay readable.
    assert_eq!(
        store.get_session(&first).unwrap().unwrap().public_lifecycle,
        "exited"
    );
}

#[test]
fn redelivered_task_keeps_its_one_session() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        true,
        backend.clone(),
        store,
    );

    let envelope = sample_envelope("planner", "task 1");
    let first = dispatch(&state, &envelope).unwrap();
    let again = dispatch(&state, &envelope).unwrap();

    assert_eq!(first.task_id, again.task_id);
    assert_eq!(
        backend.sessions().len(),
        1,
        "a redelivery must not spawn a second resource"
    );
    assert_eq!(state.session_count(), 1);
}

#[tokio::test]
async fn ready_barrier_orders_assign_after_ready() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        true,
        backend,
        store,
    );

    let env = sample_envelope("planner", "task 1");
    let task_id = env.task_id().unwrap().to_string();
    let session = dispatch(&state, &env).unwrap();
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (io_client, mut client_inbound) =
        AdapterIo::new_with_inbound(client_io, Duration::from_secs(2), Duration::from_secs(2));
    let (io_server, _server_inbound) =
        AdapterIo::new_with_inbound(server_io, Duration::from_secs(2), Duration::from_secs(2));

    let (record_tx, mut record_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(frame) = client_inbound.recv().await {
            if let AdapterMsg::Host(HostOp::Assign(assign)) = frame.msg {
                let _ = record_tx.send(format!("assign:{}", assign.task_id));
            }
        }
    });

    on_ready(
        &state,
        ReadyNotice {
            task_id: task_id.clone(),
            session_id: session.task_id.clone(),
            generation: 1,
            io: io_server,
            capabilities: vec![Capability::Inject],
        },
        "prose",
    )
    .await
    .unwrap();

    let recorded = tokio::time::timeout(Duration::from_secs(2), record_rx.recv())
        .await
        .unwrap()
        .unwrap();
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
    // The server answers `invalid` for conditions a retry clears, such as a frame
    // that arrives before the routed `hello`, so the row ends as an observable
    // fault rather than a silent deletion.
    assert!(!permanent_error(ErrorCode::Invalid));
}

#[test]
fn intent_survives_a_frame_refused_before_the_routed_hello() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let machine = IntentMachine::new(store.clone(), 3, vec![1000, 2000]);

    let envelope = sample_envelope("planner", "hello");
    machine.enqueue(&envelope).unwrap();
    let rows = store.flush_order().unwrap();

    // The link layer redials on its own, so the first frame of a fresh
    // connection can reach a session that has not read `hello` yet.
    let res = machine
        .attempt(
            &rows[0],
            Some(&ResBody::err(
                ErrorCode::Invalid,
                onlyne_proto::HELLO_REQUIRED_MESSAGE,
                Some("op".to_string()),
            )),
        )
        .unwrap();
    assert!(matches!(
        res,
        IntentResult::Retryable(ErrorCode::Internal, _)
    ));
    assert_eq!(store.pending_intent_count().unwrap(), 1);
    let kept = store.flush_order().unwrap();
    assert_eq!(kept[0].attempt, rows[0].attempt);
}

#[test]
fn assign_ack_rejection_queues_a_rejected_delivery_ack() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        false,
        Arc::new(FakeBackend::new()),
        store.clone(),
    );
    let env = sample_envelope("planner", "reject me");
    let task_id = env.task_id().unwrap().to_string();
    dispatch(&state, &env).unwrap();
    state.attach_msg_id(&task_id, "msg-reject");

    assert!(state.push_assign_ack(AssignAckArgs {
        task_id: task_id.clone(),
        accepted: false,
        reason: Some("already injected conflict".into()),
    }));
    let rows = store.flush_order().unwrap();
    assert_eq!(rows.len(), 1);
    let op = op_for_intent(&rows[0]).unwrap();
    let ClientOp::Ack(ack) = op else {
        panic!("assign rejection must queue a delivery ack, got {op:?}");
    };
    assert_eq!(ack.msg_id, "msg-reject");
    assert!(!ack.accepted);
    assert_eq!(ack.reason.as_deref(), Some("already injected conflict"));

    assert!(!state.push_assign_ack(AssignAckArgs {
        task_id,
        accepted: true,
        reason: None,
    }));
    assert_eq!(
        store.flush_order().unwrap().len(),
        1,
        "accepted assign_ack is still non-terminal"
    );
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
    let table = table_from([(
        "planner".to_string(),
        keypair.public_str(),
        false,
        Vec::new(),
        Vec::new(),
    )])
    .unwrap();
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));

    let welcome = Welcome {
        cluster: "cluster-b".into(),
        server: "srv".into(),
        role: "planner".into(),
        admin: false,
        max_sessions: 2,
        reuse: true,
        prose: "cluster b exposes planner".into(),
        spec_hash: "hash-spec-b".into(),
        aggregate: Some("cluster-b".into()),
        allowed_targets: vec![],
        allowed_senders: vec![],
        session_command: None,
        timeout_ready_ms: None,
        timeout_running_ms: None,
        timeout_idle_ms: None,
        intent_attempts: Some(4),
        intent_backoff_ms: Some(vec![10, 20]),
        relay_required: None,
        relay_count: None,
        seq: 10,
    };

    let seen_server = seen.clone();
    let server = tokio::spawn(async move {
        // Stay listening for the whole test, so the wrong-pin dial meets the
        // pin check on a listening socket.
        loop {
            let Ok(accepted) = listener.accept_next(&config).await else {
                break;
            };
            let table = table.clone();
            let welcome = welcome.clone();
            let seen = seen_server.clone();
            tokio::spawn(async move {
                let TlsConn::Server(mut stream) = accepted else {
                    panic!("server stream expected")
                };
                // The pinned TLS handshake then the challenge signature, both real.
                let Ok(ok) = accept_handshake(&mut stream, &table, PROTOCOL_VERSION).await else {
                    return;
                };
                assert_eq!(ok.role, "planner");
                loop {
                    let frame: Option<Frame<ClientOp>> = match read_frame(&mut stream).await {
                        Ok(frame) => frame,
                        Err(_) => break,
                    };
                    let Some(frame) = frame else { break };
                    match frame {
                        Frame::Req { id, op } => {
                            seen.lock()
                                .unwrap()
                                .push(serde_json::to_value(&op).unwrap());
                            match op {
                                ClientOp::Hello(_) => write_frame(
                                    &mut stream,
                                    &Frame::ok(id, serde_json::to_value(&welcome).unwrap()),
                                )
                                .await
                                .unwrap(),
                                ClientOp::Subscribe(_) => write_frame(
                                    &mut stream,
                                    &Frame::ok(id, serde_json::json!({"subscribed": true})),
                                )
                                .await
                                .unwrap(),
                                ClientOp::Pull(_) => write_frame(
                                    &mut stream,
                                    &Frame::ok(
                                        id,
                                        serde_json::json!({"deliveries": [], "seq": 11}),
                                    ),
                                )
                                .await
                                .unwrap(),
                                _ => {
                                    write_frame(&mut stream, &Frame::ok(id, serde_json::json!({})))
                                        .await
                                        .unwrap()
                                }
                            }
                        }
                        Frame::Bye { .. } => break,
                        _ => {}
                    }
                }
            });
        }
    });

    // A report queued while the link was down: the flusher must carry the
    // cluster name the welcome advertised, not the `None` it was stored with.
    let queued = ClientOp::Report(Report::Ready {
        task_id: "task-1".into(),
        session_id: "s1".into(),
        generation: 1,
        seq: 1,
        cluster_ref: None,
    });
    store
        .enqueue_intent("report-1", &serde_json::to_value(&queued).unwrap())
        .unwrap();
    let init = ClientInit::new(
        dir.path(),
        "planner",
        endpoint.clone(),
        key_path.clone(),
        certificate.spki_pin.clone(),
    );
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

    let mut report_frame = None;
    for _ in 0..60 {
        if let Some(found) = seen
            .lock()
            .unwrap()
            .iter()
            .find(|op| serde_json::to_string(op).unwrap().contains("cluster_ref"))
        {
            report_frame = Some(serde_json::to_string(found).unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let report_frame = report_frame.expect("the queued report reached the server");
    assert!(
        report_frame.contains("\"cluster_ref\":\"cluster-b\""),
        "the aggregate name comes from the welcome, not the stored op: {report_frame}"
    );

    let machine = IntentMachine::new(store.clone(), 3, vec![100]);
    let cli = LocalCli::with_role(machine, "planner");
    let res = cli
        .query_roles_local(&QueryRolesArgs {
            role: Some("planner".into()),
        })
        .unwrap();
    assert_eq!(
        res.data.unwrap()["roles"][0]["prose"],
        "cluster b exposes planner"
    );

    // A wrong pin must fail the dial inside the bound, which proves the pin is checked.
    let bad_pin = format!("sha256/{}", "0".repeat(64));
    let bad = ClientInit::new(dir.path(), "planner", endpoint, key_path, bad_pin);
    let refusal = tokio::time::timeout(Duration::from_secs(3), ClientLink::connect(&bad))
        .await
        .expect("the wrong-pin dial must answer inside 3s");
    assert!(
        matches!(refusal, Err(onlyne_net::NetError::PinMismatch { .. })),
        "a wrong pin is refused as a pin mismatch"
    );

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
async fn spawn_ready(
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
            io: io_server,
            capabilities: vec![Capability::Inject],
        },
        "prose",
    )
    .await
    .unwrap();
    (task_id, record_rx)
}

#[tokio::test]
async fn ready_is_reported_before_assign() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        true,
        backend,
        store,
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());

    let (task_id, mut record_rx) = spawn_ready(&state, "task 1").await;
    let recorded = tokio::time::timeout(Duration::from_secs(2), record_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recorded, format!("assign:{}", task_id));

    let kinds = outbox.kinds().await;
    assert_eq!(
        kinds.first(),
        Some(&"report"),
        "the ready report leaves first: {kinds:?}"
    );
    assert!(
        kinds.contains(&"session_sync"),
        "the projection follows the report: {kinds:?}"
    );
}

#[tokio::test]
async fn lifecycle_write_emits_session_sync() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        true,
        backend,
        store.clone(),
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    let (task_id, _assigns) = spawn_ready(&state, "task 1").await;

    // A heartbeat whose version is the stored watermark plus one, and whose
    // body is the stored observation with the agent idle: a legal transition.
    let row = store.get_session(&task_id).unwrap().unwrap();
    let mut body: onlyne_session::Observation = serde_json::from_str(&row.observed_json).unwrap();
    body.agent = onlyne_session::AgentState::Idle;
    body.version = onlyne_session::Version::new(row.generation as u64, row.seq as u64 + 1);
    on_plugin_report(
        &state,
        onlyne_proto::Report::Heartbeat {
            task_id: task_id.clone(),
            generation: row.generation as u64,
            seq: row.seq as u64 + 1,
            observed: serde_json::to_value(body).unwrap(),
            cluster_ref: None,
        },
    )
    .await
    .unwrap();

    let frames = outbox.frames().await;
    let syncs: Vec<&ClientOp> = frames
        .iter()
        .filter(|op| matches!(op, ClientOp::SessionSync(_)))
        .collect();
    let Some(ClientOp::SessionSync(args)) = syncs.last() else {
        panic!("a lifecycle write must publish its projection")
    };
    assert_eq!(args.task_id, task_id);
    assert_eq!(args.projection.agent, onlyne_proto::AgentPhase::Idle);
    assert_eq!(
        store.get_session(&task_id).unwrap().unwrap().agent_state,
        "idle"
    );
}

#[tokio::test]
async fn report_when_link_down_lands_in_intents() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    // No outbox is installed, so the report takes the durable path.
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        true,
        backend,
        store.clone(),
    );

    let (task_id, _assigns) = spawn_ready(&state, "task 1").await;

    assert!(
        !state.accept_new().load(std::sync::atomic::Ordering::SeqCst),
        "a frame that could not be sent parks intake"
    );
    assert_eq!(
        store.get_session(&task_id).unwrap().unwrap().agent_state,
        "ready",
        "the local tuple advanced"
    );
    let queued: Vec<ClientOp> = store
        .flush_order()
        .unwrap()
        .iter()
        .map(|row| op_for_intent(row).unwrap())
        .collect();
    assert!(
        queued.iter().any(|op| matches!(op, ClientOp::Report(_))),
        "the frame reached the outbox: {:?}",
        queued.len()
    );
}

/// Reports the reason the dispatcher handed the backend.
#[derive(Clone, Default)]
struct ReasonBackend {
    inner: FakeBackend,
    reasons: Arc<parking_lot::Mutex<Vec<onlyne_session::CloseReason>>>,
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
        self.inner.attach(session)
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
        self.inner.close(session, reason, force)
    }
}

#[test]
fn cancelled_settle_closes_with_the_real_reason() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        false,
        backend.clone(),
        store,
    );

    let env = sample_envelope("planner", "task 1");
    let task_id = env.task_id().unwrap().to_string();
    dispatch(&state, &env).unwrap();
    on_recycled(&state, &task_id, onlyne_session::CloseReason::Cancelled).unwrap();

    assert_eq!(
        backend.reasons.lock().as_slice(),
        [onlyne_session::CloseReason::Cancelled]
    );
    assert_eq!(state.session_count(), 0);
}

#[test]
fn detached_tuple_sees_no_close_call() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        false,
        backend.clone(),
        store.clone(),
    );

    let env = sample_envelope("planner", "task 1");
    let task_id = env.task_id().unwrap().to_string();
    dispatch(&state, &env).unwrap();

    // Rewind the stored tuple to Detached at a higher watermark, which is the
    // state a probe-confirmed loss leaves behind.
    let row = store.get_session(&task_id).unwrap().unwrap();
    store
        .upsert_session(
            &task_id,
            &onlyne_session::VersionedSession {
                agent_state: row.agent_state.clone(),
                delivery_state: row.delivery_state.clone(),
                resource_state: "detached".to_string(),
                public_lifecycle: row.public_lifecycle.clone(),
                recovery_substate: row.recovery_substate.clone(),
                desired_json: row.desired_json.clone(),
                observed_json: row.observed_json.clone(),
                generation: row.generation,
                seq: row.seq + 1,
                backend_ref: row.backend_ref.clone(),
                mismatch_count: row.mismatch_count,
                updated_at: row.updated_at,
            },
        )
        .unwrap();

    on_recycled(&state, &task_id, onlyne_session::CloseReason::Completed).unwrap();

    assert!(
        backend.reasons.lock().is_empty(),
        "a detached tuple has no resource to close"
    );
    assert_eq!(state.session_count(), 0);
}

#[test]
fn a_supervisor_report_names_its_cluster() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec![],
        1,
        false,
        Arc::new(FakeBackend::new()),
        store,
    );
    state.set_cluster_ref("cluster-b");

    let report = onlyne_proto::Report::Ready {
        task_id: "t".into(),
        session_id: "t".into(),
        generation: 1,
        seq: 1,
        cluster_ref: None,
    };
    let value =
        serde_json::to_value(onlyne_client::dispatch::with_cluster(&state, report)).unwrap();
    assert_eq!(value["data"]["cluster_ref"], "cluster-b");
}

#[test]
fn a_plain_role_report_omits_the_cluster_key() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec![],
        1,
        false,
        Arc::new(FakeBackend::new()),
        store,
    );

    let report = onlyne_proto::Report::Ready {
        task_id: "t".into(),
        session_id: "t".into(),
        generation: 1,
        seq: 1,
        cluster_ref: None,
    };
    let value =
        serde_json::to_value(onlyne_client::dispatch::with_cluster(&state, report)).unwrap();
    assert!(
        value["data"].get("cluster_ref").is_none(),
        "a plain role omits the key: {value}"
    );
}

/// Remints the way Orca does: the first `attach` answers a different
/// reference, which the dispatcher has to persist before the next probe.
#[derive(Clone, Default)]
struct RemintBackend {
    inner: FakeBackend,
    probes: Arc<parking_lot::Mutex<Vec<String>>>,
    remints: Arc<parking_lot::Mutex<usize>>,
}

impl onlyne_session::SessionBackend for RemintBackend {
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
        let mut remints = self.remints.lock();
        if *remints > 0 {
            return Ok(session.clone());
        }
        *remints += 1;
        Ok(onlyne_session::SessionRef {
            backend_ref: serde_json::json!({"id": session.task_id, "handle": "term_two"}),
            generation: session.generation + 1,
            ..session.clone()
        })
    }
    fn probe(
        &self,
        session: &onlyne_session::SessionRef,
    ) -> anyhow::Result<onlyne_session::ResourceProbe> {
        self.probes.lock().push(session.backend_ref.to_string());
        self.inner.probe(session)
    }
    fn close(
        &self,
        session: &onlyne_session::SessionRef,
        reason: onlyne_session::CloseReason,
        force: bool,
    ) -> anyhow::Result<()> {
        self.inner.close(session, reason, force)
    }
}

#[test]
fn a_reminted_reference_is_written_back_before_the_next_probe() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(RemintBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        false,
        backend.clone(),
        store,
    );
    let env = sample_envelope("planner", "task 1");
    let task_id = env.task_id().unwrap().to_string();
    dispatch(&state, &env).unwrap();

    assert!(session_alive(&state, &task_id));
    assert!(session_alive(&state, &task_id));

    let probes = backend.probes.lock().clone();
    assert_eq!(probes.len(), 2, "{probes:?}");
    assert!(
        probes[0].contains("term_two"),
        "the remint reached the first probe: {probes:?}"
    );
    assert!(
        probes[1].contains("term_two"),
        "the reminted reference was persisted for the next probe: {probes:?}"
    );
    assert_eq!(
        *backend.remints.lock(),
        1,
        "one remint is enough once the slot holds the fresh handle"
    );
}

/// One delivery of a fresh task to this role, as the pull loop receives it.
fn task_delivery(text: &str) -> Delivery {
    Delivery {
        msg_id: format!("msg-{}", new_task_id()),
        envelope: Box::new(sample_envelope("planner", text)),
    }
}

/// Hand one delivery to this role the way the pull loop does: stage the
/// session, then route its payload to whichever connection serves it.
async fn deliver(state: &DispatchState, delivery: &Delivery) -> String {
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
async fn mount_plugin(
    socket: &Path,
    session: Option<&str>,
) -> (AdapterIo, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let stream = tokio::net::UnixStream::connect(socket)
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

/// Poll a state predicate so a socket-level hand-off is never raced.
async fn eventually(mut predicate: impl FnMut() -> bool, what: &str) {
    for _ in 0..400 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}
/// Bind the role socket and answer every connection on it until this returns.
async fn serve_role_socket(
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

/// A settled task is `exited`/`done` in the projection the client publishes.
fn assert_settled(store: &ClientStore, task_id: &str) {
    let row = store
        .get_session(task_id)
        .unwrap()
        .expect("the settled session keeps its row");
    let projection = projection_of(&row);
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
    // The whole tuple survives the settle: the projection a supervisor reads
    // must carry the resource dimension beside the lifecycle, so a row can
    // never read `exited` with a missing resource leg.
    let observed = projection
        .observed
        .as_ref()
        .expect("a settled session publishes its raw observation");
    assert!(
        observed.get("resource").is_some(),
        "the published tuple carries the resource dimension: {observed}"
    );
}

/// With `reuse = false` a second task must get a session and a connection of
/// its own, and it must be spawned by the client that is already running.
///
/// Before this case passed, a plugin's connection was remembered as the whole
/// role's transport (`DispatchInner::plugin_transport`), so the assignment of
/// the second task was written to the finished session's socket: the first
/// process answered it and no second session was ever spawned, which is the
/// live defect (task B's `onlyne-assign` entry inside task A's pi session
/// file, a single tab, two session rows).
///
/// The client itself is not part of a session's lifecycle: it stays up after
/// the session settles and serves the next task, which the admin hello at the
/// end asserts on the same socket.
#[tokio::test]
async fn reuse_off_gives_the_second_task_its_own_session_and_connection() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        2,
        false,
        backend.clone(),
        store.clone(),
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    // Task A: the session is staged, then its plugin mounts under that id.
    let first_task = deliver(&state, &task_delivery("task A")).await;
    // The connection stays open on purpose: the session is settled while its
    // process is still reachable, which is the state the live defect left.
    let (_first_io, mut first_assigns) = mount_plugin(&socket, Some(&first_task)).await;
    let assigned = tokio::time::timeout(Duration::from_secs(2), first_assigns.recv())
        .await
        .expect("the mounted session is handed its payload")
        .expect("the plugin connection is still open");
    assert_eq!(assigned, first_task);

    // It completes; no reuse means the slot goes with the settled task.
    on_plugin_report(
        &state,
        Report::Complete {
            task_id: first_task.clone(),
            outcome: Outcome::Done,
            head: Some("A done".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        state.session_count(),
        0,
        "without reuse a settled session leaves the live map"
    );
    assert_settled(&store, &first_task);
    assert!(
        !session_alive(&state, &first_task),
        "the client stops routing to a settled session"
    );

    // Task B: a second session is spawned, and its assignment rides its own
    // connection rather than the finished one.
    let second_task = deliver(&state, &task_delivery("task B")).await;
    assert_ne!(second_task, first_task);
    assert_eq!(
        backend.sessions().len(),
        2,
        "the second task spawns a second resource"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), first_assigns.recv())
            .await
            .is_err(),
        "the finished session's connection is never handed another task"
    );
    let (_second_io, mut second_assigns) = mount_plugin(&socket, Some(&second_task)).await;
    let assigned = tokio::time::timeout(Duration::from_secs(2), second_assigns.recv())
        .await
        .expect("the second session is handed its payload")
        .expect("the second plugin connection is still open");
    assert_eq!(
        assigned, second_task,
        "the second assign names the second task"
    );

    // The client is not reaped by a session ending: its socket still answers.
    let admin = HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: "onlyne-client-cli:test".into(),
        version: "1.0.0".into(),
        kind: MountKind::Admin,
        capabilities: Vec::new(),
        mount: Some(Mount::Admin),
    };
    let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let io = AdapterIo::new(stream, Duration::from_secs(2), Duration::from_secs(2));
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Hello(admin)))
        .await
        .expect("the client socket answers after a session settled");
    assert!(
        body.ok,
        "the client keeps serving for the next task: {body:?}"
    );

    let syncs = outbox
        .kinds()
        .await
        .iter()
        .filter(|kind| **kind == "session_sync")
        .count();
    assert!(
        syncs >= 2,
        "both sessions published a projection over the live link: {syncs}"
    );
    host.abort();
}

/// `reuse` hands the next task to an idle session, and an idle session is only
/// usable while its agent is attached. A plugin that detached — the shape a
/// session process that exited itself leaves behind, and the shape an operator
/// `/onlyne disconnect` leaves — takes its slot out of the map, so the next
/// task spawns a new session instead of writing into a dead connection.
#[tokio::test]
async fn an_idle_session_whose_plugin_left_is_not_reused() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        2,
        true,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let first_task = deliver(&state, &task_delivery("task A")).await;
    let (first_io, mut first_assigns) = mount_plugin(&socket, Some(&first_task)).await;
    tokio::time::timeout(Duration::from_secs(2), first_assigns.recv())
        .await
        .expect("the mounted session is handed its payload");

    on_plugin_report(
        &state,
        Report::Complete {
            task_id: first_task.clone(),
            outcome: Outcome::Done,
            head: Some("A done".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .unwrap();
    assert_settled(&store, &first_task);
    assert_eq!(
        state.session_count(),
        1,
        "reuse keeps the idle slot for the role's next task"
    );

    // The plugin leaves: the connection it served on is over.
    first_io
        .notify(AdapterMsg::Plugin(PluginOp::Detach(DetachArgs {
            reason: "plugin left".into(),
        })))
        .await
        .unwrap();
    eventually(|| state.session_count() == 0, "the idle slot to go").await;

    let second_task = deliver(&state, &task_delivery("task B")).await;
    assert_eq!(
        backend.sessions().len(),
        2,
        "a session whose plugin left cannot carry the next task"
    );
    let _second_io = mount_plugin(&socket, Some(&second_task)).await;
    host.abort();
}
