use crate::dispatch::{DispatchState, ReadyNotice, on_plugin_report, on_ready};
use anyhow::{Context, Result};
use onlyne_adapter::{AdapterIo, AdapterServer, ServerConnection};
use onlyne_layout::local_socket::prelude::TokioListener;
use onlyne_layout::{
    LocalListener, LocalStream, RoleWorkspace, SocketEndpoint, bind_socket, connect_local,
};
use onlyne_proto::{
    AdapterMsg, Capability, ErrorCode, HelloAck, HelloArgs, HostOp, Mount, MountKind,
    PROTOCOL_VERSION, PluginOp, Report, ResBody, ServerInfo,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone)]
pub struct AdapterSocket {
    pub workspace: PathBuf,
    pub role: String,
    pub cluster: String,
    pub server: String,
    pub dispatch: DispatchState,
}

impl AdapterSocket {
    /// The path this role's clients connect to, per
    /// [`RoleWorkspace::socket_path`].
    ///
    /// One accessor answers for the whole tree, so a daemon that bound the short
    /// path and a caller that derived it from the workspace root reach the same
    /// socket through the marker in `<run>/socket`.
    pub fn path(&self) -> PathBuf {
        RoleWorkspace::resolve(&self.workspace).socket_path()
    }

    /// Bind the workspace socket and report the endpoint that was served.
    ///
    /// [`bind_socket`] owns the whole sequence — resolve, create the run
    /// directory, drop a stale name, bind, publish the marker — and its failure
    /// message carries both spellings it tried with each length.
    ///
    /// A short endpoint means the socket moved off the canonical path, which an
    /// operator reading a stale `s` would otherwise chase forever, so the log
    /// line names the canonical path, its byte length, the served path, and the
    /// marker that carries the answer.
    #[allow(clippy::unused_async)]
    pub async fn bind(&self) -> Result<(LocalListener, SocketEndpoint)> {
        let layout = RoleWorkspace::resolve(&self.workspace);
        let (listener, endpoint) =
            bind_socket(layout.root(), &layout.run_dir()).with_context(|| {
                format!(
                    "bind the workspace socket {}",
                    layout.socket_path_natural().display(),
                )
            })?;
        if endpoint.short() {
            let natural = endpoint.natural();
            tracing::warn!(
                canonical = %natural.display(),
                canonical_bytes = natural.as_os_str().len(),
                served = %endpoint.actual().display(),
                marker = %endpoint.marker().display(),
                "adapter socket moved to the short path"
            );
        } else {
            tracing::info!(socket = %endpoint.actual().display(), "adapter socket serving");
        }
        Ok((listener, endpoint))
    }

    /// Answer every connection on `listener` for the life of the process.
    ///
    /// The listener stays open across a failed `accept`: one bad handshake on one
    /// socket is that client's problem, and every plugin already mounted would
    /// lose its transport if the surface came down for it. The pause keeps a
    /// persistent failure — a descriptor ceiling, a name pulled out from under
    /// the listener — from spinning the loop at full speed.
    pub async fn accept_loop(&self, listener: LocalListener) -> Result<()> {
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    let this = self.clone();
                    tokio::spawn(async move {
                        if let Err(err) = this.connection(stream).await {
                            tracing::debug!(error = %err, "adapter connection closed");
                        }
                    });
                }
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        kind = ?error.kind(),
                        "adapter socket accept failed; retrying"
                    );
                    tokio::time::sleep(ACCEPT_RETRY_PAUSE).await;
                }
            }
        }
    }

    /// Bind and serve in one call, for a caller that owns no endpoint interest.
    pub async fn serve(self) -> Result<()> {
        let (listener, _) = self.bind().await?;
        self.accept_loop(listener).await
    }

    /// Serve one accepted connection on whichever surface it opened.
    ///
    /// The workspace socket carries two vocabularies (plan §7 line 293): a
    /// plugin opens with an adapter `hello`, and the local CLI opens with a
    /// request frame. The first frame decides, so one path serves both.
    async fn connection(&self, mut stream: LocalStream) -> Result<()> {
        let first = onlyne_frame::read_frame::<_, serde_json::Value>(&mut stream)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let Some(first) = first else {
            return Ok(());
        };
        if first.get("f").is_some() {
            return self.serve_local(stream, first).await;
        }
        self.serve_plugin(stream, first).await
    }

    /// Serve the local CLI vocabulary on the workspace socket.
    ///
    /// A message verb reports the server's verdict, which is the answer the
    /// operator asked for, and only a link that is down queues the envelope as
    /// a durable intent (plan §6 line 289). A liveness probe is answered in
    /// place: `onlyne ping --workspace <dir>` sends a bare `Frame::Ping` and
    /// reads the pong, so the connection stays open for the exchange the caller
    /// opened it for.
    async fn serve_local(&self, mut stream: LocalStream, first: serde_json::Value) -> Result<()> {
        let mut pending = Some(first);
        loop {
            let value = match pending.take() {
                Some(value) => value,
                None => match onlyne_frame::read_frame::<_, serde_json::Value>(&mut stream).await {
                    Ok(Some(value)) => value,
                    Ok(None) => return Ok(()),
                    Err(error) => return Err(anyhow::anyhow!(error.to_string())),
                },
            };
            let frame: onlyne_proto::Frame<onlyne_proto::ClientOp> =
                serde_json::from_value(value).context("decode a client frame")?;
            if let onlyne_proto::Frame::Ping { t } = frame {
                let pong = onlyne_proto::Frame::<onlyne_proto::ClientOp>::Pong { t, server_seq: 0 };
                onlyne_frame::write_frame(&mut stream, &pong)
                    .await
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                continue;
            }
            let onlyne_proto::Frame::Req { id, op } = frame else {
                return Ok(());
            };
            let body = self.local_op(op).await;
            let reply = onlyne_proto::Frame::<onlyne_proto::ClientOp>::res(id, body);
            onlyne_frame::write_frame(&mut stream, &reply)
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
    }

    /// Answer one local CLI request.
    async fn local_op(&self, op: onlyne_proto::ClientOp) -> ResBody {
        use onlyne_proto::ClientOp;
        match op {
            ClientOp::Send(envelope) => self.forward(*envelope).await,
            ClientOp::QueryRoles(args) => {
                let role = args.role.unwrap_or_else(|| self.role.clone());
                let prose = self.dispatch.role_prose();
                ResBody::ok(serde_json::json!({
                    "roles": [{"name": role, "role": role, "prose": prose}]
                }))
            }
            other => match self.dispatch.request(other).await {
                Ok(body) => body,
                Err(error) => ResBody::err(ErrorCode::Internal, error.to_string(), None),
            },
        }
    }

    /// Hand one envelope to the live link and answer with the server's verdict.
    async fn forward(&self, envelope: onlyne_proto::Envelope) -> ResBody {
        use onlyne_proto::ClientOp;
        let op = ClientOp::Send(Box::new(envelope.clone()));
        match self.dispatch.request(op).await {
            Ok(body) if body.ok => body,
            Ok(body) => body,
            Err(error) => {
                // The link is down: the plan's disconnect rule keeps the send
                // durable rather than losing it (plan §6 line 289).
                tracing::warn!(error = %error, "local send queued because the link is down");
                match self.dispatch.enqueue_outbound(&envelope) {
                    Ok(op_id) => ResBody::ok(serde_json::json!({
                        "queued": true,
                        "op_id": op_id,
                    })),
                    Err(error) => ResBody::err(ErrorCode::Internal, error.to_string(), None),
                }
            }
        }
    }

    async fn serve_plugin(&self, stream: LocalStream, first: serde_json::Value) -> Result<()> {
        let first: onlyne_adapter::WireMessage =
            serde_json::from_value(first).context("decode an adapter frame")?;
        let role = self.role.clone();
        let cluster = self.cluster.clone();
        let server = self.server.clone();
        let dispatch = self.dispatch.clone();
        let connection = AdapterServer::accept_from_first(stream, first, move |hello| {
            let role = role.clone();
            let cluster = cluster.clone();
            let server = server.clone();
            let dispatch = dispatch.clone();
            async move {
                let mounted = mount_allowed(hello.mount.as_ref(), hello.kind, &role);
                if !mounted {
                    return Err((
                        ErrorCode::Forbidden,
                        "adapter mount does not match role".to_string(),
                    ));
                }
                let prose = dispatch.role_prose();
                Ok(HelloAck {
                    protocol: hello.protocol,
                    role,
                    session_id: hello.mount.as_ref().and_then(|m| match m {
                        Mount::Agent(a) => a.session.clone(),
                        _ => None,
                    }),
                    generation: 1,
                    prose,
                    server: ServerInfo {
                        connected: dispatch.link_up(),
                        cluster,
                        name: server,
                    },
                    host_capabilities: vec![Capability::Probe, Capability::Recycle],
                })
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
        self.serve_connection(connection).await
    }

    async fn serve_connection(&self, mut connection: ServerConnection) -> Result<()> {
        let io = connection.io.clone();
        let capabilities = connection.hello.capabilities.clone();
        let agent = connection.hello.kind == onlyne_proto::MountKind::Agent;
        // The session this connection serves, when it is an agent mount: the
        // plugin names it with the id the client spawned it for, or names
        // nothing and serves whatever session the role stages next.
        let mounted = match connection.hello.mount.as_ref() {
            Some(onlyne_proto::Mount::Agent(mount)) => mount.session.clone(),
            _ => None,
        };
        if agent {
            // The ready barrier of §6 runs now: the local `ready` row and its
            // report reach the server before the `assign` frame leaves.
            self.hand_over(mounted.as_deref(), io.clone(), capabilities.clone())
                .await;
        }
        let mut graceful_detach = false;
        while let Some(frame) = connection.inbound.recv().await {
            let id = frame.id.unwrap_or_default();
            match frame.msg {
                AdapterMsg::Plugin(PluginOp::Report(report)) => {
                    let result = match report {
                        Report::Ready {
                            task_id,
                            session_id,
                            generation,
                            ..
                        } => {
                            let prose = self.dispatch.role_prose();
                            on_ready(
                                &self.dispatch,
                                ReadyNotice {
                                    task_id,
                                    session_id,
                                    generation,
                                    io: io.clone(),
                                    capabilities: capabilities.clone(),
                                },
                                &prose,
                            )
                            .await
                            .map(|()| serde_json::Value::Null)
                        }
                        other => on_plugin_report(&self.dispatch, other)
                            .await
                            .map(|()| serde_json::Value::Null),
                    };
                    if frame.id.is_some() {
                        io.respond(id, result_to_body(result))
                            .await
                            .map_err(|e| anyhow::anyhow!(e))?;
                    }
                }
                AdapterMsg::Plugin(PluginOp::SessionRegister(args)) => {
                    if should_bye_on_register(&args.session_id) {
                        io.notify(AdapterMsg::Host(HostOp::Bye(onlyne_proto::ByeNotice {
                            reason: "session ended".into(),
                        })))
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?;
                        break;
                    }
                    if frame.id.is_some() {
                        io.respond(
                            id,
                            ResBody::ok(serde_json::json!({"registered": args.session_id})),
                        )
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?;
                    }
                }
                AdapterMsg::Plugin(PluginOp::AssignAck(args)) => {
                    self.dispatch.push_assign_ack(args);
                    if frame.id.is_some() {
                        io.respond(id, ResBody::ok(serde_json::Value::Null))
                            .await
                            .map_err(|e| anyhow::anyhow!(e))?;
                    }
                }
                AdapterMsg::Plugin(PluginOp::Detach(_)) => {
                    graceful_detach = true;
                    break;
                }
                AdapterMsg::Plugin(PluginOp::Hello(_)) => {
                    if frame.id.is_some() {
                        io.respond(
                            id,
                            ResBody::err(
                                ErrorCode::Invalid,
                                "hello already completed",
                                Some("op".into()),
                            ),
                        )
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?;
                    }
                }
                AdapterMsg::Plugin(PluginOp::Send(envelope)) => {
                    let result = self
                        .dispatch
                        .enqueue_outbound(&envelope)
                        .map(|op_id| serde_json::json!({"queued": true, "op_id": op_id}));
                    if frame.id.is_some() {
                        io.respond(id, result_to_body(result))
                            .await
                            .map_err(|e| anyhow::anyhow!(e))?;
                    }
                }
                _ => {
                    if frame.id.is_some() {
                        io.respond(
                            id,
                            ResBody::err(
                                ErrorCode::Invalid,
                                "unsupported adapter operation",
                                Some("op".into()),
                            ),
                        )
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?;
                    }
                }
            }
        }
        if agent {
            // The ended connection releases its bindings. A detach frame also
            // retires each idle resource served by this agent.
            self.dispatch
                .release_connection(mounted.as_deref(), &io, graceful_detach);
        }
        Ok(())
    }

    /// Bind a freshly mounted plugin to the session it names, or park it.
    ///
    /// A mount that names its session is that session's transport: the
    /// connection takes the staged payload for it and nothing else, because the
    /// id names the one session the client spawned this plugin for. A mount
    /// that names nothing is a plugin that arrived before any work existed: it
    /// waits as this role's parked agent and takes the next staged session
    /// (plan §6 line 285).
    async fn hand_over(
        &self,
        session_id: Option<&str>,
        io: AdapterIo,
        capabilities: Vec<Capability>,
    ) {
        let Some(session_id) = session_id else {
            self.dispatch.park_transport(io, capabilities);
            // Work that arrived ahead of this agent is staged with a payload and
            // no connection. The park is that connection now, so the wait ends
            // here, and the claim binds the session to it for the tasks a `reuse`
            // role hands the same session later.
            let Some(staged) = self.dispatch.staged_without_transport() else {
                return;
            };
            if let Err(error) = self.dispatch.hand_staged(&staged).await {
                tracing::warn!(error = %error, session = %staged, "staged hand-off refused");
            }
            return;
        };
        self.dispatch
            .bind_adapter(session_id, io, capabilities.clone());
        if let Err(error) = self.dispatch.hand_staged(session_id).await {
            tracing::warn!(error = %error, session = %session_id, "staged hand-off refused");
        }
    }
}

fn result_to_body(result: Result<serde_json::Value>) -> ResBody {
    match result {
        Ok(value) => ResBody::ok(value),
        Err(error) => ResBody::err(ErrorCode::Internal, error.to_string(), None),
    }
}

/// An `agent` mount must name this role; the `admin` probe carries no mount
/// marker at all, because `Mount` is untagged and its unit variant serializes
/// as `null`, which reads back as `None`, so the `kind` field is what
/// identifies the probe.
pub fn mount_allowed(mount: Option<&Mount>, kind: MountKind, role: &str) -> bool {
    match mount {
        Some(Mount::Agent(agent)) => agent.role == role,
        _ => kind == MountKind::Admin,
    }
}

/// A `session_register` naming a terminated session ends the plugin connection.
pub fn should_bye_on_register(session_id: &str) -> bool {
    session_id == "terminated"
}

/// Wait bound for the link probe's `hello` round trip.
///
/// The handshake runs over a local socket, and the bound matches the adapter
/// protocol's own hello budget.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Pause before the accept loop asks the listener for a connection again after
/// an `accept` failure.
///
/// The value is short enough that a transient failure costs one plugin one
/// keystroke and long enough that a persistent one stays off the CPU.
pub const ACCEPT_RETRY_PAUSE: Duration = Duration::from_millis(100);

/// The link state of the client serving `socket`, and `None` when no client
/// answers the probe.
///
/// The probe is an `admin` `hello` on the adapter surface (§7): it names no
/// mount, binds nothing, and the host's `HelloAck` carries the link state. A
/// socket nobody answers is a client that is not serving — `status` reads that
/// as not running — while a client that answers with a down link is running
/// and disconnected.
pub async fn server_link_state(socket: &Path) -> Option<bool> {
    let stream = connect_local(socket).await.ok()?;
    let io = AdapterIo::new(stream, PROBE_TIMEOUT, PROBE_TIMEOUT);
    let hello = HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: "onlyne-client".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        kind: MountKind::Admin,
        capabilities: Vec::new(),
        mount: None,
    };
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Hello(hello)))
        .await
        .ok()?;
    if !body.ok {
        return Some(false);
    }
    let ack = body
        .data
        .and_then(|value| serde_json::from_value::<HostOp>(value).ok());
    Some(matches!(
        ack,
        Some(HostOp::Welcome(ack)) if ack.server.connected
    ))
}

/// Remove a socket file a previous run left behind.
///
/// The leaf is absent in the common case, and `NotFound` is that answer. A name
/// that holds a live listener answers `connect` and keeps the bind of a client
/// restarting behind it, so the readiness path clears it first, and the log line
/// is the record that a surface was cleared.
pub async fn stale_socket_removed(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {
            tracing::info!(socket = %path.display(), "removed stale adapter socket");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove stale socket {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use onlyne_session::backend::fake::FakeBackend;
    #[cfg(unix)]
    use onlyne_store::ClientStore;
    #[cfg(unix)]
    use tempfile::tempdir;

    #[cfg(unix)]
    fn dispatch_state(workspace: &Path) -> DispatchState {
        let layout = RoleWorkspace::resolve(workspace);
        layout.bootstrap().unwrap();
        let store = ClientStore::open(layout.client_db_path()).unwrap();
        DispatchState::new(
            "planner",
            workspace,
            vec!["agent".into()],
            1,
            false,
            std::sync::Arc::new(FakeBackend::new()),
            store,
        )
    }

    /// A workspace whose canonical socket spelling is over the unix bound binds
    /// the short path, names it in the marker, and answers for it through the one
    /// accessor the clients use.
    ///
    /// The hand-joined canonical leaf is the shape this case replaces: past the
    /// bound it fails to bind, and the client keeps a server link while its local
    /// surface stays shut. Windows keeps the canonical spelling as the bound
    /// spelling, so the premise lives on unix.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_deep_workspace_serves_the_short_endpoint() {
        use onlyne_layout::UNIX_SOCKET_PATH_MAX;
        let segment = "deep-workspace-segment-aaaaaaaaaaaaaaaaaaaaaa";
        let dir = tempdir().unwrap();
        let workspace = dir.path().join(segment).join(segment).join("leaf");
        std::fs::create_dir_all(&workspace).unwrap();
        let layout = RoleWorkspace::resolve(&workspace);
        let adapter = AdapterSocket {
            workspace: workspace.clone(),
            role: "planner".into(),
            cluster: "c".into(),
            server: "s".into(),
            dispatch: dispatch_state(&workspace),
        };
        assert!(
            layout.socket_path_natural().as_os_str().len() > UNIX_SOCKET_PATH_MAX,
            "the premise: {} bytes at {}",
            layout.socket_path_natural().as_os_str().len(),
            layout.socket_path_natural().display(),
        );

        let (listener, endpoint) = adapter.bind().await.unwrap();
        assert!(
            endpoint.short(),
            "a canonical path over the bound moves the socket: {}",
            endpoint.actual().display(),
        );
        assert!(
            endpoint.actual().as_os_str().len() <= UNIX_SOCKET_PATH_MAX,
            "the served path fits the bound: {} bytes at {}",
            endpoint.actual().as_os_str().len(),
            endpoint.actual().display(),
        );
        assert_eq!(
            adapter.path(),
            endpoint.actual().to_path_buf(),
            "the accessor answers the path that was bound",
        );
        assert_eq!(
            std::fs::read_to_string(endpoint.marker()).unwrap().trim(),
            endpoint.actual().to_string_lossy().as_ref(),
            "the marker names the served path",
        );
        assert!(
            !endpoint.natural().exists(),
            "the canonical leaf stays empty: {}",
            endpoint.natural().display(),
        );
        drop(listener);
        let _ = std::fs::remove_file(endpoint.actual());
        let _ = std::fs::remove_dir(endpoint.actual().parent().unwrap());
    }

    #[test]
    fn admin_probe_mounts_without_a_marker() {
        let agent = onlyne_proto::Mount::Agent(onlyne_proto::AgentMount {
            role: "planner".to_string(),
            ..Default::default()
        });
        assert!(mount_allowed(Some(&agent), MountKind::Agent, "planner"));
        assert!(!mount_allowed(Some(&agent), MountKind::Agent, "reviewer"));
        assert!(mount_allowed(None, MountKind::Admin, "planner"));
        assert!(!mount_allowed(None, MountKind::Agent, "planner"));
    }
    #[test]
    fn terminated_register_requests_bye() {
        assert!(should_bye_on_register("terminated"));
        assert!(!should_bye_on_register("live-session"));
    }
}
