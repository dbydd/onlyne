use anyhow::{Context, Result};
use onlyne_adapter::{AdapterServer, ServerConnection};
use onlyne_layout::apply_private_mode;
use onlyne_proto::{AdapterMsg, Capability, ErrorCode, HelloAck, HostOp, Mount, MountKind, PluginOp, Report, ResBody, ServerInfo};
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};
use crate::dispatch::{DispatchState, ReadyNotice, on_plugin_report, on_ready};

#[derive(Clone)]
pub struct AdapterSocket {
    pub workspace: PathBuf,
    pub role: String,
    pub cluster: String,
    pub server: String,
    pub dispatch: DispatchState,
}

impl AdapterSocket {
    pub fn path(&self) -> PathBuf { self.workspace.join(".onlyne/run/s") }

    pub async fn bind(&self) -> Result<UnixListener> {
        let path = self.path();
        if path.exists() {
            tracing::info!(socket = %path.display(), "removing stale adapter socket");
            tokio::fs::remove_file(&path).await.with_context(|| format!("remove stale socket {}", path.display()))?;
        }
        if let Some(parent) = path.parent() { tokio::fs::create_dir_all(parent).await?; }
        let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
        apply_private_mode(&path).map_err(|e| anyhow::anyhow!(e))?;
        Ok(listener)
    }

    pub async fn serve(self) -> Result<()> {
        let listener = self.bind().await?;
        loop {
            let (stream, _) = listener.accept().await?;
            let this = self.clone();
            tokio::spawn(async move {
                if let Err(err) = this.connection(stream).await { tracing::debug!(error = %err, "adapter connection closed"); }
            });
        }
    }

    async fn connection(&self, stream: UnixStream) -> Result<()> {
        let role = self.role.clone();
        let cluster = self.cluster.clone();
        let server = self.server.clone();
        let dispatch = self.dispatch.clone();
        let connection = AdapterServer::accept_async(stream, move |hello| {
            let role = role.clone(); let cluster = cluster.clone(); let server = server.clone(); let dispatch = dispatch.clone();
            async move {
                let mounted = match hello.mount.as_ref() { Some(Mount::Agent(m)) => m.role == role, Some(Mount::Admin) if hello.kind == MountKind::Admin => true, _ => false };
                if !mounted { return Err((ErrorCode::Forbidden, "adapter mount does not match role".to_string())); }
                let prose = dispatch.role_prose();
                Ok(HelloAck { protocol: hello.protocol, role, session_id: hello.mount.as_ref().and_then(|m| match m { Mount::Agent(a) => a.session.clone(), _ => None }), generation: 1, prose, server: ServerInfo { connected: true, cluster, name: server }, host_capabilities: vec![Capability::Probe, Capability::Recycle] })
            }
        }).await.map_err(|e| anyhow::anyhow!(e))?;
        self.serve_connection(connection).await
    }

    async fn serve_connection(&self, mut connection: ServerConnection) -> Result<()> {
        let io = connection.io.clone();
        let capabilities = connection.hello.capabilities.clone();
        while let Some(frame) = connection.inbound.recv().await {
            let id = frame.id.unwrap_or_default();
            match frame.msg {
                AdapterMsg::Plugin(PluginOp::Report(report)) => {
                    let result = match report {
                        Report::Ready { task_id, session_id, generation, .. } => {
                            let prose = self.dispatch.role_prose();
                            on_ready(&self.dispatch, ReadyNotice { task_id, session_id, generation, io: io.clone(), capabilities: capabilities.clone() }, &prose).await.map(|()| serde_json::Value::Null)
                        }
                        other => on_plugin_report(&self.dispatch, other).await.map(|()| serde_json::Value::Null),
                    };
                    if frame.id.is_some() { io.respond(id, result_to_body(result)).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
                AdapterMsg::Plugin(PluginOp::SessionRegister(args)) => {
                    if should_bye_on_register(&args.session_id) {
                        io.notify(AdapterMsg::Host(HostOp::Bye(onlyne_proto::ByeNotice { reason: "session ended".into() }))).await.map_err(|e| anyhow::anyhow!(e))?;
                        break;
                    }
                    if frame.id.is_some() { io.respond(id, ResBody::ok(serde_json::json!({"registered": args.session_id}))).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
                AdapterMsg::Plugin(PluginOp::AssignAck(_)) => {
                    if frame.id.is_some() { io.respond(id, ResBody::ok(serde_json::Value::Null)).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
                AdapterMsg::Plugin(PluginOp::Detach(_)) => break,
                AdapterMsg::Plugin(PluginOp::Hello(_)) => {
                    if frame.id.is_some() { io.respond(id, ResBody::err(ErrorCode::Invalid, "hello already completed", Some("op".into()))).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
                AdapterMsg::Plugin(PluginOp::Send(envelope)) => {
                    let result = self.dispatch.enqueue_outbound(&envelope).map(|op_id| serde_json::json!({"queued": true, "op_id": op_id}));
                    if frame.id.is_some() { io.respond(id, result_to_body(result)).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
                _ => {
                    if frame.id.is_some() { io.respond(id, ResBody::err(ErrorCode::Invalid, "unsupported adapter operation", Some("op".into()))).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
            }
        }
        Ok(())
    }
}

fn result_to_body(result: Result<serde_json::Value>) -> ResBody {
    match result { Ok(value) => ResBody::ok(value), Err(error) => ResBody::err(ErrorCode::Internal, error.to_string(), None) }
}


/// A `session_register` naming a terminated session ends the plugin connection.
pub fn should_bye_on_register(session_id: &str) -> bool { session_id == "terminated" }

pub async fn stale_socket_removed(path: &Path) -> Result<()> {
    if path.exists() { tracing::info!(socket = %path.display(), "removing stale adapter socket"); tokio::fs::remove_file(path).await?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminated_register_requests_bye() {
        assert!(should_bye_on_register("terminated"));
        assert!(!should_bye_on_register("live-session"));
    }
}
