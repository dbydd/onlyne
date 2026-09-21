//! The role socket: admission of an admin `hello`, the `ping` probe, and the `status`
//! link probe.

use crate::common::serve_role_socket;
use onlyne_adapter::AdapterIo;
use onlyne_client::session::{adapter_socket::AdapterSocket, dispatch::DispatchState};
use onlyne_frame::{read_frame, write_frame};
use onlyne_proto::{
    AdapterMsg, ClientOp, ErrorCode, Frame, HelloArgs, HostOp, Mount, MountKind, PROTOCOL_VERSION,
    PluginOp, QueryRolesArgs,
};
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

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

    let stream = onlyne_layout::connect_local(&socket).await.unwrap();
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
    let stream = onlyne_layout::connect_local(&socket).await.unwrap();
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
        backend,
        store,
    );
    let (socket, host) = serve_role_socket(&state, dir.path()).await;
    let mut stream = onlyne_layout::connect_local(&socket).await.unwrap();

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
/// holds it. A socket that answers carries the link state; a socket nobody
/// answers is not a running client at all.
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
        backend,
        store,
    );
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    assert_eq!(
        onlyne_client::session::adapter_socket::server_link_state(&socket).await,
        Some(false),
        "a client that holds no link answers without one"
    );
    state.set_link_up(true);
    assert_eq!(
        onlyne_client::session::adapter_socket::server_link_state(&socket).await,
        Some(true),
        "the probe reads the connection the runtime holds"
    );
    state.set_link_up(false);
    assert_eq!(
        onlyne_client::session::adapter_socket::server_link_state(&socket).await,
        Some(false),
        "a dropped link is reported as not connected"
    );
    host.abort();

    let absent = dir.path().join(".onlyne/run/absent");
    assert_eq!(
        onlyne_client::session::adapter_socket::server_link_state(&absent).await,
        None,
        "nothing answers a path that holds no socket"
    );
}
