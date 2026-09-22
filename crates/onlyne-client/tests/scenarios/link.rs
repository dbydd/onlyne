//! The client's server link: the pinned TLS handshake, the welcome it caches, and a
//! report queued while the link is down.

use crate::common::spawn_ready;
use onlyne_client::{
    ClientInit,
    ops::local_cli::LocalCli,
    runtime::intent::{IntentMachine, op_for_intent},
    session::dispatch::{ClientLink, DispatchState},
};
use onlyne_frame::{read_frame, write_frame};
use onlyne_net::{
    KeyPair, TcpListen, TlsConn, accept as accept_handshake, gen_self_signed, server_config,
    table_from,
};
use onlyne_proto::{ClientOp, Frame, PROTOCOL_VERSION, QueryRolesArgs, Report, Welcome};
use onlyne_session::SessionLedger;
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::tempdir;

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
        prose: "cluster b exposes planner".into(),
        spec_hash: "hash-spec-b".into(),
        aggregate: Some("cluster-b".into()),
        allowed_targets: vec![],
        allowed_senders: vec![],
        session_command: None,
        timeout_ready_ms: None,
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
    )
    // CI (and `env -u` local runs) have no herdr/orca/zellij; `fake` is
    // opt-in and is the session backend this test needs. Product exit 5
    // when auto-detect finds nothing stays intact.
    .with_backend("fake");
    let client = tokio::spawn(onlyne_client::run(init));

    let mut cached = None;
    for _ in 0..200 {
        if let Some(prose) = store.prose("planner").unwrap() {
            cached = Some(prose);
            break;
        }
        if client.is_finished() {
            let outcome = client.await;
            panic!("client exited before welcome prose cached: {outcome:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if cached.is_none() {
        if client.is_finished() {
            panic!(
                "client exited before welcome prose cached: {:?}",
                client.await
            );
        }
        panic!("welcome prose reached the cache");
    }
    let (prose, hash) = cached.unwrap();
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
    let refusal = tokio::time::timeout(
        Duration::from_secs(3),
        ClientLink::connect(&bad, Vec::new()),
    )
    .await
    .expect("the wrong-pin dial must answer inside 3s");
    assert!(
        matches!(refusal, Err(onlyne_net::NetError::PinMismatch { .. })),
        "a wrong pin is refused as a pin mismatch"
    );

    client.abort();
    server.abort();
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
        backend,
        store.clone(),
    );

    let (task_id, _assigns) = spawn_ready(&state, "task 1").await;

    // The frame waits in the intent table, which is all a failed send says. The
    // accept gate belongs to the connection and the runloop is its only author
    // (`watch_readiness`): a send that gave up must not park intake, because the
    // pull loop reads this flag and a latch here is a role that stops draining its
    // inbox for the life of the link (`scenarios::restart`, the gate case).
    assert!(
        state.accept_new().load(std::sync::atomic::Ordering::SeqCst),
        "a frame that could not be sent leaves intake where the link left it"
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
