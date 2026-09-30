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
use onlyne_wire::{FrameReader, read_frame, write_frame};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Frames a subscription may queue for one connection before the forwarder
/// waits for the writer — the depth the role connection gives a client.
const OUTBOUND_DEPTH: usize = 256;

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
/// carries this process's pid, which is what `status` reads in place of the old
/// `run/server.pid`.
///
/// A refused registration fails the whole bind: a socket no reader can find is
/// not the outcome the serving side asked for.
pub fn bind(state: &State) -> anyhow::Result<LocalListener> {
    let root = state.root.as_path();
    let socket = socket_path(root).context("resolve the admin socket path")?;
    let listener = bind_socket_v2(root)
        .with_context(|| format!("bind the admin socket {}", socket.display()))?;
    let registration = registration_path(root);
    write_registration(root, &RegistrationFile::server(root))
        .with_context(|| format!("publish the admin registration {}", registration.display()))?;
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
/// registration goes with the socket: a file naming a process that has exited
/// would misreport the tree as served.
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
///
/// The loop reads a frame or writes one a subscription is owed, so a connection
/// that carries a stream still answers the requests that arrive on it. The
/// reader keeps the bytes of a partial frame across a branch `select!` does not
/// take, which is what makes that choice safe for the next frame.
pub async fn frame_loop(
    state: Arc<State>,
    mut stream: LocalStream,
    first: Option<Value>,
) -> anyhow::Result<()> {
    let mut pending = first;
    let (sender, mut outbound) = mpsc::channel::<Frame>(OUTBOUND_DEPTH);
    let mut session = Session {
        sender: Some(sender),
        ..Session::default()
    };
    let mut reader = FrameReader::new();
    loop {
        let value: Value = if let Some(value) = pending.take() {
            value
        } else {
            // Nothing is queued: wait for the peer, or for a frame the stream is
            // owed. A closed outbound channel is this process dropping the
            // session, so the connection is done either way.
            loop {
                tokio::select! {
                    incoming = reader.next::<_, Value>(&mut stream) => match incoming? {
                        Some(value) => break value,
                        None => return Ok(()),
                    },
                    outgoing = outbound.recv() => match outgoing {
                        Some(frame) => write_frame(&mut stream, &frame).await?,
                        None => return Ok(()),
                    },
                }
            }
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
