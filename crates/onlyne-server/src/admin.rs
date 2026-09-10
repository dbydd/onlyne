//! Admin and gateway socket at `<server-root>/.onlyne/run/s` (plan §7 line 293).
//!
//! One listener serves both surfaces: a connection that opens with an adapter
//! `hello` is a gateway process, and a connection that opens with a request
//! frame speaks the admin vocabulary. The socket is bound `0600`.

use crate::router::{self, Session};
use crate::state::State;
use crate::ServerInit;
use anyhow::Context;
use onlyne_frame::{read_frame, write_frame};
use onlyne_layout::{ServerRoot, apply_private_mode};
use onlyne_proto::{AdminOp, ClientOp, ErrorCode, Frame, GatewayOp, ResBody};
use serde_json::Value;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};

/// Serve a server started from CLI arguments.
pub async fn run(init: ServerInit) -> anyhow::Result<()> {
    let state = crate::Server::open(&init)?;
    crate::serve(state).await
}

/// Bind the run socket and apply `0600`.
pub fn bind(state: &State) -> anyhow::Result<UnixListener> {
    let layout = ServerRoot::resolve(&state.root);
    let path = layout.socket_path();
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("remove the stale socket {}", path.display()))?;
    }
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bind the admin socket {}", path.display()))?;
    apply_private_mode(&path)
        .with_context(|| format!("apply 0600 to {}", path.display()))?;
    Ok(listener)
}

/// Accept connections on the run socket until it fails.
pub async fn serve_socket(state: Arc<State>, listener: UnixListener) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
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
pub async fn handle(state: Arc<State>, mut stream: UnixStream) -> anyhow::Result<()> {
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
    mut stream: UnixStream,
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
