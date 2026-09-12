//! `onlyne-gateway` — one process per platform, one host around one plugin.
//!
//! Verbs: `run <platform>`, `list`, `status [<platform>]`, `auth <platform>`.
//! The host owns the adapter socket, the spec route table, and the local
//! `gateway_ref` correlation table; the plugin owns platform SDK calls.

use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use onlyne_adapter::{AdapterError, GatewayPlugin};
use onlyne_config::{GatewayEntry, Spec};
use onlyne_gateway::{
    host::{self, Host},
    kit::{self, KitError, media},
    refs::{self, GatewayRef, GatewayRefStore},
};
use onlyne_net::Backoff;
use onlyne_proto::{
    Capability, Envelope, ErrorCode, GatewayHealth, HostOp, ImagePart, Principal,
    RegisterChannelArgs, RenderSendArgs, TypingArgs,
};
use serde_json::{Value, json};
use std::{
    fmt,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
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
    /// Server root holding `.onlyne/spec.toml` and `.onlyne/run/s`.
    #[arg(long, global = true)]
    server_root: Option<PathBuf>,
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Debug, Subcommand)]
enum Verb {
    /// Serve one platform until the server closes the connection.
    Run(RunArgs),
    /// List the `[[gateway]]` entries declared in the spec.
    List,
    /// Report local status: declarations, capabilities, credentials.
    Status(StatusArgs),
    /// Print the platform onboarding prompt.
    Auth(AuthArgs),
}

#[derive(Debug, ClapArgs)]
struct RunArgs {
    platform: Platform,
    /// Gateway id from `[[gateway]]`. Defaults to the only entry for the platform.
    #[arg(long)]
    gateway_id: Option<String>,
    /// Credential source: a literal token, or `$ENV_NAME`.
    #[arg(long)]
    token: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct StatusArgs {
    platform: Option<Platform>,
}

#[derive(Debug, ClapArgs)]
struct AuthArgs {
    platform: Platform,
    /// Credential source: a literal token, or `$ENV_NAME`.
    #[arg(long)]
    token: Option<String>,
}

/// A failure with the exit code the contract assigns to it.
#[derive(Debug)]
struct VerbError {
    code: u8,
    message: String,
}

impl VerbError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }

    fn runtime(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }
}

impl fmt::Display for VerbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    match dispatch(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("onlyne-gateway: {err}");
            ExitCode::from(err.code)
        }
    }
}

fn dispatch(args: Args) -> Result<(), VerbError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| VerbError::runtime(format!("runtime unavailable: {err}")))?;
    match &args.verb {
        Verb::Run(run) => {
            let root = server_root(&args)?;
            let spec = load_spec(&root)?;
            let entry = select_gateway(
                &spec.gateway,
                run.platform.as_str(),
                run.gateway_id.as_deref(),
            )?;
            let entry = entry.clone();
            if !entry.enabled {
                return Err(VerbError::usage(format!(
                    "gateway {} for platform {} is disabled in the spec",
                    entry.id, entry.platform
                )));
            }
            let routes: Vec<onlyne_config::RouteEntry> = spec
                .route
                .iter()
                .filter(|route| route.gateway == entry.id)
                .cloned()
                .collect();
            runtime.block_on(serve(&root, &entry, routes, run.token.as_deref()))
        }
        Verb::List => {
            let root = server_root(&args)?;
            let spec = load_spec(&root)?;
            let entries: Vec<Value> = spec
                .gateway
                .iter()
                .map(|entry| {
                    json!({
                        "id": entry.id,
                        "platform": entry.platform,
                        "enabled": entry.enabled,
                        "compiled": compiled(&entry.platform),
                    })
                })
                .collect();
            print_json(&json!({ "ok": true, "data": { "gateways": entries } }));
            Ok(())
        }
        Verb::Status(status) => {
            let root = server_root(&args)?;
            let spec = load_spec(&root)?;
            let want = status.platform.map(Platform::as_str);
            let rows: Vec<Value> = spec
                .gateway
                .iter()
                .filter(|entry| want.is_none_or(|platform| platform == entry.platform))
                .map(|entry| status_row(entry).to_value())
                .collect();
            print_json(&json!({ "ok": true, "data": { "gateways": rows } }));
            Ok(())
        }
        Verb::Auth(auth) => {
            let platform = auth.platform.as_str();
            let mut plugin = build_plugin(
                platform,
                "gateway",
                auth.token.as_deref(),
                CredentialSource::Placeholder,
            )
            .map_err(|err| err.into_verb_error())?;
            let prompt = plugin
                .gateway()
                .onboarding()
                .map_err(|err| VerbError::runtime(err.to_string()))?
                .ok_or_else(|| {
                    VerbError::runtime(format!(
                        "platform {platform} has no onboarding flow; set its credentials in the environment"
                    ))
                })?;
            let kind = match prompt.kind {
                onlyne_adapter::OnboardingKind::Qr => "qr",
                onlyne_adapter::OnboardingKind::ManualCode => "manual_code",
            };
            if prompt.kind == onlyne_adapter::OnboardingKind::Qr {
                match kit::onboarding::render_login_qr_ascii(&prompt.payload) {
                    Ok(art) => eprintln!("{art}"),
                    Err(err) => eprintln!("onlyne-gateway: cannot render the QR payload: {err}"),
                }
            }
            print_json(&json!({
                "ok": true,
                "data": {
                    "platform": platform,
                    "kind": kind,
                    "payload": prompt.payload,
                    "expires_in": prompt.expires_in,
                }
            }));
            Ok(())
        }
    }
}

fn server_root(args: &Args) -> Result<PathBuf, VerbError> {
    args.server_root.clone().ok_or_else(|| {
        VerbError::usage(
            "--server-root <dir> is required; it names the directory holding .onlyne/spec.toml",
        )
    })
}

fn load_spec(root: &Path) -> Result<Spec, VerbError> {
    Spec::load(host::spec_path(root))
        .map_err(|err| VerbError::runtime(format!("cannot load gateway spec: {err}")))
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string(value).expect("status value is serialisable")
    );
}

/// Pick the gateway entry this process serves.
fn select_gateway<'a>(
    entries: &'a [GatewayEntry],
    platform: &str,
    requested: Option<&str>,
) -> Result<&'a GatewayEntry, VerbError> {
    let candidates: Vec<&GatewayEntry> = entries
        .iter()
        .filter(|entry| entry.platform == platform)
        .filter(|entry| requested.is_none_or(|id| entry.id == id))
        .collect();
    match candidates.as_slice() {
        [entry] => Ok(entry),
        [] => Err(VerbError::usage(format!(
            "gateway {} for platform {platform} is not declared in the spec",
            requested.unwrap_or("<auto>")
        ))),
        many => {
            let ids: Vec<&str> = many.iter().map(|entry| entry.id.as_str()).collect();
            Err(VerbError::usage(format!(
                "platform {platform} has several gateways ({}); pass --gateway-id",
                ids.join(", ")
            )))
        }
    }
}

/// Whether the platform plugin is linked into this binary.
fn compiled(platform: &str) -> bool {
    build_plugin(platform, "gateway", None, CredentialSource::Placeholder).is_ok()
}

/// Local status for one `[[gateway]]` entry.
struct StatusRow {
    id: String,
    platform: String,
    enabled: bool,
    compiled: bool,
    configured: bool,
    capabilities: Vec<&'static str>,
    conversations: &'static str,
    detail: Option<String>,
}

impl StatusRow {
    fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "platform": self.platform,
            "enabled": self.enabled,
            "compiled": self.compiled,
            "configured": self.configured,
            "capabilities": self.capabilities,
            "conversations": self.conversations,
            "detail": self.detail,
        })
    }
}

/// Status is a local view: declarations, linked plugin, declared capabilities,
/// and whether the plugin can resolve credentials in this process.
fn status_row(entry: &GatewayEntry) -> StatusRow {
    let mut row = StatusRow {
        id: entry.id.clone(),
        platform: entry.platform.clone(),
        enabled: entry.enabled,
        compiled: false,
        configured: false,
        capabilities: Vec::new(),
        conversations: "unsupported",
        detail: None,
    };
    let mut plugin = match build_plugin(
        &entry.platform,
        &entry.id,
        None,
        CredentialSource::Placeholder,
    ) {
        Ok(plugin) => plugin,
        Err(err) => {
            row.detail = Some(err.message());
            return row;
        }
    };
    row.compiled = true;
    row.capabilities = plugin
        .gateway()
        .capabilities()
        .into_iter()
        .map(Capability::as_str)
        .collect();
    if row
        .capabilities
        .contains(&Capability::Conversations.as_str())
    {
        row.conversations = "supported";
    }
    match build_plugin(&entry.platform, &entry.id, None, CredentialSource::Env) {
        Ok(_) => row.configured = true,
        Err(err) => row.detail = Some(err.message()),
    }
    row
}

/// Where credentials come from when a plugin is constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialSource {
    /// Resolve the real platform credentials, failing when they are absent.
    Env,
    /// Build with placeholder values, used to read capabilities and prompts.
    Placeholder,
}

/// Why a plugin could not be constructed.
#[derive(Debug)]
enum BuildError {
    Credentials(AdapterError),
    Unlinked(String),
}

impl BuildError {
    fn message(&self) -> String {
        match self {
            Self::Credentials(err) => err.to_string(),
            Self::Unlinked(detail) => detail.clone(),
        }
    }

    fn into_verb_error(self) -> VerbError {
        match self {
            Self::Credentials(err) => VerbError::runtime(err.to_string()),
            Self::Unlinked(detail) => VerbError::usage(detail),
        }
    }
}

/// The plugin linked into this process, plus the calls outside the
/// `GatewayPlugin` trait that the host dispatches by hand.
enum ActivePlugin {
    #[cfg(feature = "telegram")]
    Telegram(Box<onlyne_gateway_telegram::TelegramPlugin>),
    #[cfg(feature = "feishu")]
    Feishu(Box<onlyne_gateway_feishu::FeishuPlugin>),
    #[cfg(feature = "qqbot")]
    Qqbot(Box<onlyne_gateway_qqbot::QqBotPlugin>),
    #[cfg(feature = "weixin")]
    Weixin(Box<onlyne_gateway_weixin::WeixinPlugin>),
}

impl ActivePlugin {
    /// The trait object behind the linked plugin.
    fn gateway(&mut self) -> &mut dyn GatewayPlugin {
        match self {
            #[cfg(feature = "telegram")]
            ActivePlugin::Telegram(plugin) => plugin.as_mut(),
            #[cfg(feature = "feishu")]
            ActivePlugin::Feishu(plugin) => plugin.as_mut(),
            #[cfg(feature = "qqbot")]
            ActivePlugin::Qqbot(plugin) => plugin.as_mut(),
            #[cfg(feature = "weixin")]
            ActivePlugin::Weixin(plugin) => plugin.as_mut(),
            // With no platform feature the enum has no variant to match.
            #[cfg(not(any(
                feature = "telegram",
                feature = "feishu",
                feature = "qqbot",
                feature = "weixin"
            )))]
            _ => match *self {},
        }
    }

    fn typing_declared(&mut self) -> bool {
        self.gateway().capabilities().contains(&Capability::Typing)
    }

    /// Show or hide the platform typing indicator for one conversation.
    ///
    /// A plugin that does not declare `Capability::Typing` answers through the
    /// same absence path an unsupported host op uses.
    async fn set_typing(&mut self, conversation: &str, on: bool) -> Result<(), AdapterError> {
        match self {
            #[cfg(feature = "telegram")]
            ActivePlugin::Telegram(plugin) => plugin.set_typing(conversation, on).await,
            #[cfg(feature = "feishu")]
            ActivePlugin::Feishu(_) => Err(typing_absent(conversation, on)),
            #[cfg(feature = "qqbot")]
            ActivePlugin::Qqbot(_) => Err(typing_absent(conversation, on)),
            #[cfg(feature = "weixin")]
            ActivePlugin::Weixin(plugin) => plugin.set_typing(conversation, on).await,
            #[cfg(not(any(
                feature = "telegram",
                feature = "feishu",
                feature = "qqbot",
                feature = "weixin"
            )))]
            _ => {
                let _ = (conversation, on);
                match *self {}
            }
        }
    }
}

/// The one wording for an op this mount does not serve.
fn unsupported_host_op(op: &str) -> String {
    format!("gateway received unsupported host op {op}")
}

/// The refusal a mount gives when it holds no typing capability.
#[cfg(any(feature = "feishu", feature = "qqbot"))]
fn typing_absent(conversation: &str, on: bool) -> AdapterError {
    AdapterError::Unexpected(format!(
        "{} for conversation {conversation} (on: {on})",
        unsupported_host_op("typing")
    ))
}

fn build_plugin(
    platform: &str,
    gateway_id: &str,
    token: Option<&str>,
    source: CredentialSource,
) -> Result<ActivePlugin, BuildError> {
    #[cfg(not(any(
        feature = "telegram",
        feature = "feishu",
        feature = "qqbot",
        feature = "weixin"
    )))]
    let _ = (token, source);
    if gateway_id.trim().is_empty() {
        return Err(BuildError::Credentials(AdapterError::new(
            ErrorCode::Invalid,
            "gateway id must not be empty",
        )));
    }
    match platform {
        #[cfg(feature = "telegram")]
        "telegram" => {
            let token = match source {
                CredentialSource::Placeholder => String::new(),
                CredentialSource::Env => onlyne_gateway_telegram::auth::resolve_token(token)
                    .map_err(BuildError::Credentials)?,
            };
            Ok(ActivePlugin::Telegram(Box::new(
                onlyne_gateway_telegram::TelegramPlugin::new(token).with_gateway_id(gateway_id),
            )))
        }
        #[cfg(feature = "feishu")]
        "feishu" => {
            let credentials = match source {
                CredentialSource::Placeholder => {
                    onlyne_gateway_feishu::auth::FeishuCredentials::new(
                        "",
                        "",
                        onlyne_gateway_feishu::auth::DEFAULT_DOMAIN,
                    )
                }
                CredentialSource::Env if token.is_none() => {
                    onlyne_gateway_feishu::auth::FeishuCredentials::from_env()
                        .map_err(BuildError::Credentials)?
                }
                CredentialSource::Env => {
                    return Err(BuildError::Credentials(AdapterError::new(
                        ErrorCode::Invalid,
                        format!(
                            "--token is not supported for feishu; set {} and {}",
                            onlyne_gateway_feishu::auth::APP_ID_ENV,
                            onlyne_gateway_feishu::auth::APP_SECRET_ENV
                        ),
                    )));
                }
            };
            Ok(ActivePlugin::Feishu(Box::new(
                onlyne_gateway_feishu::FeishuPlugin::new(credentials),
            )))
        }
        #[cfg(feature = "qqbot")]
        "qqbot" => {
            let credentials = match source {
                CredentialSource::Placeholder => onlyne_gateway_qqbot::QqBotCredentials {
                    app_id: String::new(),
                    app_secret: String::new(),
                },
                CredentialSource::Env if token.is_none() => {
                    onlyne_gateway_qqbot::QqBotCredentials::from_env()
                        .map_err(BuildError::Credentials)?
                }
                CredentialSource::Env => {
                    return Err(BuildError::Credentials(AdapterError::new(
                        ErrorCode::Invalid,
                        format!(
                            "--token is not supported for qqbot; set {} and {}",
                            onlyne_gateway_qqbot::APP_ID_ENV,
                            onlyne_gateway_qqbot::APP_SECRET_ENV
                        ),
                    )));
                }
            };
            Ok(ActivePlugin::Qqbot(Box::new(
                onlyne_gateway_qqbot::QqBotPlugin::new(credentials, false),
            )))
        }
        #[cfg(feature = "weixin")]
        "weixin" => {
            let token = match source {
                CredentialSource::Placeholder => None,
                CredentialSource::Env => Some(
                    onlyne_gateway_weixin::auth::resolve_token(token, None)
                        .map_err(BuildError::Credentials)?,
                ),
            };
            Ok(ActivePlugin::Weixin(Box::new(
                onlyne_gateway_weixin::WeixinPlugin::new(onlyne_gateway_weixin::WeixinConfig {
                    token,
                    ..Default::default()
                }),
            )))
        }
        other => Err(BuildError::Unlinked(format!(
            "platform {other} was not compiled into this gateway binary; rebuild with --features {other}"
        ))),
    }
}

#[cfg(feature = "telegram")]
fn telegram_ref(value: &str) -> Option<GatewayRef> {
    let parsed = onlyne_gateway_telegram::parse_gateway_ref(value).ok()?;
    let has_scene = !parsed.scene.trim().is_empty();
    Some(GatewayRef::new(
        parsed.channel,
        parsed.conversation,
        parsed.external_id,
        has_scene.then_some(parsed.scene),
    ))
}

#[cfg(feature = "feishu")]
fn feishu_ref(value: &str) -> Option<GatewayRef> {
    let parsed = onlyne_gateway_feishu::parse_gateway_ref(value)?;
    Some(GatewayRef::new(
        parsed.channel,
        parsed.conversation,
        parsed.external_id,
        parsed.scene.filter(|scene| !scene.trim().is_empty()),
    ))
}

#[cfg(feature = "qqbot")]
fn qqbot_ref(value: &str) -> Option<GatewayRef> {
    let parsed = onlyne_gateway_qqbot::parse_gateway_ref(value).ok()?;
    let scene = serde_json::to_value(parsed.scene)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned));
    Some(GatewayRef::new(
        parsed.channel,
        parsed.conversation,
        parsed.external_id,
        scene,
    ))
}

#[cfg(feature = "weixin")]
fn weixin_ref(value: &str) -> Option<GatewayRef> {
    let parsed = onlyne_gateway_weixin::GatewayRef::decode(value).ok()?;
    Some(GatewayRef::new(
        parsed.channel,
        parsed.conversation,
        parsed.external_id.unwrap_or_default(),
        parsed.scene.filter(|scene| !scene.trim().is_empty()),
    ))
}

/// Decode a plugin handle into the four correlation columns.
fn ref_codec(platform: &str) -> Option<fn(&str) -> Option<GatewayRef>> {
    match platform {
        #[cfg(feature = "telegram")]
        "telegram" => Some(telegram_ref),
        #[cfg(feature = "feishu")]
        "feishu" => Some(feishu_ref),
        #[cfg(feature = "qqbot")]
        "qqbot" => Some(qqbot_ref),
        #[cfg(feature = "weixin")]
        "weixin" => Some(weixin_ref),
        _ => None,
    }
}

fn db_path(server_root: &Path, gateway_id: &str) -> PathBuf {
    server_root
        .join(".onlyne")
        .join("gateway")
        .join(format!("{}.db", file_component(gateway_id)))
}

/// Gateway ids name a database file, so path separators never survive.
fn file_component(gateway_id: &str) -> String {
    let cleaned: String = gateway_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "gateway".to_string()
    } else {
        trimmed.to_string()
    }
}

async fn serve(
    server_root: &Path,
    entry: &GatewayEntry,
    routes: Vec<onlyne_config::RouteEntry>,
    token: Option<&str>,
) -> Result<(), VerbError> {
    let platform = entry.platform.as_str();
    let mut plugin = match build_plugin(platform, &entry.id, token, CredentialSource::Env) {
        Ok(plugin) => plugin,
        Err(BuildError::Credentials(err)) => {
            report_unconfigured(server_root, entry, &err).await;
            return Err(VerbError::runtime(err.to_string()));
        }
        Err(err @ BuildError::Unlinked(_)) => return Err(err.into_verb_error()),
    };
    let mut backoff = Backoff::new();
    loop {
        match run_once(server_root, entry, &routes, &mut plugin).await {
            Ok(returned) => {
                plugin = returned;
                backoff.reset();
            }
            Err(err) => {
                eprintln!("onlyne-gateway: connection failed: {err}; retrying");
                tokio::time::sleep(backoff.next()).await;
                plugin = build_plugin(platform, &entry.id, token, CredentialSource::Env)
                    .map_err(BuildError::into_verb_error)?;
            }
        }
    }
}

/// Tell the server this gateway cannot serve, so it records
/// `fault{kind:"gateway_unconfigured"}`. Absent server socket: stay silent.
async fn report_unconfigured(server_root: &Path, entry: &GatewayEntry, err: &AdapterError) {
    let socket = host::socket_path(server_root);
    let connect = Host::connect(
        &socket,
        entry.id.clone(),
        entry.platform.clone(),
        Vec::new(),
    );
    let Ok(host) = tokio::time::timeout(Duration::from_secs(2), connect)
        .await
        .unwrap_or_else(|_| Err(AdapterError::Unexpected("connect timed out".to_string())))
    else {
        return;
    };
    let _ = host
        .register(channel_args(&entry.platform, &entry.id))
        .await;
    let _ = host
        .send_health(GatewayHealth::Failed, Some(err.to_string()))
        .await;
}

/// The platform this mount may serve.
///
/// A binary links one platform's plugin, so a spec entry from another platform
/// is refused here with both names in the detail.
fn mounted_platform(
    plugin_platform: &'static str,
    entry_platform: &str,
) -> Result<&'static str, String> {
    if plugin_platform == entry_platform {
        Ok(plugin_platform)
    } else {
        Err(format!(
            "refusing mount: this binary links the {plugin_platform} plugin and the spec entry declares platform {entry_platform}"
        ))
    }
}

async fn run_once(
    server_root: &Path,
    entry: &GatewayEntry,
    routes: &[onlyne_config::RouteEntry],
    plugin: &mut ActivePlugin,
) -> Result<ActivePlugin, String> {
    let platform = mounted_platform(plugin.gateway().platform(), entry.platform.as_str())?;
    let router = Router::new(entry.id.clone(), platform.to_string(), routes.to_vec());
    let refs =
        GatewayRefStore::open(&db_path(server_root, &entry.id)).map_err(|err| err.to_string())?;
    let capabilities = plugin.gateway().capabilities();
    let host = Host::connect(
        &host::socket_path(server_root),
        entry.id.clone(),
        platform,
        capabilities,
    )
    .await
    .map_err(|err| err.to_string())?;
    host.register(channel_args(platform, &entry.id))
        .await
        .map_err(|err| err.to_string())?;
    host.send_health(GatewayHealth::Online, None)
        .await
        .map_err(|err| err.to_string())?;
    let shared = Arc::new(Mutex::new(RouterHost::new(router, host, refs)));
    plugin
        .gateway()
        .start(&mut PluginHost::new(Arc::clone(&shared)))
        .await
        .map_err(|err| err.to_string())?;
    let mut health_tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            op = async {
                let guard = shared.lock().await;
                guard.host.next_host_op().await
            } => {
                match op.map_err(|err| err.to_string())? {
                    HostOp::RenderSend(render) => {
                        handle_render_send(platform, &shared, plugin, &render).await?;
                    }
                    HostOp::Probe(_) => {
                        report_probe(plugin, &shared).await?;
                    }
                    HostOp::Bye(bye) => {
                        plugin
                            .gateway()
                            .stop(&bye.reason)
                            .await
                            .map_err(|err| err.to_string())?;
                        return Err(format!("server closed gateway connection: {}", bye.reason));
                    }
                    other => {
                        return Err(unsupported_host_op(other.name()));
                    }
                }
            }
            _ = health_tick.tick() => {
                report_probe(plugin, &shared).await?;
            }
        }
    }
}

async fn handle_render_send(
    platform: &str,
    shared: &Arc<Mutex<RouterHost>>,
    plugin: &mut ActivePlugin,
    render: &RenderSendArgs,
) -> Result<(), String> {
    let reply_to = {
        let guard = shared.lock().await;
        resolve_reply_target(
            &guard.refs,
            platform,
            &render.conversation,
            render.gateway_ref.as_deref(),
            render.reply_to.as_deref(),
        )
        .map_err(|err| err.to_string())?
    };
    let outbound = outbound_from_envelope(
        platform,
        render.conversation.clone(),
        reply_to,
        render.envelope.as_ref(),
    )
    .await?;
    let typing = plugin.typing_declared();
    if typing {
        plugin
            .set_typing(&render.conversation, true)
            .await
            .map_err(|err| err.to_string())?;
        let guard = shared.lock().await;
        let _ = guard
            .host
            .send_typing(render.conversation.clone(), true)
            .await;
    }
    let receipt = plugin
        .gateway()
        .send(&outbound)
        .await
        .map_err(|err| err.to_string())?;
    if receipt.external_id.trim().is_empty() {
        return Err(format!("{platform} send returned an empty external_id"));
    }
    if typing {
        plugin
            .set_typing(&render.conversation, false)
            .await
            .map_err(|err| err.to_string())?;
        let guard = shared.lock().await;
        let _ = guard
            .host
            .send_typing(render.conversation.clone(), false)
            .await;
    }
    Ok(())
}

/// The platform message id an outbound message answers.
///
/// Precedence: the frame's own `reply_to`, then the handle it names, then the
/// newest row on record for that conversation.  A conversation with no history
/// stays unthreaded.
fn resolve_reply_target(
    refs: &GatewayRefStore,
    channel: &str,
    conversation: &str,
    gateway_ref: Option<&str>,
    reply_to: Option<&str>,
) -> Result<Option<String>, refs::RefError> {
    if let Some(reply_to) = reply_to.filter(|reply_to| !reply_to.trim().is_empty()) {
        return Ok(Some(reply_to.to_string()));
    }
    let row = match gateway_ref {
        Some(handle) => refs.resolve(handle)?,
        None => refs.newest_for_conversation(channel, conversation)?,
    };
    Ok(row
        .map(|row| row.external_id)
        .filter(|external_id| !external_id.trim().is_empty()))
}

async fn report_probe(
    plugin: &mut ActivePlugin,
    shared: &Arc<Mutex<RouterHost>>,
) -> Result<(), String> {
    let health = plugin
        .gateway()
        .probe()
        .await
        .map_err(|err| err.to_string())?;
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
    reply_to: Option<String>,
    envelope: &Envelope,
) -> Result<onlyne_adapter::Outbound, String> {
    let text = finished_text(platform, envelope).await?;
    let image = finished_image(envelope).map_err(|err| err.to_string())?;
    Ok(onlyne_adapter::Outbound {
        conversation,
        text,
        image,
        reply_to,
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
}

struct RouterHost {
    router: Router,
    host: Host,
    refs: GatewayRefStore,
}

impl RouterHost {
    fn new(router: Router, host: Host, refs: GatewayRefStore) -> Self {
        Self { router, host, refs }
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

/// Resolve the route for one inbound envelope, then record its correlation row.
fn route_inbound(
    router: &Router,
    refs: &mut GatewayRefStore,
    envelope: &Envelope,
) -> Result<Envelope, AdapterError> {
    let conversation = match &envelope.from {
        Principal::Gateway {
            conversation: Some(conversation),
            ..
        } => conversation.clone(),
        _ => {
            return Err(AdapterError::new(
                ErrorCode::Invalid,
                "gateway inbound envelope must carry Principal::Gateway with a conversation",
            ));
        }
    };
    let target = router.route_target(Some(&conversation)).ok_or_else(|| {
        AdapterError::new(
            ErrorCode::UnknownRole,
            format!(
                "no [[route]] row matches gateway {} conversation {conversation}",
                router.gateway_id
            ),
        )
    })?;
    let mut routed = envelope.clone();
    routed.to = target;
    if let Some(handle) = routed
        .causality
        .as_ref()
        .and_then(|chain| chain.reply_to.clone())
    {
        if let Some(codec) = ref_codec(&router.platform) {
            if let Some(row) = codec(&handle) {
                refs.record(&handle, &row)
                    .map_err(|err| AdapterError::Unexpected(err.to_string()))?;
            }
        }
    }
    Ok(routed)
}

#[async_trait::async_trait]
impl onlyne_adapter::GatewayHost for PluginHost {
    async fn deliver_inbound(&mut self, envelope: &Envelope) -> Result<(), AdapterError> {
        let mut guard = self.shared.lock().await;
        let RouterHost { router, host, refs } = &mut *guard;
        let routed = route_inbound(router, refs, envelope)?;
        host.deliver(&routed).await
    }

    async fn report_health(
        &mut self,
        health: &onlyne_proto::HealthArgs,
    ) -> Result<(), AdapterError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_proto::{Body, MsgKind, new_envelope};
    #[cfg(feature = "telegram")]
    use onlyne_proto::{Causality, new_task_id};

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

    fn inbound(text: &str) -> Envelope {
        new_envelope(
            MsgKind::Note,
            Principal::gateway("gw1", "telegram", Some("c1".to_string())),
            Principal::role("unrouted"),
            Body::text(text),
            None,
        )
        .unwrap()
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
    fn routed_inbound_lands_on_the_spec_target() {
        let mut refs = GatewayRefStore::open_in_memory().unwrap();
        let routed = route_inbound(&router(), &mut refs, &inbound("hello")).unwrap();
        assert_eq!(routed.to, Principal::role("planner"));
        assert_eq!(routed.body.text.as_deref(), Some("hello"));
    }

    #[test]
    fn unmapped_conversation_is_a_clean_error() {
        let mut refs = GatewayRefStore::open_in_memory().unwrap();
        let empty = Router::new("gw1".to_string(), "telegram".to_string(), vec![]);
        let err = route_inbound(&empty, &mut refs, &inbound("hello")).unwrap_err();
        assert!(err.to_string().contains("no [[route]] row"));
    }

    #[test]
    fn inbound_without_a_gateway_conversation_is_refused() {
        let mut refs = GatewayRefStore::open_in_memory().unwrap();
        let envelope = new_envelope(
            MsgKind::Note,
            Principal::role("planner"),
            Principal::role("planner"),
            Body::text("hello"),
            None,
        )
        .unwrap();
        let err = route_inbound(&router(), &mut refs, &envelope).unwrap_err();
        assert!(err.to_string().contains("Principal::Gateway"));
    }

    #[cfg(feature = "telegram")]
    #[test]
    fn inbound_handle_is_recorded_for_the_reply_path() {
        let mut refs = GatewayRefStore::open_in_memory().unwrap();
        let handle = onlyne_gateway_telegram::gateway_ref("telegram", "c1", "ext-1", "private");
        let envelope = new_envelope(
            MsgKind::Task,
            Principal::gateway("gw1", "telegram", Some("c1".to_string())),
            Principal::role("unrouted"),
            Body::text("/task ship it"),
            Some(Causality {
                task: new_task_id(),
                parent_task: None,
                reply_to: Some(handle.clone()),
                hop: 0,
                attempt: 0,
            }),
        )
        .unwrap();
        let routed = route_inbound(&router(), &mut refs, &envelope).unwrap();
        assert_eq!(
            routed.causality.unwrap().reply_to.as_deref(),
            Some(handle.as_str())
        );
        let row = refs.resolve(&handle).unwrap().expect("row recorded");
        assert_eq!(row.conversation, "c1");
        assert_eq!(row.external_id, "ext-1");
        assert_eq!(row.scene.as_deref(), Some("private"));
    }

    #[test]
    fn several_gateway_entries_for_one_platform_need_an_id() {
        let entries = vec![
            GatewayEntry {
                id: "tg1".to_string(),
                platform: "telegram".to_string(),
                key: "ed25519/x".to_string(),
                enabled: true,
            },
            GatewayEntry {
                id: "tg2".to_string(),
                platform: "telegram".to_string(),
                key: "ed25519/y".to_string(),
                enabled: true,
            },
        ];
        let err = select_gateway(&entries, "telegram", None).unwrap_err();
        assert!(err.to_string().contains("tg1, tg2"));
        let picked = select_gateway(&entries, "telegram", Some("tg2")).unwrap();
        assert_eq!(picked.id, "tg2");
    }

    #[test]
    fn missing_gateway_entry_is_a_usage_error() {
        let err = select_gateway(&[], "telegram", None).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.to_string().contains("is not declared"));
    }

    #[test]
    fn gateway_ids_with_separators_map_to_one_database_name() {
        assert_eq!(file_component("tg/../1"), "tg_.._1");
        assert_eq!(file_component(".."), "gateway");
        assert_eq!(
            db_path(Path::new("/tmp/server"), "tg1"),
            PathBuf::from("/tmp/server/.onlyne/gateway/tg1.db")
        );
    }

    #[cfg(feature = "telegram")]
    #[test]
    fn status_marks_conversations_unsupported_without_the_capability() {
        let entry = GatewayEntry {
            id: "tg1".to_string(),
            platform: "telegram".to_string(),
            key: "ed25519/x".to_string(),
            enabled: true,
        };
        let row = status_row(&entry);
        assert!(row.compiled);
        assert!(row.capabilities.contains(&"typing"));
        assert!(!row.capabilities.contains(&"conversations"));
        assert_eq!(row.conversations, "unsupported");
        let value = row.to_value();
        assert_eq!(value["conversations"], "unsupported");
        assert_eq!(value["id"], "tg1");
    }

    #[test]
    fn unlinked_platforms_report_the_feature_to_rebuild_with() {
        let entry = GatewayEntry {
            id: "xx1".to_string(),
            platform: "not-a-platform".to_string(),
            key: "ed25519/x".to_string(),
            enabled: true,
        };
        let row = status_row(&entry);
        assert!(!row.compiled);
        assert!(!row.configured);
        assert_eq!(row.conversations, "unsupported");
        assert!(
            row.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("--features not-a-platform"))
        );
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

    #[tokio::test]
    async fn duplex_pair_drives_inbound_and_render_send() {
        use onlyne_adapter::{Host, HostDispatcher};
        use onlyne_proto::{DetachArgs, HealthArgs, Receipt, SessionRegisterArgs};

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
        let mut refs = GatewayRefStore::open_in_memory().unwrap();
        let envelope = inbound("hello inbound");
        let routed = route_inbound(&router(), &mut refs, &envelope).unwrap();
        gateway
            .deliver_inbound(onlyne_proto::Delivery {
                msg_id: routed.id.clone(),
                envelope: Box::new(routed.clone()),
            })
            .await
            .unwrap();
        let outbound = outbound_from_envelope(
            "telegram",
            "c1".to_string(),
            Some("ext-1".to_string()),
            &routed,
        )
        .await
        .unwrap();
        assert_eq!(outbound.conversation, "c1");
        assert_eq!(outbound.reply_to.as_deref(), Some("ext-1"));
        assert!(outbound.text.contains("hello inbound"));
        server_task.abort();
    }

    #[cfg(feature = "telegram")]
    #[test]
    fn telegram_declares_typing_for_the_host_dispatch() {
        let mut plugin = build_plugin("telegram", "tg1", None, CredentialSource::Placeholder)
            .expect("telegram builds with placeholder credentials");
        assert!(plugin.typing_declared());
    }

    #[cfg(feature = "qqbot")]
    #[tokio::test]
    async fn plugin_without_typing_answers_through_the_absence_path() {
        let mut plugin = build_plugin("qqbot", "qq1", None, CredentialSource::Placeholder)
            .expect("qqbot builds with placeholder credentials");
        assert!(!plugin.typing_declared());
        let err = plugin.set_typing("c1", true).await.unwrap_err();
        assert!(
            err.to_string().contains(&unsupported_host_op("typing")),
            "err = {err}"
        );
    }

    #[test]
    fn a_matching_platform_mount_is_admitted() {
        assert_eq!(
            mounted_platform("telegram", "telegram").expect("same platform mounts"),
            "telegram"
        );
    }

    #[test]
    fn a_mismatched_platform_mount_is_refused_with_both_names() {
        let err = mounted_platform("telegram", "feishu").unwrap_err();
        assert!(err.contains("telegram"), "err = {err}");
        assert!(err.contains("feishu"), "err = {err}");
    }

    #[cfg(feature = "telegram")]
    #[test]
    fn a_reply_threads_to_the_recorded_handle() {
        let mut refs = GatewayRefStore::open_in_memory().unwrap();
        let handle = onlyne_gateway_telegram::gateway_ref("telegram", "c1", "ext-9", "private");
        refs.record(
            &handle,
            &GatewayRef::new("telegram", "c1", "ext-9", Some("private".to_string())),
        )
        .unwrap();

        let threaded = resolve_reply_target(&refs, "telegram", "c1", None, None).unwrap();
        assert_eq!(threaded.as_deref(), Some("ext-9"));

        let explicit_handle =
            resolve_reply_target(&refs, "telegram", "c1", Some(&handle), None).unwrap();
        assert_eq!(explicit_handle.as_deref(), Some("ext-9"));

        let frame_field =
            resolve_reply_target(&refs, "telegram", "c1", Some(&handle), Some("ext-frame"))
                .unwrap();
        assert_eq!(
            frame_field.as_deref(),
            Some("ext-frame"),
            "the frame's own reply_to wins over the table"
        );
    }

    #[test]
    fn a_fresh_message_to_a_conversation_needs_no_history() {
        let refs = GatewayRefStore::open_in_memory().unwrap();
        assert_eq!(
            resolve_reply_target(&refs, "telegram", "c-fresh", None, None).unwrap(),
            None
        );
        assert_eq!(
            resolve_reply_target(&refs, "telegram", "c-fresh", Some("unknown-handle"), None)
                .unwrap(),
            None
        );
    }
}
