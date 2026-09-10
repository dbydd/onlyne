//! Onlyne v1 server: routing, ledger, delivery, gateway host, admin socket.
//!
//! [`serve`] starts the three listeners and returns when the server is asked to
//! shut down: the role TCP plus TLS listener, and the run socket that serves the
//! gateway and admin surfaces (plan §7 line 293).

pub mod admin;
pub mod cli;
pub mod events;
pub mod faults;
pub mod gateway_host;
pub mod generate;
pub mod projection;
pub mod relay;
pub mod router;
pub mod state;

pub use state::{
    AdapterLink, ChannelBinding, DeliveryTicket, GatewayConnection, GatewayRegistry,
    ListenerHandles, RoleConnection, RoleRegistry, Server, ServerInit, State, acl_from_spec,
};
pub use generate::{GenerateArgs, GenerateError, GenerateReport, GeneratedRole, generate};
use anyhow::Context;
use onlyne_layout::ServerRoot;
use onlyne_net::{TcpListen, TlsConn, handshake, tls};
use onlyne_proto::{ClientOp, Frame, PROTOCOL_VERSION};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Run a server from CLI arguments.
pub async fn run(init: ServerInit) -> anyhow::Result<()> {
    let state = Server::open(&init)?;
    serve(state).await
}

/// Start every listener and return once the runtime is asked to stop.
pub async fn serve(state: Arc<crate::state::State>) -> anyhow::Result<()> {
    let layout = ServerRoot::resolve(&state.root);
    let spec = state.spec_snapshot().context("the spec is unavailable")?;
    let cert = tls::load_or_create(&layout.key_path(), &spec.server.name)?;
    let tls_config = tls::server_config(&cert)?;
    let listener = TcpListen::bind(&spec.server.listen).await?;
    let run_socket = admin::bind(&state)?;
    let socket_state = state.clone();
    let socket_task = tokio::spawn(async move {
        if let Err(error) = admin::serve_socket(socket_state, run_socket).await {
            tracing::warn!(error = %error, "the run socket stopped");
        }
    });
    let role_state = state.clone();
    let role_task = tokio::spawn(async move {
        if let Err(error) = role_listener(role_state, listener, tls_config).await {
            tracing::warn!(error = %error, "the role listener stopped");
        }
    });
    let signal_task = spawn_reload_signal(state.clone());
    state.await_shutdown().await;
    socket_task.abort();
    role_task.abort();
    if let Some(task) = signal_task {
        task.abort();
    }
    Ok(())
}

/// Serve SIGHUP as the second reload trigger (plan §5 line 270).
///
/// The signal runs the same body as `AdminOp::Reload` and logs the diff text.
pub fn spawn_reload_signal(
    state: Arc<crate::state::State>,
) -> Option<tokio::task::JoinHandle<()>> {
    let mut hangup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
        Ok(signal) => signal,
        Err(error) => {
            tracing::warn!(error = %error, "the SIGHUP listener is unavailable");
            return None;
        }
    };
    Some(tokio::spawn(async move {
        while hangup.recv().await.is_some() {
            match router::reload_spec(&state) {
                Ok(outcome) => tracing::info!(
                    spec_hash = %outcome.spec_hash,
                    diff = %outcome.render,
                    "spec reloaded on SIGHUP"
                ),
                Err(message) => {
                    tracing::warn!(error = %message, "spec reload on SIGHUP failed");
                }
            }
        }
    }))
}

/// Accept role connections, handshake each one, then serve its frames.
pub async fn role_listener(
    state: Arc<crate::state::State>,
    mut listener: TcpListen,
    tls_config: rustls::ServerConfig,
) -> anyhow::Result<()> {
    loop {
        let mut connection = listener.accept_next(&tls_config).await?;
        let acl = state.acl_table();
        match handshake::accept(&mut connection, &acl, PROTOCOL_VERSION).await {
            Ok(ok) => {
                let connection_state = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = role_connection(connection_state, connection, &ok.role).await
                    {
                        tracing::debug!(error = %error, "role connection ended");
                    }
                });
            }
            Err(error) => {
                let _ = faults::record(&state, faults::FaultDraft::hello_timeout(&error.to_string()));
            }
        }
    }
}

/// Serve one authenticated role connection until it closes.
pub async fn role_connection(
    state: Arc<crate::state::State>,
    mut connection: TlsConn,
    role: &str,
) -> anyhow::Result<()> {
    let (sender, mut outbound) = mpsc::channel::<Frame>(256);
    let mut session = router::Session::with_sender(sender);
    loop {
        tokio::select! {
            outgoing = outbound.recv() => {
                let Some(frame) = outgoing else {
                    break;
                };
                connection.send_frame(&frame).await?;
            }
            incoming = connection.recv_frame::<Frame<ClientOp>>() => {
                match incoming {
                    Ok(Some(Frame::Req { id, op })) => {
                        let body = router::dispatch_client(&state, &mut session, op).await;
                        connection
                            .send_frame(&Frame::<ClientOp>::res(id, body))
                            .await?;
                    }
                    Ok(Some(Frame::Ping { t })) => {
                        let pong: Frame = Frame::Pong {
                            t,
                            server_seq: state.event_head().max(0) as u64,
                        };
                        connection.send_frame(&pong).await?;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(error) => {
                        tracing::debug!(error = %error, "role connection read failed");
                        break;
                    }
                }
            }
        }
    }
    if let Some(bound) = session.role.clone() {
        let _ = relay::disconnect(&state, &bound);
    }
    let _ = role;
    Ok(())
}

/// The crate version, reported by `status`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The server entrypoint used by the `onlyne-server` binary.
pub async fn entrypoint() -> i32 {
    cli::entrypoint().await
}
