use super::socket::AdapterSocket;
use crate::session::dispatch::{
    DispatchState, ReadyNotice, on_plugin_report, on_ready, sync_frame, sync_session,
};
use anyhow::{Context, Result};
use onlyne_adapter::{AdapterIo, AdapterServer, ServerConnection};
use onlyne_proto::{
    AdapterMsg, Capability, ErrorCode, HelloAck, HostOp, Mount, MountKind, PluginOp, Report,
    ResBody, ServerInfo,
};
use onlyne_wire::socket::LocalStream;

/// How often a hosting connection looks for a session the client staged for it.
///
/// The runloop's own readiness tick is the same order of magnitude, and a
/// delivery that finds no session is already a wait rather than a refusal, so
/// the session opens on the next tick and not before.
const HOSTING_POLL: std::time::Duration = std::time::Duration::from_millis(250);

impl AdapterSocket {
    /// Serve one accepted connection on whichever surface it opened.
    ///
    /// The workspace socket carries two vocabularies (plan §7 line 293): a
    /// plugin opens with an adapter `hello`, and the local CLI opens with a
    /// request frame. The first frame decides, so one path serves both.
    pub(super) async fn connection(&self, mut stream: LocalStream) -> Result<()> {
        let first = onlyne_wire::read_frame::<_, serde_json::Value>(&mut stream)
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
                None => match onlyne_wire::read_frame::<_, serde_json::Value>(&mut stream).await {
                    Ok(Some(value)) => value,
                    Ok(None) => return Ok(()),
                    Err(error) => return Err(anyhow::anyhow!(error.to_string())),
                },
            };
            let frame: onlyne_proto::Frame<onlyne_proto::ClientOp> =
                serde_json::from_value(value).context("decode a client frame")?;
            if let onlyne_proto::Frame::Ping { t } = frame {
                let pong = onlyne_proto::Frame::<onlyne_proto::ClientOp>::Pong { t, server_seq: 0 };
                onlyne_wire::write_frame(&mut stream, &pong)
                    .await
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                continue;
            }
            let onlyne_proto::Frame::Req { id, op } = frame else {
                return Ok(());
            };
            let body = self.local_op(op).await;
            let reply = onlyne_proto::Frame::<onlyne_proto::ClientOp>::res(id, body);
            onlyne_wire::write_frame(&mut stream, &reply)
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
            ClientOp::Report(report @ Report::Complete { .. }) => {
                // The operator's door answers a completion that breaks the
                // client's own shape rule the way every other door does
                // (`docs/v2-CONTRACT.md` §3b). No connection arrived with the
                // frame, so only the rule over the report itself is read here:
                // the relay guard measures a session's delivery record, and this
                // door has no sender whose record it could read.
                match self.dispatch.completion_refusal(None, &report) {
                    Some(refusal) => refusal,
                    None => result_to_body(
                        on_plugin_report(&self.dispatch, None, report)
                            .await
                            .map(|()| serde_json::Value::Null),
                    ),
                }
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
                // A tools mount names no session of its own: the token is the
                // binding, so the session it speaks for, that session's
                // generation, and the role it speaks as all come from this
                // client's record rather than from a field the caller supplies
                // (`docs/v2-CONTRACT.md` §3b). A token that names no live
                // session is refused here, before any welcome, and the sentence
                // carries the field the adapter's welcome refusal has no slot
                // for.
                let (session_id, generation) = match hello.mount.as_ref() {
                    Some(Mount::Tools(mount)) => {
                        let Some(session) = dispatch.tools_mount_for(&mount.token) else {
                            return Err((
                                ErrorCode::Unauthorized,
                                DispatchState::TOOLS_GONE_MESSAGE.to_string(),
                            ));
                        };
                        (Some(session.session_id), session.generation)
                    }
                    Some(Mount::Agent(mount)) => (mount.session.clone(), 1),
                    _ => (None, 1),
                };
                let prose = dispatch.role_prose();
                // The same claim the runloop makes to the server at `hello`: the
                // tasks this client already holds a session for, read from its
                // durable store. A plugin that says hello after a restart seeds
                // its own injections from this list instead of racing a second
                // copy of work the host already dispatched.
                // A store failure is logged by `hello_live_sessions` itself; the
                // seed list then carries the memory half, which is all this
                // process can still prove it serves. A session bound to no
                // delivery seeds nothing: there is no task to name.
                let delivered_tasks: Vec<String> = dispatch
                    .hello_live_sessions()
                    .unwrap_or_else(|_| dispatch.live_claim_from_slots())
                    .into_iter()
                    .filter_map(|session| session.task_id)
                    .collect();
                Ok(HelloAck {
                    protocol: hello.protocol,
                    role,
                    session_id,
                    generation,
                    prose,
                    server: ServerInfo {
                        connected: dispatch.link_up(),
                        cluster,
                        name: server,
                    },
                    host_capabilities: vec![Capability::Probe, Capability::Recycle],
                    delivered_tasks,
                })
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
        // The token's session can retire between the handshake and this line. The
        // binding written here is what every later frame is measured against, so
        // a connection that cannot take it is closed without serving anything
        // (`docs/v2-CONTRACT.md` §3b).
        if let Some(Mount::Tools(mount)) = connection.hello.mount.as_ref() {
            if self
                .dispatch
                .bind_tools_mount(&mount.token, connection.io.clone())
                .is_none()
            {
                return Ok(());
            }
        }
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
        // A tools mount holds no process: it carries one session's obligations
        // and nothing about its lifecycle, so it is neither a transport nor a
        // parked agent and the agent paths below are not its own
        // (`docs/v2-CONTRACT.md` §3b). Its binding was written by `serve_plugin`
        // before this connection was served.
        let tools = matches!(
            connection.hello.mount.as_ref(),
            Some(onlyne_proto::Mount::Tools(_)),
        );
        if agent {
            // The ready barrier of §6 runs now: the local `ready` row and its
            // report reach the server before the `assign` frame leaves.
            self.hand_over(mounted.as_deref(), io.clone(), capabilities.clone())
                .await;
        }
        let mut graceful_detach = false;
        // A hosting runtime is asked for a session rather than left to find one:
        // the client stages a session with no transport, and the connection that
        // stands for this role is the one that can serve it. Nothing inbound
        // would otherwise wake this loop — the `assign` that would tell the
        // runtime about the delivery is exactly what cannot be sent, because the
        // session has no transport yet — so the tick is what closes the circle.
        let hosting = Capability::is_hosting(&capabilities);
        // The loop's own errors (a reply that cannot be written is the usual one,
        // and it is exactly what a dying plugin causes) end the loop and nothing
        // else. The release below is what removes this connection's transport and
        // starts the drop clock, so it runs on every way out and the loop's result
        // is returned after it.
        let served: Result<()> = async {
        loop {
            let frame = tokio::select! {
                frame = connection.inbound.recv() => match frame {
                    Some(frame) => frame,
                    None => break,
                },
                _ = tokio::time::sleep(HOSTING_POLL), if hosting => {
                    if let Err(error) = self.offer_staged(&io, capabilities.clone()).await {
                        tracing::warn!(error = %error, "a standing runtime could not be offered a session");
                    }
                    continue;
                }
            };
            let id = frame.id.unwrap_or_default();
            // A tools mount speaks for a session while that session lives, and
            // the token dies with it: the check runs before every frame, and a
            // frame that finds no session is answered `unauthorized` and takes
            // the connection with it. A frame carrying no id can be told
            // nothing, and the connection closes just the same.
            if tools && !self.dispatch.tools_connection_live(&io) {
                if frame.id.is_some() {
                    io.respond(id, DispatchState::tools_gone())
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?;
                }
                break;
            }
            // Nothing in this handler may tell this connection to leave ahead of
            // the answer it is about to receive.
            let _held = self.dispatch.hold_frame(&io);
            // A tools mount serves the agent mount's op set minus everything
            // about process lifecycle: it holds no process, so it registers no
            // session and acks no assignment (`docs/v2-CONTRACT.md` §3b). Every
            // other op is refused by name, like every other mount's, and the
            // duplicate `hello` keeps the answer the arm below already gives.
            if tools
                && let AdapterMsg::Plugin(op) = &frame.msg
                && !matches!(
                    op,
                    PluginOp::Send(_)
                        | PluginOp::Handoff(_)
                        | PluginOp::Report(_)
                        | PluginOp::Detach(_)
                        | PluginOp::Hello(_)
                )
            {
                if frame.id.is_some() {
                    io.respond(
                        id,
                        ResBody::err(
                            ErrorCode::Forbidden,
                            format!("{} is not allowed on a tools mount", op.name()),
                            Some("op".into()),
                        ),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!(e))?;
                }
                continue;
            }
            match frame.msg {
                AdapterMsg::Plugin(PluginOp::Report(report)) => {
                    let body = if tools {
                        self.tools_report(&io, report).await
                    } else if let Some(refusal) = self.dispatch.completion_refusal(Some(&io), &report)
                    {
                        // The client's own constraints answer before the frame is
                        // applied, the same `ResBody` the tools door hands its own
                        // mount (`docs/v2-CONTRACT.md` §3b): one checkpoint, both
                        // drives. A refusal is the failed tool call it is — the
                        // plugin raises it to the model, and nothing half-written
                        // is left on the row.
                        refusal
                    } else {
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
                            // The frame carries its sender: a report only moves
                            // the state of a session this very connection
                            // serves.
                            other => on_plugin_report(&self.dispatch, Some(&io), other)
                                .await
                                .map(|()| serde_json::Value::Null),
                        };
                        result_to_body(result)
                    };
                    if frame.id.is_some() {
                        io.respond(id, body).await.map_err(|e| anyhow::anyhow!(e))?;
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
                    // A detach that carries an id is a request like any other
                    // and is answered before the connection is let go; the
                    // answer is queued ahead of the writer's end of stream. The
                    // plugin said it is leaving before the reply is written, so a
                    // reply that cannot be written is still a graceful goodbye.
                    graceful_detach = true;
                    if frame.id.is_some() {
                        io.respond(id, ResBody::ok(serde_json::Value::Null))
                            .await
                            .map_err(|e| anyhow::anyhow!(e))?;
                    }
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
                AdapterMsg::Plugin(PluginOp::Send(mut envelope)) => {
                    // A live connection's frame lands in the durable outbound
                    // queue. A frame from a connection held read-only because its
                    // session was taken by a newer one is held for that task's
                    // completion, which is what routes it beside the newer
                    // session's own handoff.
                    //
                    // A tools mount's frame is measured against the family's
                    // ceiling and stamped before it is queued: the sender and
                    // the whole causality chain come from this client's own
                    // record of the delivery the session serves, never from the
                    // bridge (`docs/v2-CONTRACT.md` §3b).
                    let prepared = if tools {
                        self.dispatch.stamp_tools_send(&io, &mut envelope)
                    } else {
                        Ok(())
                    };
                    let body = match prepared {
                        Ok(()) => result_to_body(self.dispatch.plugin_send(&io, &envelope)),
                        Err(refusal) => refusal,
                    };
                    if frame.id.is_some() {
                        io.respond(id, body).await.map_err(|e| anyhow::anyhow!(e))?;
                    }
                }
                AdapterMsg::Plugin(PluginOp::Handoff(mut args)) => {
                    // The frame names the task the session hands on, and the
                    // answer names the child the host minted for the recipient.
                    // A refused frame answers the same error shape every other
                    // plugin frame answers with. A tools mount names no task at
                    // all, so its frame is stamped from the client's own record
                    // first (`docs/v2-CONTRACT.md` §3b).
                    let stamped = if tools {
                        self.dispatch.stamp_tools_task(&io, &mut args.task_id)
                    } else {
                        Ok(())
                    };
                    let body = match stamped {
                        Err(refusal) => refusal,
                        // The child this frame mints sits one hop below the task the
                        // session serves, and the family's ceiling is measured where
                        // the frame is handled (`docs/v2-CONTRACT.md` §3b).
                        Ok(()) if tools => match self.dispatch.handoff_refusal(&io, &args) {
                            Some(refusal) => refusal,
                            None => self.dispatch.plugin_handoff(&io, args),
                        },
                        Ok(()) => self.dispatch.plugin_handoff(&io, args),
                    };
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
        Ok::<(), anyhow::Error>(())
        }
        .await;
        if tools {
            // A tools mount holds no slot and no resource: its binding goes with
            // the connection, and nothing else does. No retirement runs, no exit
            // is published, and no bye is owed — this connection carried one
            // session's obligations, not its process
            // (`docs/v2-CONTRACT.md` §3b).
            self.dispatch.release_tools_connection(&io);
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
                        error = %error,
                        session = %session,
                        "a retired session's exit was not published"
                    );
                    // A lost exit leaves the server's mirror row open forever,
                    // so the failure is not the end of the publish. The same
                    // heartbeat frame `send_frame` would have queued for a down
                    // link goes into the durable intent table here, and the next
                    // link flush carries the exit. `sync_session` reaching this
                    // arm means its own queueing failed too — usually a store
                    // that refused one write, not one that refuses every write —
                    // and a second failed attempt is logged, never swallowed.
                    match sync_frame(&self.dispatch, &session) {
                        Ok(Some(op)) => {
                            if let Err(error) = self.dispatch.enqueue_op(&op) {
                                tracing::warn!(
                                    error = %error,
                                    session = %session,
                                    "a retired session's sync intent was not queued"
                                );
                            }
                        }
                        // No row, nothing to publish: the exit this loop owes was
                        // never a stored session.
                        Ok(None) => {}
                        Err(error) => tracing::warn!(
                            error = %error,
                            session = %session,
                            "a retired session's sync frame could not be rebuilt"
                        ),
                    }
                }
            }
        }
        served
    }

    /// Answer one `report` frame from a `tools` mount.
    ///
    /// A tools mount holds no process, so the ready barrier and the liveness beat
    /// describe nothing it has: a completion is the one report this path serves,
    /// and every other kind is refused by name
    /// (`docs/v2-CONTRACT.md` §3b). The task that completion settles is stamped
    /// from the client's own record of the delivery the session serves, because
    /// the mount names none — the refusal a stamp can bring answers here, before
    /// `on_plugin_report` is reached, so that door only ever sees the stamped
    /// value.
    async fn tools_report(&self, io: &AdapterIo, mut report: Report) -> ResBody {
        let kind = report.kind_name();
        let Report::Complete { task_id, .. } = &mut report else {
            return ResBody::err(
                ErrorCode::Forbidden,
                format!("report {kind} is not served on a tools mount"),
                None,
            );
        };
        if let Err(refusal) = self.dispatch.stamp_tools_task(io, task_id) {
            return refusal;
        }
        // The client's own constraints answer before the frame is applied: a
        // completion that breaks one is refused as the failed tool call it is,
        // which is the answer the pi plugin's own tools give on the other drive
        // (`docs/v2-CONTRACT.md` §3b).
        if let Some(refusal) = self.dispatch.completion_refusal(Some(io), &report) {
            return refusal;
        }
        match on_plugin_report(&self.dispatch, Some(io), report).await {
            Ok(()) => ResBody::ok(serde_json::Value::Null),
            Err(error) => ResBody::err(ErrorCode::Internal, error.to_string(), None),
        }
    }

    /// Offer this standing connection a session the client staged with no
    /// transport, and take the answer to `open`.
    ///
    /// The host named the session when it staged it, because that is the key
    /// every other lookup uses. The runtime names the conversation it opened, and
    /// hands back an opaque handle for finding it again — both go beside the host's
    /// name rather than replacing it.
    ///
    /// Nothing here composes a history summary for a runtime that cannot resume.
    /// A session with no handle is one the runtime will open fresh next time,
    /// and a summary this process invented is context the model never produced.
    async fn offer_staged(&self, io: &AdapterIo, capabilities: Vec<Capability>) -> Result<()> {
        let Some(staged) = self.dispatch.staged_hosting_session() else {
            return Ok(());
        };
        let asked = self.dispatch.hosting_open_args(&staged);
        let answer = io
            .request(AdapterMsg::Host(HostOp::Open(asked)))
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if !answer.ok {
            tracing::warn!(
                session = %staged,
                "a hosting runtime refused to open a session; the delivery waits"
            );
            return Ok(());
        }
        let opened: onlyne_proto::OpenedArgs =
            serde_json::from_value(answer.data.unwrap_or(serde_json::Value::Null))?;
        if self
            .dispatch
            .hosted_session_ready(&staged, &opened, io.clone(), capabilities)
        {
            // The transport exists now, so the payload the slot has been holding
            // goes out on it. This is the same hand-off a mount performs for a
            // session that was waiting when it arrived.
            let _ = self.dispatch.hand_staged(&staged).await;
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
    ///
    /// A `tools` mount reaches neither branch: it holds no process and serves no
    /// payload, so its binding was written by `serve_plugin` and this door is
    /// never its own (`docs/v2-CONTRACT.md` §3b).
    async fn hand_over(
        &self,
        session_id: Option<&str>,
        io: AdapterIo,
        capabilities: Vec<Capability>,
    ) {
        // A mount names the session it was spawned for, and a plugin that
        // outlived a restart still names the one it was serving when it redials —
        // to a client that has no memory of it, because nothing here rebuilds
        // slots from the store. Binding the transport under that name gives this
        // connection a session nothing will ever serve: no payload reaches it, and
        // every beat it sends lands on a refusal. So the name is dropped and the
        // mount parks like one that named nothing, which is what it is to a client
        // holding no such slot. The agent keeps its conversation and takes the
        // next staged delivery.
        let named = session_id.filter(|id| self.dispatch.knows_session(id));
        if session_id.is_some() && named.is_none() {
            tracing::warn!(
                session = ?session_id,
                capabilities = ?capabilities,
                "a plugin named a session this client holds no slot for; it is parked for the \
                 next staged delivery rather than bound to a name nothing will serve"
            );
        }
        let Some(session_id) = named else {
            // A runtime that declared `open`, `suspend` or `close` owns its
            // sessions, so its connection is this role's standing transport: it
            // joins no queue and is consumed by no claim. It is also handed
            // nothing here — a session reaches it by being asked for, on the
            // runloop's tick, and a hand-over at mount time would bind it a
            // session the runtime never opened and never named. Everything else
            // is a spawned agent waiting for the next session, which is what the
            // park has always meant, and a role whose only runtime is pi behaves
            // exactly as it did before this branch existed.
            if Capability::is_hosting(&capabilities) {
                self.dispatch.stand_transport(io, capabilities);
                return;
            }
            self.dispatch.park_transport(io, capabilities);
            // Work that arrived ahead of this agent is staged with a payload and
            // no connection. The connection is available now, so the wait ends
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

/// An `agent` mount must name this role, and a `tools` mount must carry the
/// mount its kind claims; the `admin` probe carries no mount marker at all,
/// because `Mount` is untagged and its unit variant serializes as `null`, which
/// reads back as `None`, so the `kind` field is what identifies the probe.
///
/// A `tools` mount names no role of its own: the token *is* the binding
/// (`docs/v2-CONTRACT.md` §3b), and the session it speaks for is resolved at the
/// handshake, against this client's own record.
pub fn mount_allowed(mount: Option<&Mount>, kind: MountKind, role: &str) -> bool {
    match mount {
        Some(Mount::Agent(agent)) => agent.role == role,
        Some(Mount::Tools(_)) => kind == MountKind::Tools,
        _ => kind == MountKind::Admin,
    }
}

/// A `session_register` naming a terminated session ends the plugin connection.
pub fn should_bye_on_register(session_id: &str) -> bool {
    session_id == "terminated"
}
