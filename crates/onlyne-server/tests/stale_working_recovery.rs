use chrono::{DateTime, Utc};
use onlyne_frame::{read_frame, write_frame};
use onlyne_net::{
    KeyPair, TcpListen, TlsConn, accept as accept_handshake, gen_self_signed, server_config,
    table_from,
};
use onlyne_proto::{
    Body, ClientOp, Frame, HandshakeArgs, LedgerEntry, LedgerState, MsgKind, Outcome,
    PROTOCOL_VERSION, Principal, PullReply, Report, ResBody, RoleInfo, Subscribe, Welcome,
};
use onlyne_server::state::{Server, ServerInit};
use onlyne_server::{projection, stale};
use serde_json::json;
use std::time::Duration;
use tempfile::tempdir;

const CERT_PIN: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn stale_time() -> DateTime<Utc> {
    Utc::now() - chrono::Duration::seconds(10)
}

fn stale_ledger_entry(task_id: &str) -> LedgerEntry {
    let at = stale_time();
    LedgerEntry {
        msg_id: "msg-stale".into(),
        op_id: Some("op-stale".into()),
        kind: MsgKind::Task,
        from: Principal::role("supervisor"),
        to: Principal::role("planner"),
        task: Some(task_id.to_string()),
        parent_task: None,
        hop: 0,
        attempt: 0,
        state: LedgerState::Acked,
        out_head: None,
        body_json: Some(serde_json::to_string(&Body::text("stale work")).unwrap()),
        enqueued_at: at,
        acked_at: Some(at),
    }
}

fn welcome() -> Welcome {
    Welcome {
        cluster: "cluster-a".into(),
        server: "srv".into(),
        role: "planner".into(),
        admin: false,
        max_sessions: 1,
        reuse: false,
        prose: "planner prose".into(),
        spec_hash: "hash".into(),
        aggregate: None,
        allowed_targets: vec![],
        allowed_senders: vec![],
        session_command: None,
        timeout_ready_ms: None,
        timeout_running_ms: None,
        timeout_idle_ms: None,
        intent_attempts: Some(3),
        intent_backoff_ms: Some(vec![10, 20]),
        relay_required: None,
        relay_count: None,
        seq: 0,
    }
}

fn role_info() -> RoleInfo {
    RoleInfo {
        name: "planner".into(),
        admin: false,
        max_sessions: 1,
        reuse: false,
        session_command: Vec::new(),
        spec_hash: "hash".into(),
        prose: Some("planner prose".into()),
        state: onlyne_proto::Presence::Online,
        sessions: 0,
        detail: None,
        edges: Vec::new(),
        aggregate: None,
        relay_required: None,
        relay_count: None,
    }
}

#[tokio::test]
async fn restarted_client_reports_session_dead_for_stale_acked_work() {
    unsafe { std::env::set_var("ONLYNE_BACKEND", "fake") };
    let dir = tempdir().unwrap();
    let keypair = KeyPair::generate();
    let key_path = dir.path().join(".onlyne/keys/role.key");
    std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
    keypair.save(&key_path).unwrap();

    let certificate = gen_self_signed("127.0.0.1", 1).unwrap();
    let config = server_config(&certificate).unwrap();
    let mut listener = TcpListen::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let acl = table_from([(
        "planner".to_string(),
        keypair.public_str(),
        false,
        Vec::new(),
        Vec::new(),
    )])
    .unwrap();
    let task_id = onlyne_proto::new_task_id();
    let row = stale_ledger_entry(&task_id);
    let (reports_tx, mut reports_rx) = tokio::sync::mpsc::unbounded_channel::<Report>();

    let server = tokio::spawn(async move {
        loop {
            let Ok(accepted) = listener.accept_next(&config).await else {
                break;
            };
            let acl = acl.clone();
            let row = row.clone();
            let reports_tx = reports_tx.clone();
            tokio::spawn(async move {
                let TlsConn::Server(mut stream) = accepted else {
                    return;
                };
                if accept_handshake(&mut stream, &acl, PROTOCOL_VERSION)
                    .await
                    .is_err()
                {
                    return;
                }
                while let Ok(Some(frame)) = read_frame::<_, Frame<ClientOp>>(&mut stream).await {
                    let Frame::Req { id, op } = frame else {
                        continue;
                    };
                    let body = match op {
                        ClientOp::Hello(HandshakeArgs { .. }) => {
                            ResBody::ok(serde_json::to_value(welcome()).unwrap())
                        }
                        ClientOp::Subscribe(Subscribe { .. }) => {
                            ResBody::ok(json!({"rows": [], "head": 0}))
                        }
                        ClientOp::QueryLedger(_) => ResBody::ok(json!({"ledger": [row]})),
                        ClientOp::QueryRoles(_) => ResBody::ok(json!({"roles": [role_info()]})),
                        ClientOp::Report(report) => {
                            let _ = reports_tx.send(report.clone());
                            ResBody::ok(
                                json!({"applied": true, "kind": report.kind_name(), "task_id": report.task_id()}),
                            )
                        }
                        ClientOp::Pull(_) => ResBody::ok(
                            serde_json::to_value(PullReply {
                                deliveries: Vec::new(),
                                seq: 0,
                            })
                            .unwrap(),
                        ),
                        _ => ResBody::ok(json!({})),
                    };
                    let _ = write_frame(&mut stream, &Frame::<ClientOp>::res(id, body)).await;
                }
            });
        }
    });

    let client = tokio::spawn(onlyne_client::run(
        onlyne_client::ClientInit::new(
            dir.path(),
            "planner",
            endpoint,
            key_path,
            certificate.spki_pin.clone(),
        )
        .with_stale_grace_secs(0),
    ));

    let report = tokio::time::timeout(Duration::from_secs(5), reports_rx.recv())
        .await
        .expect("client reports within the startup reconcile window")
        .expect("report channel open");
    match report {
        Report::Complete {
            task_id: reported,
            outcome,
            head,
            ..
        } => {
            assert_eq!(reported, task_id);
            assert_eq!(outcome, Outcome::Failed);
            assert_eq!(head.as_deref(), Some(onlyne_client::stale::SESSION_DEAD));
        }
        other => panic!("expected session_dead completion, got {other:?}"),
    }
    client.abort();
    server.abort();
}

fn server_spec(key: &str) -> String {
    format!(
        r#"[server]
name = "local"
listen = "127.0.0.1:0"
cert_pin = "{CERT_PIN}"
stale_watch_secs = 60

[[client]]
role = "builder"
key = "{key}"
"#
    )
}

#[test]
fn server_observer_emits_stale_working_without_auto_settling() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("server");
    std::fs::create_dir_all(root.join(".onlyne")).unwrap();
    let key = KeyPair::from_seed([7_u8; 32]).public_str();
    std::fs::write(root.join(".onlyne/spec.toml"), server_spec(&key)).unwrap();
    let state = Server::open(&ServerInit { root, listen: None }).unwrap();
    let task_id = onlyne_proto::new_task_id();
    projection::report(
        &state,
        "builder",
        &Report::Ready {
            task_id: task_id.clone(),
            session_id: "sess-stale".into(),
            generation: 1,
            seq: 1,
            cluster_ref: None,
        },
    )
    .unwrap();
    let old = (Utc::now() - chrono::Duration::seconds((stale::STALE_WATCH_GRACE_SECS + 2) as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let conn = rusqlite::Connection::open(state.ledger.path()).unwrap();
    conn.execute(
        "UPDATE sessions SET updated_at=?1 WHERE task_id=?2",
        rusqlite::params![old, task_id],
    )
    .unwrap();
    drop(conn);

    let events = stale::observe_once(&state, Utc::now()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, stale::KIND_STALE_WORKING);
    let row = state.ledger.get_session_row(&task_id).unwrap().unwrap();
    assert_eq!(row.public_lifecycle, "working");
    let faults = state.ledger.open_faults().unwrap();
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0].kind, stale::KIND_STALE_WORKING);
    assert_eq!(faults[0].task_id.as_deref(), Some(task_id.as_str()));
}
