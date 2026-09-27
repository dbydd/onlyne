//! Admin and gateway socket for a server root (plan §7 line 293).
//!
//! v2 moves the endpoint out of the tree: [`bind_socket_v2`] binds
//! `<runtime_dir>/<digest>.sock`, where the digest covers the canonical server
//! root, so there is no `run/s` spelling and no `sun_path` length rule. The
//! registration published beside it, `<runtime_dir>/<digest>.json`, is what
//! every reader resolves, and it is written by the serving side because only
//! the serving side knows the surface is an admin one.
//!
//! One listener serves both surfaces: a connection that opens with an adapter
//! `hello` is a gateway process, and a connection that opens with a request
//! frame speaks the admin vocabulary. The socket is bound `0600`.
//!
//! A bare `ping` frame is the operator's liveness probe and is answered with a
//! `pong`; everything else the admin surface sees is a request frame.

use crate::ServerInit;
use crate::router::{self, Session};
use crate::state::State;
use anyhow::Context;
use onlyne_proto::{AdminOp, ClientOp, ErrorCode, Frame, GatewayOp, ResBody};
use onlyne_wire::socket::prelude::TokioListener;
use onlyne_wire::socket::{
    LocalListener, LocalStream, RegistrationFile, bind_socket_v2, registration_path,
    remove_registration, socket_path, write_registration,
};
use onlyne_wire::{read_frame, write_frame};
use serde_json::Value;
use std::sync::Arc;

/// Serve a server started from CLI arguments.
pub async fn run(init: ServerInit) -> anyhow::Result<()> {
    let state = crate::Server::open(&init)?;
    crate::serve(state).await
}

/// Bind the run socket and publish the registration that names it.
///
/// [`bind_socket_v2`] creates the runtime directory `0700`, drops a stale name
/// at `<runtime_dir>/<digest>.sock`, and binds it; privacy is applied inside the
/// bind: unix `mode(0o600)` on the bind options (fchmod before bind, no umask
/// TOCTOU) and windows owner-only SDDL. The registration is written after the
/// bind, so a reader never sees an endpoint that is not yet served, and it
/// carries this process's pid, which is what `onlyne-server start` and `stop`
/// read in place of the old `run/server.pid`.
///
/// A refused registration fails the whole bind: a socket no reader can find is
/// not the outcome the serving side asked for.
pub fn bind(state: &State) -> anyhow::Result<LocalListener> {
    let root = state.root.as_path();
    let socket = socket_path(root).context("resolve the admin socket path")?;
    let listener = bind_socket_v2(root)
        .with_context(|| format!("bind the admin socket {}", socket.display()))?;
    let registration = registration_path(root);
    write_registration(root, &RegistrationFile::server(root)).with_context(|| {
        format!(
            "publish the admin registration {}",
            registration.display()
        )
    })?;
    tracing::info!(
        socket = %socket.display(),
        registration = %registration.display(),
        "the run socket is open"
    );
    Ok(listener)
}

/// Remove the run socket and registration this server published.
///
/// Every exit route calls this before the process leaves, so a client that
/// retries the path after a shutdown finds it absent and reports the plan's
/// absent-path answer rather than a connection refusal (plan line 344). The
/// registration goes with the socket: a file naming a process that is exiting
/// is the one fact `onlyne-server start` uses to refuse a second daemon.
pub fn unlink(state: &State) -> anyhow::Result<()> {
    let root = state.root.as_path();
    let path = socket_path(root).context("resolve the admin socket path")?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(anyhow::Error::new(error)
                .context(format!("remove the run socket {}", path.display())))
        }
    }?;
    remove_registration(root)
        .with_context(|| format!("remove the admin registration for {}", root.display()))
}

/// Accept connections on the run socket until it fails.
pub async fn serve_socket(state: Arc<State>, listener: LocalListener) -> anyhow::Result<()> {
    loop {
        let stream = listener.accept().await?;
        let connection_state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(connection_state, stream).await {
                tracing::debug!(error = %error, "admin connection ended");
            }
        });
    }
}

/// Route one accepted connection by the first frame it sends.
///
/// A gateway process opens with an adapter `hello`; the CLI opens with a request
/// frame. The adapter `hello` is read here so `WireMessage` decides the surface.
pub async fn handle(state: Arc<State>, mut stream: LocalStream) -> anyhow::Result<()> {
    let Some(first) = read_frame::<_, Value>(&mut stream).await? else {
        return Ok(());
    };
    if first.get("f").is_none() {
        let wire: onlyne_adapter::WireMessage =
            serde_json::from_value(first).context("decode an adapter frame")?;
        return crate::gateway_host::serve_adapter(state, stream, wire).await;
    }
    frame_loop(state, stream, Some(first)).await
}

/// Serve request frames of the admin and gateway vocabularies.
pub async fn frame_loop(
    state: Arc<State>,
    mut stream: LocalStream,
    first: Option<Value>,
) -> anyhow::Result<()> {
    let mut pending = first;
    let mut session = Session::default();
    loop {
        let value: Value = match pending.take() {
            Some(value) => value,
            None => match read_frame(&mut stream).await? {
                Some(value) => value,
                None => return Ok(()),
            },
        };
        if let Some(pong) = pong_for(&value, &state) {
            write_frame(&mut stream, &pong).await?;
            continue;
        }
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let body = route_value(&state, &mut session, &value).await;
        write_frame(&mut stream, &Frame::<ClientOp>::res(id, body)).await?;
    }
}

/// Decode one request frame and dispatch it in whichever vocabulary it uses.
pub async fn route_value(state: &Arc<State>, session: &mut Session, value: &Value) -> ResBody {
    if value.get("f").and_then(Value::as_str) != Some("req") {
        return ResBody::err(
            ErrorCode::BadFrame,
            "the socket accepts request frames only",
            Some("f".to_string()),
        );
    }
    if let Ok(Frame::Req { op, .. }) = serde_json::from_value::<Frame<AdminOp>>(value.clone()) {
        return router::dispatch_admin(state, session, op).await;
    }
    if let Ok(Frame::Req { op, .. }) = serde_json::from_value::<Frame<GatewayOp>>(value.clone()) {
        return router::dispatch_gateway(state, session, op).await;
    }
    match value.get("op").and_then(Value::as_str) {
        Some(op) => ResBody::err(
            ErrorCode::UnknownOp,
            format!("unknown op {op}"),
            Some("op".to_string()),
        ),
        None => ResBody::err(
            ErrorCode::BadFrame,
            "the frame carries no op",
            Some("op".to_string()),
        ),
    }
}

/// The answer to a liveness probe, or `None` for anything that is not one.
///
/// The operator CLI opens the socket and sends a bare
/// `{"f":"ping","t":<unix millis>}` frame, so the heartbeat is answered as a
/// frame rather than as an op. `t` is echoed verbatim and `server_seq` is the
/// in-memory event cursor: the answer costs no ledger row, no event, and no
/// query. A frame that names `ping` but carries no integer `t` is not a probe;
/// it falls through to [`route_value`], which answers `bad_frame`.
fn pong_for(value: &Value, state: &State) -> Option<Frame<ClientOp>> {
    if value.get("f").and_then(Value::as_str) != Some("ping") {
        return None;
    }
    match serde_json::from_value::<Frame<ClientOp>>(value.clone()) {
        Ok(Frame::Ping { t }) => Some(Frame::Pong {
            t,
            server_seq: state.event_cursor(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Server, ServerInit};
    use onlyne_proto::{Event, LedgerQuery, Presence, RolePresence};
    use serde_json::json;
    use tempfile::TempDir;

    const CERT_PIN: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn spec_text() -> String {
        format!(
            r#"[server]
name = "admin-ping"
listen = "127.0.0.1:0"
cert_pin = "{CERT_PIN}"
note_queue = false
heartbeat_timeout_ms = 30000

[[client]]
role = "planner"
key = "{key}"
allowed_senders = ["planner"]
allowed_targets = ["planner"]
"#,
            key = onlyne_net::KeyPair::from_seed([7_u8; 32]).public_str()
        )
    }

    /// A scratch server root, bound nowhere: the test binds the run socket.
    fn scratch() -> (TempDir, Arc<State>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("server");
        std::fs::create_dir_all(root.join(".onlyne")).expect("create the root");
        std::fs::write(root.join(".onlyne/spec.toml"), spec_text()).expect("write the spec");
        let state = Server::open(&ServerInit { root, listen: None }).expect("open the server");
        (dir, state)
    }

    /// Ledger rows on disk: the probe must leave this count untouched.
    fn ledger_rows(state: &State) -> usize {
        state
            .ledger
            .ledger_query(LedgerQuery {
                limit: 10,
                ..LedgerQuery::default()
            })
            .expect("query the ledger")
            .len()
    }

    /// The journey `onlyne ping --server-root <root>` makes: open the run
    /// socket, write one heartbeat, read the answer. The CLI takes the frame
    /// asserted here as its success branch (`crates/onlyne-cli/src/verbs.rs`),
    /// where it renders `{"ok":true,"data":{"t":…,"server_seq":…}}` and exits 0.
    #[tokio::test]
    async fn a_ping_over_the_run_socket_is_answered_with_a_pong_from_memory() {
        let (_dir, state) = scratch();
        // One real event, so the answer's cursor is a value this server holds
        // rather than the zero of a fresh ledger.
        state
            .emit(Event::RolePresence(RolePresence {
                role: "planner".to_string(),
                state: Presence::Online,
                aggregate: None,
                sessions: 0,
                detail: None,
            }))
            .expect("emit one event");
        let head = state.event_cursor();
        assert!(head > 0, "an emitted event moves the cursor");

        let listener = bind(&state).expect("bind the run socket");
        let path = onlyne_wire::socket::socket_path(&state.root).expect("the socket path");
        let serving = state.clone();
        let socket_task = tokio::spawn(async move {
            let _ = serve_socket(serving, listener).await;
        });

        let mut stream = onlyne_wire::socket::connect_local(&path)
            .await
            .expect("connect");
        let probe: Frame<ClientOp> = Frame::Ping { t: 1_699_600_000 };
        write_frame(&mut stream, &probe)
            .await
            .expect("write the ping");
        let answer: Frame<ClientOp> = read_frame(&mut stream)
            .await
            .expect("read the answer")
            .expect("one answer frame");
        assert_eq!(
            serde_json::to_value(&answer).expect("encode the answer"),
            json!({"f": "pong", "t": 1_699_600_000, "server_seq": head})
        );

        // Zero policy: the probe appended no event and no ledger row.
        assert_eq!(
            state.event_cursor(),
            head,
            "the probe moved the event cursor"
        );
        assert_eq!(state.ledger.event_head().expect("head") as u64, head);
        assert_eq!(ledger_rows(&state), 0, "the probe left a ledger row");

        socket_task.abort();
    }

    /// What v2 publishes at bind: the socket under the runtime directory, named
    /// by a registration that states the surface, the tree, and this process.
    #[cfg(unix)]
    #[tokio::test]
    async fn binding_publishes_a_server_registration_in_the_runtime_directory() {
        use onlyne_wire::socket::{RegistrationKind, read_registration, registration_path, runtime_dir, workspace_digest};

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("server");
        std::fs::create_dir_all(root.join(".onlyne")).expect("create the root");
        std::fs::write(root.join(".onlyne/spec.toml"), spec_text()).expect("write the spec");
        let state = Server::open(&ServerInit { root, listen: None }).expect("open the server");

        let _listener = bind(&state).expect("bind the run socket");
        let runtime = runtime_dir().expect("the runtime directory");
        let digest = workspace_digest(&state.root);
        assert_eq!(registration_path(&state.root), runtime.join(format!("{digest}.json")));
        assert!(runtime.join(format!("{digest}.sock")).exists(), "the socket is under the runtime directory");

        let registration = read_registration(&state.root)
            .expect("read the registration")
            .expect("a bound socket publishes one");
        assert_eq!(registration.kind, RegistrationKind::Server);
        assert_eq!(registration.role, None, "a server root serves no role");
        assert_eq!(registration.root, onlyne_wire::socket::absolute_path(&state.root));
        assert_eq!(registration.pid, std::process::id());
        assert!(!registration.version.is_empty());

        // The tree keeps no socket: nothing is bound under `.onlyne/run`.
        assert!(!state.root.join(".onlyne/run/s").exists());
    }

    /// A deep root binds the same short runtime path as a shallow one, because
    /// the digest covers the canonical root and the length of the tree is not
    /// part of the name. v1 needed a second derived path here.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_root_past_the_path_bound_serves_the_same_runtime_path() {
        use onlyne_wire::socket::{UNIX_SOCKET_PATH_MAX, socket_path};

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("server-root".repeat(8)).join("deep");
        std::fs::create_dir_all(root.join(".onlyne")).expect("create the root");
        std::fs::write(root.join(".onlyne/spec.toml"), spec_text()).expect("write the spec");
        let state = Server::open(&ServerInit { root, listen: None }).expect("open the server");

        let natural = state.root.join(".onlyne/run/s");
        assert!(
            natural.as_os_str().len() > UNIX_SOCKET_PATH_MAX,
            "{} is {} bytes",
            natural.display(),
            natural.as_os_str().len()
        );

        let listener = bind(&state).expect("bind on a root past the bound");
        let served = socket_path(&state.root).expect("the served path");
        assert!(
            served.as_os_str().len() <= UNIX_SOCKET_PATH_MAX,
            "{} is {} bytes",
            served.display(),
            served.as_os_str().len()
        );

        let serving = state.clone();
        let socket_task = tokio::spawn(async move {
            let _ = serve_socket(serving, listener).await;
        });
        let mut stream = onlyne_wire::socket::connect_local(&served)
            .await
            .expect("connect the served path");
        let probe: Frame<ClientOp> = Frame::Ping { t: 1_700_000_000 };
        write_frame(&mut stream, &probe)
            .await
            .expect("write the ping");
        let answer: Frame<ClientOp> = read_frame(&mut stream)
            .await
            .expect("read the answer")
            .expect("one answer frame");
        assert!(
            matches!(
                answer,
                Frame::Pong {
                    t: 1_700_000_000,
                    ..
                }
            ),
            "the served path answers {answer:?}"
        );
        drop(stream);
        socket_task.abort();

        unlink(&state).expect("unlink the served path");
        assert!(!served.exists(), "the served path survived unlink");
        assert!(
            onlyne_wire::socket::read_registration(&state.root)
                .expect("read the registration")
                .is_none(),
            "the registration survived unlink"
        );
    }

    /// Windows binds no `sun_path`: the runtime path is a regular marker file
    /// naming the NPFS pipe the daemon holds, so the same
    /// `<runtime_dir>/<digest>.sock` every reader resolves carries the pipe name
    /// and the registration beside it names the surface. A root past the unix
    /// bound still serves that one path end to end.
    #[cfg(not(unix))]
    #[tokio::test]
    async fn a_root_past_the_path_bound_serves_the_runtime_marker() {
        use onlyne_wire::socket::{UNIX_SOCKET_PATH_MAX, socket_path};

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("server-root".repeat(8)).join("deep");
        std::fs::create_dir_all(root.join(".onlyne")).expect("create the root");
        std::fs::write(root.join(".onlyne/spec.toml"), spec_text()).expect("write the spec");
        let state = Server::open(&ServerInit { root, listen: None }).expect("open the server");

        let natural = state.root.join(".onlyne/run/s");
        assert!(
            natural.as_os_str().len() > UNIX_SOCKET_PATH_MAX,
            "{} is {} bytes",
            natural.display(),
            natural.as_os_str().len()
        );

        let listener = bind(&state).expect("bind on a root past the bound");
        let served = socket_path(&state.root).expect("the served path");
        let marker = std::fs::read_to_string(&served).expect("read the served path");
        assert!(
            marker.starts_with("v1:"),
            "the served path names the pipe, got {marker:?}"
        );
        assert!(
            onlyne_wire::socket::read_registration(&state.root)
                .expect("read the registration")
                .is_some(),
            "a bound socket publishes a registration"
        );

        let serving = state.clone();
        let socket_task = tokio::spawn(async move {
            let _ = serve_socket(serving, listener).await;
        });
        let mut stream = onlyne_wire::socket::connect_local(&served)
            .await
            .expect("connect the served path");
        let probe: Frame<ClientOp> = Frame::Ping { t: 1_700_000_000 };
        write_frame(&mut stream, &probe)
            .await
            .expect("write the ping");
        let answer: Frame<ClientOp> = read_frame(&mut stream)
            .await
            .expect("read the answer")
            .expect("one answer frame");
        assert!(
            matches!(
                answer,
                Frame::Pong {
                    t: 1_700_000_000,
                    ..
                }
            ),
            "the served path answers {answer:?}"
        );
        drop(stream);
        socket_task.abort();

        unlink(&state).expect("unlink the served path");
        assert!(!served.exists(), "the served path survived unlink");
    }
}
