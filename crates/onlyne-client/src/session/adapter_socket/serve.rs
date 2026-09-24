use super::socket::AdapterSocket;
use crate::session::dispatch::{ReadyNotice, on_plugin_report, on_ready, sync_session};
use anyhow::{Context, Result};
use onlyne_adapter::{AdapterIo, AdapterServer, ServerConnection};
use onlyne_layout::LocalStream;
use onlyne_proto::{
    AdapterMsg, Capability, ErrorCode, HelloAck, HostOp, Mount, MountKind, PluginOp, Report,
    ResBody, ServerInfo,
};

impl AdapterSocket {
    /// Serve one accepted connection on whichever surface it opened.
    ///
    /// The workspace socket carries two vocabularies (plan §7 line 293): a
    /// plugin opens with an adapter `hello`, and the local CLI opens with a
    /// request frame. The first frame decides, so one path serves both.
    pub(super) async fn connection(&self, mut stream: LocalStream) -> Result<()> {
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
            // Nothing in this handler may tell this connection to leave ahead of
            // the answer it is about to receive.
            let _held = self.dispatch.hold_frame(&io);
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
                                    io: Some(io.clone()),
                                    capabilities: capabilities.clone(),
                                },
                                &prose,
                            )
                            .await
                            .map(|()| serde_json::Value::Null)
                        }
                        // The frame carries its sender: a report only moves the
                        // state of a session this very connection serves.
                        other => on_plugin_report(&self.dispatch, Some(&io), other)
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
                    // A live connection's frame lands in the durable outbound
                    // queue. A frame from a connection held read-only because its
                    // session was taken by a newer one is held for that task's
                    // completion, which is what routes it beside the newer
                    // session's own handoff.
                    let result = self.dispatch.plugin_send(&io, &envelope);
                    if frame.id.is_some() {
                        io.respond(id, result_to_body(result))
                            .await
                            .map_err(|e| anyhow::anyhow!(e))?;
                    }
                }
                AdapterMsg::Plugin(PluginOp::Handoff(args)) => {
                    // The frame names the task the session hands on, and the
                    // answer names the child the host minted for the recipient.
                    // A refused frame answers the same error shape every other
                    // plugin frame answers with.
                    let body = self.dispatch.plugin_handoff(&io, args);
                    if frame.id.is_some() {
                        io.respond(id, body).await.map_err(|e| anyhow::anyhow!(e))?;
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
            let retired =
                self.dispatch
                    .release_connection(mounted.as_deref(), &io, graceful_detach);
            // Each retirement wrote that session's own row — the resource close
            // and, for a completed session, the agent's exit — and the server
            // mirrors only what this client reports, so the exit travels here,
            // where the lock is back and the stored row is final. A session the
            // completion already settled had its delivery row answered before
            // the publish, and a published exit runs the server's
            // `release_exited_delivery`, which after 200c88d refuses a released
            // row whose task already carries a verdict rather than handing the
            // work back. That is the second reason this publish sits after the
            // close: the mirror moves the row the close wrote, and a close
            // already answered is a row the release cannot re-offer.
            for session in retired {
                if let Err(error) = sync_session(&self.dispatch, &session).await {
                    tracing::warn!(
                        session = %session,
                        error = %error,
                        "a retired session's exit was not published"
                    );
                }
            }
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
            // here, and the claim binds the session to this plugin.
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
