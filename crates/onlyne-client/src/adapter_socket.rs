use anyhow::{Context, Result};
use onlyne_adapter::{AdapterServer, ServerConnection};
use onlyne_layout::apply_private_mode;
use onlyne_proto::{AdapterMsg, Capability, ErrorCode, HelloAck, HostOp, Mount, MountKind, PluginOp, ResBody, ServerInfo};
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};
use tokio::time::{Duration, timeout};
use crate::dispatch::{DispatchState, on_plugin_report};

#[derive(Clone)]
pub struct AdapterSocket {
    pub workspace: PathBuf,
    pub role: String,
    pub prose: String,
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
        let prose = self.prose.clone();
        let cluster = self.cluster.clone();
        let server = self.server.clone();
        let connection = AdapterServer::accept_async(stream, move |hello| {
            let role = role.clone(); let prose = prose.clone(); let cluster = cluster.clone(); let server = server.clone();
            async move {
                let mounted = match hello.mount.as_ref() { Some(Mount::Agent(m)) => m.role == role, Some(Mount::Admin) if hello.kind == MountKind::Admin => true, _ => false };
                if !mounted { return Err((ErrorCode::Forbidden, "adapter mount does not match role".to_string())); }
                Ok(HelloAck { protocol: hello.protocol, role, session_id: hello.mount.as_ref().and_then(|m| match m { Mount::Agent(a) => a.session.clone(), _ => None }), generation: 1, prose, server: ServerInfo { connected: true, cluster, name: server }, host_capabilities: vec![Capability::Probe, Capability::Recycle] })
            }
        }).await.map_err(|e| anyhow::anyhow!(e))?;
        self.serve_connection(connection).await
    }

    async fn serve_connection(&self, mut connection: ServerConnection) -> Result<()> {
        let io = connection.io.clone();
        while let Some(frame) = connection.inbound.recv().await {
            let id = frame.id.unwrap_or_default();
            match frame.msg {
                AdapterMsg::Plugin(PluginOp::Report(report)) => {
                    let result = on_plugin_report(&self.dispatch, report).await;
                    if frame.id.is_some() { io.respond(id, result_to_body(result)).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
                AdapterMsg::Plugin(PluginOp::SessionRegister(args)) => {
                    if frame.id.is_some() { io.respond(id, ResBody::ok(serde_json::json!({"registered": args.session_id}))).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
                AdapterMsg::Plugin(PluginOp::AssignAck(_)) => {
                    if frame.id.is_some() { io.respond(id, ResBody::ok(serde_json::Value::Null)).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
                AdapterMsg::Plugin(PluginOp::Detach(_)) => break,
                AdapterMsg::Plugin(PluginOp::Hello(_)) => {
                    if frame.id.is_some() { io.respond(id, ResBody::err(ErrorCode::Invalid, "hello already completed", Some("op".into()))).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
                AdapterMsg::Plugin(PluginOp::Send(_)) => {
                    if frame.id.is_some() { io.respond(id, ResBody::ok(serde_json::Value::Null)).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
                _ => {
                    if frame.id.is_some() { io.respond(id, ResBody::err(ErrorCode::Invalid, "unsupported adapter operation", Some("op".into()))).await.map_err(|e| anyhow::anyhow!(e))?; }
                }
            }
        }
        Ok(())
    }
}

fn result_to_body(result: Result<()>) -> ResBody {
    match result { Ok(()) => ResBody::ok(serde_json::Value::Null), Err(error) => ResBody::err(ErrorCode::Internal, error.to_string(), None) }
}

pub async fn stale_socket_removed(path: &Path) -> Result<()> {
    if path.exists() { tracing::info!(socket = %path.display(), "removing stale adapter socket"); tokio::fs::remove_file(path).await?; }
    Ok(())
}
