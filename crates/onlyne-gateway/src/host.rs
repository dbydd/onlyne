use onlyne_adapter::{AdapterError, GatewayHandle, GatewayHost};
use onlyne_proto::{Delivery, Envelope, GatewayHealth, HealthArgs, RegisterChannelArgs, TypingArgs};
use std::{path::{Path, PathBuf}, time::Instant};
use tokio::net::UnixStream;


/// Server-facing gateway host for a platform plugin.
///
/// The host owns the adapter socket and keeps platform plugins focused on their
/// SDK calls and translation functions.
pub struct Host {
    gateway: GatewayHandle,
    started: Instant,
    gateway_id: String,
    platform: String,
}

impl Host {
    pub fn new(gateway: GatewayHandle, gateway_id: impl Into<String>, platform: impl Into<String>) -> Self {
        Self {
            gateway,
            started: Instant::now(),
            gateway_id: gateway_id.into(),
            platform: platform.into(),
        }
    }

    pub async fn connect(
        socket: &Path,
        gateway_id: impl Into<String>,
        platform: impl Into<String>,
        capabilities: Vec<onlyne_proto::Capability>,
    ) -> Result<Self, AdapterError> {
        let stream = UnixStream::connect(socket).await?;
        let gateway = onlyne_adapter::AdapterClient::gateway(stream);
        let gateway_id = gateway_id.into();
        let platform = platform.into();
        gateway
            .hello_gateway(gateway_id.clone(), platform.clone(), capabilities)
            .await?;
        Ok(Self::new(gateway, gateway_id, platform))
    }

    pub fn gateway_id(&self) -> &str {
        &self.gateway_id
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    pub fn uptime_s(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    pub async fn register(&self, args: RegisterChannelArgs) -> Result<(), AdapterError> {
        self.gateway.register_channel(args).await
    }

    pub async fn send_health(
        &self,
        state: GatewayHealth,
        detail: Option<String>,
    ) -> Result<(), AdapterError> {
        self.gateway.health(state, detail, self.uptime_s()).await
    }

    pub async fn send_typing(
        &self,
        conversation: impl Into<String>,
        seconds: u32,
    ) -> Result<(), AdapterError> {
        self.gateway.typing(conversation, seconds).await
    }

    pub async fn deliver(&self, envelope: &Envelope) -> Result<(), AdapterError> {
        self.gateway
            .deliver_inbound(Delivery {
                msg_id: envelope.id.clone(),
                envelope: Box::new(envelope.clone()),
            })
            .await
    }

    pub async fn next_host_op(&self) -> Result<onlyne_proto::HostOp, AdapterError> {
        self.gateway.next_host_op().await
    }
}

#[async_trait::async_trait]
impl GatewayHost for Host {
    async fn deliver_inbound(&mut self, envelope: &Envelope) -> Result<(), AdapterError> {
        self.deliver(envelope).await
    }

    async fn report_health(&mut self, health: &HealthArgs) -> Result<(), AdapterError> {
        let state = match health.state.as_str() {
            "online" => GatewayHealth::Online,
            "reconnecting" => GatewayHealth::Reconnecting,
            "failed" => GatewayHealth::Failed,
            other => {
                return Err(AdapterError::Unexpected(format!(
                    "unknown gateway health state: {other}"
                )))
            }
        };
        self.gateway
            .health(state, health.detail.clone(), health.uptime_s)
            .await
    }

    async fn register_channel(&mut self, args: &RegisterChannelArgs) -> Result<(), AdapterError> {
        self.register(args.clone()).await
    }

    async fn typing(&mut self, args: &TypingArgs) -> Result<(), AdapterError> {
        self.send_typing(args.conversation.clone(), args.seconds).await
    }
}

pub fn socket_path(server_root: &Path) -> PathBuf {
    server_root.join(".onlyne").join("run").join("s")
}

pub fn spec_path(server_root: &Path) -> PathBuf {
    server_root.join(".onlyne").join("spec.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_proto::{Body, MsgKind, Principal, new_envelope};

    #[test]
    fn paths_are_derived_from_caller_server_root() {
        let root = Path::new("/tmp/server");
        assert_eq!(socket_path(root), PathBuf::from("/tmp/server/.onlyne/run/s"));
        assert_eq!(spec_path(root), PathBuf::from("/tmp/server/.onlyne/spec.toml"));
    }

    #[test]
    fn inbound_delivery_preserves_envelope_id() {
        let envelope = new_envelope(
            MsgKind::Note,
            Principal::gateway("g", "telegram", Some("c".into())),
            Principal::role("planner"),
            Body::text("hello"),
            None,
        )
        .unwrap();
        let delivery = Delivery {
            msg_id: envelope.id.clone(),
            envelope: Box::new(envelope.clone()),
        };
        assert_eq!(delivery.msg_id, envelope.id);
        assert_eq!(delivery.envelope.from, envelope.from);
    }
}
