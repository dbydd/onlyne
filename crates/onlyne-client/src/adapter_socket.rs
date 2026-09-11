use crate::dispatch::{DispatchState, ReadyNotice, on_plugin_report, on_ready};
use anyhow::{Context, Result};
use onlyne_adapter::{AdapterIo, AdapterServer, ServerConnection};
use onlyne_layout::apply_private_mode;
use onlyne_proto::{
    AdapterMsg, Capability, ErrorCode, HelloAck, HostOp, Mount, MountKind, PluginOp, Report,
    ResBody, ServerInfo,
};
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};

#[derive(Clone)]
pub struct AdapterSocket {
    pub workspace: PathBuf,
    pub role: String,
    pub cluster: String,
    pub server: String,
    pub dispatch: DispatchState,
}

impl AdapterSocket {
    pub fn path(&self) -> PathBuf {
        self.workspace.join(".onlyne/run/s")
    }

    pub async fn bind(&self) -> Result<UnixListener> {
        let path = self.path();
        if path.exists() {
            tracing::info!(socket = %path.display(), "removing stale adapter socket");
            tokio::fs::remove_file(&path)
                .await
                .with_context(|| format!("remove stale socket {}", path.display()))?;
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let listener =
            UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
        apply_private_mode(&path).map_err(|e| anyhow::anyhow!(e))?;
        Ok(listener)
    }

    pub async fn serve(self) -> Result<()> {
        let listener = self.bind().await?;
        loop {
            let (stream, _) = listener.accept().await?;
            let this = self.clone();
            tokio::spawn(async move {
                if let Err(err) = this.connection(stream).await {
                    tracing::debug!(error = %err, "adapter connection closed");
                }
            });
        }
    }

    /// Serve one accepted connection on whichever surface it opened.
    ///
    /// The workspace socket carries two vocabularies (plan §7 line 293): a
    /// plugin opens with an adapter `hello`, and the local CLI opens with a
    /// request frame. The first frame decides, so one path serves both.
    async fn connection(&self, mut stream: UnixStream) -> Result<()> {
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
    /// a durable intent (plan §6 line 289).
    async fn serve_local(&self, mut stream: UnixStream, first: serde_json::Value) -> Result<()> {
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

    async fn serve_plugin(&self, stream: UnixStream, first: serde_json::Value) -> Result<()> {
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
                        connected: true,
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
                AdapterMsg::Plugin(PluginOp::AssignAck(_)) => {
                    if frame.id.is_some() {
                        io.respond(id, ResBody::ok(serde_json::Value::Null))
                            .await
                            .map_err(|e| anyhow::anyhow!(e))?;
                    }
                }
                AdapterMsg::Plugin(PluginOp::Detach(_)) => break,
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
            // The connection is over: what it bound stops being reachable, so
            // no later task of this role is routed to it.
            self.dispatch.release_connection(mounted.as_deref());
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

pub async fn stale_socket_removed(path: &Path) -> Result<()> {
    if path.exists() {
        tracing::info!(socket = %path.display(), "removing stale adapter socket");
        tokio::fs::remove_file(path).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
