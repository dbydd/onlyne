use clap::{Parser, ValueEnum};
use onlyne_adapter::{AdapterError, GatewayPlugin};
use onlyne_config::Spec;
use onlyne_gateway::{
    host::{self, Host},
    kit::{
        self, KitError,
        media::{self, MAX_IMAGE_BYTES},
    },
};
use onlyne_net::Backoff;
use onlyne_proto::{
    Body, Capability, Delivery, Envelope, GatewayHealth, HealthArgs, ImagePart, MsgKind, Principal,
    RegisterChannelArgs, RenderSendArgs, TypingArgs, new_envelope, new_task_id,
};
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Platform {
    Telegram,
    Feishu,
    Qqbot,
    Weixin,
}

impl Platform {
    fn as_str(self) -> &'static str {
        match self {
            Platform::Telegram => "telegram",
            Platform::Feishu => "feishu",
            Platform::Qqbot => "qqbot",
            Platform::Weixin => "weixin",
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "onlyne-gateway", about = "Onlyne v1 platform gateway host")]
struct Args {
    #[arg(long, value_enum)]
    platform: Platform,
    #[arg(long)]
    server_root: PathBuf,
    #[arg(long, default_value = "gateway")]
    gateway_id: String,
}

fn main() {
    std::process::exit(match run() {
        Ok(()) => 0,
        Err(code) => code,
    });
}

fn run() -> Result<(), i32> {
    let args = Args::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            eprintln!("onlyne-gateway: runtime unavailable: {err}");
            1
        })?;
    runtime.block_on(serve(args)).map_err(|err| {
        eprintln!("onlyne-gateway: {err}");
        1
    })
}

async fn serve(args: Args) -> Result<(), String> {
    let platform = args.platform.as_str();
    let spec = Spec::load(host::spec_path(&args.server_root)).map_err(report_spec_error)?;
    let entry = spec
        .gateway
        .iter()
        .find(|entry| entry.id == args.gateway_id && entry.platform == platform)
        .ok_or_else(|| {
            format!(
                "gateway {} for platform {platform} is not declared in <server-root>/.onlyne/spec.toml",
                args.gateway_id
            )
        })?;
    if !entry.enabled {
        return Err(format!(
            "gateway {} for platform {platform} is disabled in the spec",
            entry.id
        ));
    }
    let mut plugin = build_plugin(platform, &args.gateway_id)?;
    let routes: Vec<onlyne_config::RouteEntry> = spec
        .route
        .iter()
        .filter(|route| route.gateway == args.gateway_id)
        .cloned()
        .collect();
    let mut backoff = Backoff::new();
    loop {
        match run_once(platform, &args, &routes, plugin.as_mut()).await {
            Ok(returned) => {
                plugin = returned;
                backoff.reset();
            }
            Err(err) => {
                eprintln!("onlyne-gateway: connection failed: {err}; retrying");
                tokio::time::sleep(backoff.next()).await;
                plugin = build_plugin(platform, &args.gateway_id)?;
            }
        }
    }
}

fn report_spec_error(err: onlyne_config::SpecError) -> String {
    format!("cannot load gateway spec: {err}")
}

fn build_plugin(platform: &str, gateway_id: &str) -> Result<Box<dyn GatewayPlugin>, String> {
    match platform {
        #[cfg(feature = "telegram")]
        "telegram" => {
            let token = onlyne_gateway_telegram::auth::resolve_token(None)
                .map_err(|err| missing_credential_text("telegram", &err))?;
            Ok(Box::new(
                onlyne_gateway_telegram::TelegramPlugin::new(token).with_gateway_id(gateway_id),
            ))
        }
        #[cfg(feature = "feishu")]
        "feishu" => {
            let credentials = onlyne_gateway_feishu::auth::FeishuCredentials::from_env()
                .map_err(|err| missing_credential_text("feishu", &err))?;
            Ok(Box::new(onlyne_gateway_feishu::FeishuPlugin::new(
                credentials,
            )))
        }
        #[cfg(feature = "qqbot")]
        "qqbot" => {
            let credentials = onlyne_gateway_qqbot::QqBotCredentials::from_env()
                .map_err(|err| missing_credential_text("qqbot", &err))?;
            Ok(Box::new(onlyne_gateway_qqbot::QqBotPlugin::new(
                credentials,
                false,
            )))
        }
        #[cfg(feature = "weixin")]
        "weixin" => {
            let token = onlyne_gateway_weixin::auth::resolve_token(None, None)
                .map_err(|err| missing_credential_text("weixin", &err))?;
            Ok(Box::new(onlyne_gateway_weixin::WeixinPlugin::new(
                onlyne_gateway_weixin::WeixinConfig {
                    token: Some(token),
                    ..Default::default()
                },
            )))
        }
        other => Err(format!(
            "platform {other} was not compiled into this gateway binary; rebuild with --features {other}"
        )),
    }
}

fn missing_credential_text(platform: &str, err: &AdapterError) -> String {
    match err.code() {
        Some(onlyne_proto::ErrorCode::Unauthorized) => err.to_string(),
        _ => format!("{platform} credentials missing: {err}"),
    }
}

async fn run_once(
    platform: &str,
    args: &Args,
    routes: &[onlyne_config::RouteEntry],
    plugin: &mut dyn GatewayPlugin,
) -> Result<Box<dyn GatewayPlugin>, String> {
    let router = Router::new(
        args.gateway_id.clone(),
        platform.to_string(),
        routes.to_vec(),
    );
    let capabilities = plugin.capabilities();
    let host = Host::connect(
        &host::socket_path(&args.server_root),
        args.gateway_id.clone(),
        platform,
        capabilities,
    )
    .await
    .map_err(|err| err.to_string())?;
    host.register(channel_args(platform, &args.gateway_id))
        .await
        .map_err(|err| err.to_string())?;
    host.send_health(GatewayHealth::Online, None)
        .await
        .map_err(|err| err.to_string())?;
    let shared = Arc::new(Mutex::new(RouterHost::new(router, host)));
    plugin
        .start(&mut PluginHost::new(Arc::clone(&shared)))
        .await
        .map_err(|err| err.to_string())?;
    let started = Instant::now();
    let mut health_tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            op = async {
                let guard = shared.lock().await;
                guard.host.next_host_op().await
            } => {
                match op.map_err(|err| err.to_string())? {
                    onlyne_proto::HostOp::RenderSend(render) => {
                        handle_render_send(platform, &shared, plugin, &render).await?;
                    }
                    onlyne_proto::HostOp::Probe(_) => {
                        report_probe(plugin, &shared).await?;
                    }
                    onlyne_proto::HostOp::Bye(bye) => {
                        plugin.stop(&bye.reason).await.map_err(|err| err.to_string())?;
                        return Err(format!("server closed gateway connection: {}", bye.reason));
                    }
                    _ => {}
                }
            }
            _ = health_tick.tick() => {
                report_probe(plugin, &shared).await?;
                let _ = started;
            }
        }
    }
}

async fn handle_render_send(
    platform: &str,
    shared: &Arc<Mutex<RouterHost>>,
    plugin: &mut dyn GatewayPlugin,
    render: &RenderSendArgs,
) -> Result<(), String> {
    let envelope = render.envelope.as_ref();
    let outbound = outbound_from_envelope(platform, render.conversation.clone(), envelope).await?;
    if plugin.capabilities().contains(&Capability::Typing) {
        let guard = shared.lock().await;
        let _ = guard.host.send_typing(render.conversation.clone(), 3).await;
    }
    let receipt = plugin
        .send(&outbound)
        .await
        .map_err(|err| err.to_string())?;
    let _ = receipt;
    Ok(())
}
async fn report_probe(
    plugin: &mut dyn GatewayPlugin,
    shared: &Arc<Mutex<RouterHost>>,
) -> Result<(), String> {
    let health = plugin.probe().await.map_err(|err| err.to_string())?;
    let guard = shared.lock().await;
    guard
        .host
        .send_health(
            match health.state.as_str() {
                "online" => GatewayHealth::Online,
                "reconnecting" => GatewayHealth::Reconnecting,
                _ => GatewayHealth::Failed,
            },
            health.detail,
        )
        .await
        .map_err(|err| err.to_string())
}

async fn outbound_from_envelope(
    platform: &str,
    conversation: String,
    envelope: &Envelope,
) -> Result<onlyne_adapter::Outbound, String> {
    let text = finished_text(platform, envelope).await?;
    let image = finished_image(envelope).map_err(|err| err.to_string())?;
    Ok(onlyne_adapter::Outbound {
        conversation,
        text,
        image,
        reply_to: envelope
            .causality
            .as_ref()
            .and_then(|chain| chain.reply_to.clone()),
        kind: envelope.kind,
    })
}

async fn finished_text(platform: &str, envelope: &Envelope) -> Result<String, String> {
    let raw = envelope.body.text.clone().unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(raw);
    }
    let segments = kit::markdown::split_tables(&raw);
    let mut parts = Vec::new();
    for segment in segments {
        match segment {
            kit::markdown::MarkdownSegment::Text(text) => parts.push(text),
            kit::markdown::MarkdownSegment::Table(table) => {
                match kit::markdown::render_markdown_table(&table) {
                    Ok(kit::markdown::RenderedMarkdownTable::Png { .. }) => {
                        parts.push(kit::markdown::plain_text_fallback(&table));
                    }
                    Ok(kit::markdown::RenderedMarkdownTable::Text { text, .. }) => parts.push(text),
                    Err(KitError::RenderFailure { detail, .. }) => {
                        return Err(format!("{platform} table render failed: {detail}"));
                    }
                    Err(err) => return Err(err.to_string()),
                }
            }
        }
    }
    Ok(parts.join("\n\n"))
}

fn finished_image(envelope: &Envelope) -> Result<Option<ImagePart>, KitError> {
    let Some(image) = envelope.body.image.clone() else {
        return Ok(None);
    };
    let bytes = image.decode().map_err(|err| {
        KitError::Unsupported(format!("envelope image is not valid base64: {err}"))
    })?;
    media::ensure_image_budget(&bytes)?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(KitError::OversizedPayload {
            max: MAX_IMAGE_BYTES,
            actual: bytes.len(),
        });
    }
    Ok(Some(image))
}

fn channel_args(platform: &str, gateway_id: &str) -> RegisterChannelArgs {
    RegisterChannelArgs {
        platform: platform.to_string(),
        channel: gateway_id.to_string(),
        conversations: None,
    }
}

#[derive(Debug, Clone)]
struct Router {
    gateway_id: String,
    platform: String,
    routes: Vec<onlyne_config::RouteEntry>,
}

impl Router {
    fn new(gateway_id: String, platform: String, routes: Vec<onlyne_config::RouteEntry>) -> Self {
        Self {
            gateway_id,
            platform,
            routes,
        }
    }

    fn route_target(&self, conversation: Option<&str>) -> Option<Principal> {
        for route in &self.routes {
            let channel_matches = route.channel == self.platform;
            let conversation_matches = match (&route.conversation, conversation) {
                (None, _) => true,
                (Some(want), Some(got)) => want == got,
                (Some(_), None) => false,
            };
            if route.gateway == self.gateway_id && channel_matches && conversation_matches {
                return Some(match &route.to.session {
                    Some(session) => {
                        Principal::role_session(route.to.role.clone(), session.clone())
                    }
                    None => Principal::role(route.to.role.clone()),
                });
            }
        }
        None
    }

    fn inbound_envelope(
        &self,
        conversation: &str,
        text: &str,
        external_id: Option<&str>,
    ) -> Result<Envelope, String> {
        let Some(target) = self.route_target(Some(conversation)) else {
            return Err(format!(
                "no [[route]] row matches gateway {} conversation {conversation}",
                self.gateway_id
            ));
        };
        let from = Principal::gateway(
            self.gateway_id.clone(),
            self.platform.clone(),
            Some(conversation.to_string()),
        );
        let kind = if text.trim_start().starts_with("/task ") {
            MsgKind::Task
        } else {
            MsgKind::Note
        };
        let causality = if kind == MsgKind::Task {
            Some(onlyne_proto::Causality::root(new_task_id()))
        } else {
            None
        };
        let mut envelope =
            new_envelope(kind, from, target, Body::text(text.to_string()), causality)
                .map_err(|err| err.to_string())?;
        if let Some(external_id) = external_id {
            let chain = envelope
                .causality
                .get_or_insert(onlyne_proto::Causality::root(new_task_id()));
            chain.reply_to = Some(external_id.to_string());
        }
        Ok(envelope)
    }
}

struct RouterHost {
    router: Router,
    host: Host,
}

impl RouterHost {
    fn new(router: Router, host: Host) -> Self {
        Self { router, host }
    }
}

struct PluginHost {
    shared: Arc<Mutex<RouterHost>>,
}

impl PluginHost {
    fn new(shared: Arc<Mutex<RouterHost>>) -> Self {
        Self { shared }
    }
}

#[async_trait::async_trait]
impl onlyne_adapter::GatewayHost for PluginHost {
    async fn deliver_inbound(&mut self, _envelope: &Envelope) -> Result<(), AdapterError> {
        Err(AdapterError::Unexpected(
            "plugins deliver platform payloads through the host event loop".to_string(),
        ))
    }

    async fn report_health(&mut self, health: &HealthArgs) -> Result<(), AdapterError> {
        let mut guard = self.shared.lock().await;
        guard.host.report_health(health).await
    }

    async fn register_channel(&mut self, args: &RegisterChannelArgs) -> Result<(), AdapterError> {
        let mut guard = self.shared.lock().await;
        guard.host.register_channel(args).await
    }

    async fn typing(&mut self, args: &TypingArgs) -> Result<(), AdapterError> {
        let mut guard = self.shared.lock().await;
        guard.host.typing(args).await
    }
}

fn resolve_target_role() -> Option<String> {
    env::var("ONLYNE_GATEWAY_TARGET_ROLE")
        .ok()
        .filter(|role| !role.trim().is_empty())
}

fn default_gateway_ref(channel: &str, conversation: &str, external_id: &str) -> String {
    format!("gw:{channel}:{conversation}:{external_id}")
}

fn inbound_delivery_for(
    router: &Router,
    conversation: &str,
    text: &str,
    external_id: Option<&str>,
) -> Result<Delivery, String> {
    let envelope = router.inbound_envelope(conversation, text, external_id)?;
    Ok(Delivery {
        msg_id: envelope.id.clone(),
        envelope: Box::new(envelope),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Router {
        Router::new(
            "gw1".to_string(),
            "telegram".to_string(),
            vec![onlyne_config::RouteEntry {
                gateway: "gw1".to_string(),
                channel: "telegram".to_string(),
                conversation: None,
                to: onlyne_config::RouteTarget {
                    role: "planner".to_string(),
                    session: None,
                },
            }],
        )
    }

    #[test]
    fn route_target_resolves_role() {
        let router = router();
        assert_eq!(
            router.route_target(Some("c1")),
            Some(Principal::role("planner"))
        );
    }

    #[test]
    fn inbound_envelope_uses_gateway_principal() {
        let envelope = router()
            .inbound_envelope("c1", "hello", Some("ext-1"))
            .unwrap();
        assert!(matches!(envelope.from, Principal::Gateway { .. }));
        assert_eq!(envelope.to, Principal::role("planner"));
        assert_eq!(envelope.kind, MsgKind::Note);
    }

    #[test]
    fn unmapped_conversation_is_a_clean_error() {
        let router = Router::new("gw1".to_string(), "telegram".to_string(), vec![]);
        let err = router.inbound_envelope("c1", "hello", None).unwrap_err();
        assert!(err.contains("no [[route]] row"));
    }

    #[test]
    fn gateway_ref_round_trip_is_opaque() {
        let reference = default_gateway_ref("telegram", "c1", "ext-1");
        assert!(reference.contains("c1"));
        assert!(reference.contains("ext-1"));
    }

    #[test]
    fn missing_target_role_env_is_none() {
        let _ = resolve_target_role();
    }

    #[tokio::test]
    async fn duplex_pair_drives_inbound_and_render_send() {
        use onlyne_adapter::{AdapterIo, Host, HostDispatcher};
        use onlyne_proto::{DetachArgs, ErrorCode, HealthArgs, Receipt, SessionRegisterArgs};
        use std::time::Duration;

        struct FakeGatewayHost;

        #[async_trait::async_trait]
        impl Host for FakeGatewayHost {
            async fn hello(
                &self,
                _args: &onlyne_proto::HelloArgs,
            ) -> std::result::Result<onlyne_proto::HelloAck, (ErrorCode, String)> {
                Ok(onlyne_proto::HelloAck {
                    protocol: onlyne_proto::PROTOCOL_VERSION,
                    role: "gateway".to_string(),
                    session_id: None,
                    generation: 1,
                    prose: "gateway".to_string(),
                    server: onlyne_proto::ServerInfo {
                        connected: true,
                        cluster: "local".to_string(),
                        name: "server".to_string(),
                    },
                    host_capabilities: vec![],
                })
            }

            async fn deliver(
                &self,
                delivery: &onlyne_proto::Delivery,
            ) -> std::result::Result<(), (ErrorCode, String)> {
                assert_eq!(
                    delivery.envelope.body.text.as_deref(),
                    Some("hello inbound")
                );
                Ok(())
            }

            async fn register_channel(
                &self,
                _args: &onlyne_proto::RegisterChannelArgs,
            ) -> std::result::Result<(), (ErrorCode, String)> {
                Ok(())
            }

            async fn health(
                &self,
                _args: &HealthArgs,
            ) -> std::result::Result<(), (ErrorCode, String)> {
                Ok(())
            }

            async fn typing(
                &self,
                _args: &onlyne_proto::TypingArgs,
            ) -> std::result::Result<(), (ErrorCode, String)> {
                Ok(())
            }

            async fn send(
                &self,
                _envelope: &Envelope,
            ) -> std::result::Result<Receipt, (ErrorCode, String)> {
                Err((ErrorCode::UnknownOp, "send is unsupported".to_string()))
            }

            async fn report(
                &self,
                _report: &onlyne_proto::Report,
            ) -> std::result::Result<(), (ErrorCode, String)> {
                Err((ErrorCode::UnknownOp, "report is unsupported".to_string()))
            }

            async fn session_register(
                &self,
                _args: &SessionRegisterArgs,
            ) -> std::result::Result<(), (ErrorCode, String)> {
                Err((
                    ErrorCode::UnknownOp,
                    "session_register is unsupported".to_string(),
                ))
            }

            async fn detach(
                &self,
                _args: &DetachArgs,
            ) -> std::result::Result<(), (ErrorCode, String)> {
                Ok(())
            }
        }

        let (client, server) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            let mut server = server;
            let connection = onlyne_adapter::AdapterServer::accept(server, |hello| {
                assert_eq!(hello.kind, onlyne_proto::MountKind::Gateway);
                Ok(onlyne_proto::HelloAck {
                    protocol: onlyne_proto::PROTOCOL_VERSION,
                    role: "gateway".to_string(),
                    session_id: None,
                    generation: 1,
                    prose: "gateway".to_string(),
                    server: onlyne_proto::ServerInfo {
                        connected: true,
                        cluster: "local".to_string(),
                        name: "server".to_string(),
                    },
                    host_capabilities: vec![],
                })
            })
            .await
            .unwrap();
            let dispatcher = HostDispatcher::new(
                onlyne_proto::MountKind::Gateway,
                std::sync::Arc::new(FakeGatewayHost),
            );
            dispatcher
                .serve(connection.io, connection.inbound)
                .await
                .unwrap();
        });
        let gateway = onlyne_adapter::AdapterClient::gateway(client);
        gateway
            .hello_gateway("gw1", "fake", vec![Capability::Typing])
            .await
            .unwrap();
        let delivery =
            inbound_delivery_for(&router(), "c1", "hello inbound", Some("ext-1")).unwrap();
        gateway.deliver_inbound(delivery.clone()).await.unwrap();
        let render = RenderSendArgs {
            envelope: delivery.envelope,
            conversation: "c1".to_string(),
            gateway_ref: Some(default_gateway_ref("telegram", "c1", "ext-1")),
        };
        let outbound =
            outbound_from_envelope("telegram", render.conversation.clone(), &render.envelope)
                .await
                .unwrap();
        assert_eq!(outbound.conversation, "c1");
        assert!(outbound.text.contains("hello inbound"));
        server_task.abort();
    }

    #[test]
    fn platform_names_are_stable() {
        assert_eq!(Platform::Telegram.as_str(), "telegram");
        assert_eq!(Platform::Feishu.as_str(), "feishu");
        assert_eq!(Platform::Qqbot.as_str(), "qqbot");
        assert_eq!(Platform::Weixin.as_str(), "weixin");
    }

    #[test]
    fn socket_and_spec_paths_use_server_root() {
        let root = Path::new("/tmp/server");
        assert_eq!(
            host::socket_path(root),
            PathBuf::from("/tmp/server/.onlyne/run/s")
        );
        assert_eq!(
            host::spec_path(root),
            PathBuf::from("/tmp/server/.onlyne/spec.toml")
        );
    }
}
