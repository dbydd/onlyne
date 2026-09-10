//! Gateway host: one platform process per conversation set (§7, §8).
//!
//! Inbound platform traffic arrives as `deliver` and runs the same send path as
//! a role. Outbound traffic leaves as a `render_send` host call. A gateway that
//! stops answering keeps its queued outbound in the ledger: detection records a
//! fault, and only `control` or `repair_*` moves the row forward.

use crate::faults::{FaultDraft, record};
use crate::relay::{RelayReject, RelayReply, send};
use crate::state::{AdapterLink, ChannelBinding, GatewayConnection, State};
use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use onlyne_adapter::{Host, HostDispatcher};
use onlyne_config::{RouteEntry, Spec};
use onlyne_proto::{
    AdapterMsg, Capability, Delivery, ErrorCode, Event, GatewayHealth, GatewayOp, GatewayMount,
    HelloAck, HelloArgs, HealthArgs, HostOp, Mount, MountKind, PROTOCOL_VERSION,
    RegisterChannelArgs, RenderSendArgs, ResBody, ServerInfo,
};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Fault kind recorded when a gateway presents no usable credentials.
pub const KIND_GATEWAY_UNCONFIGURED: &str = "gateway_unconfigured";
/// Fault kind recorded when a registered gateway stops answering.
pub const KIND_GATEWAY_LOSS: &str = "gateway_loss";

/// The route that carries an inbound platform conversation to a role.
///
/// First match in file order wins.
pub fn resolve_inbound_route<'a>(
    spec: &'a Spec,
    gateway: &str,
    channel: &str,
    conversation: Option<&str>,
) -> Option<&'a RouteEntry> {
    spec.route.iter().find(|route| {
        if route.gateway != gateway || route.channel != channel {
            return false;
        }
        match (&route.conversation, conversation) {
            (Some(declared), Some(seen)) => declared == seen,
            (Some(_), None) => false,
            (None, _) => true,
        }
    })
}

/// The route that carries a role's outbound envelope back to a platform.
///
/// Selection reads `from` first and `reply_to` second, first match in file order.
pub fn select_outbound_route<'a>(
    spec: &'a Spec,
    gateway: &str,
    from: &onlyne_proto::Principal,
    reply_to: Option<&str>,
) -> Option<&'a RouteEntry> {
    let by_sender = spec.route.iter().find(|route| {
        route.gateway == gateway
            && from
                .role_name()
                .map(|role| route.to.role == role)
                .unwrap_or(false)
    });
    by_sender.or_else(|| {
        let wanted = reply_to?;
        spec.route.iter().find(|route| {
            route.gateway == gateway && route.to.session.as_deref() == Some(wanted)
        })
    })
}

/// The conversation a gateway principal addresses.
pub fn conversation_of(principal: &onlyne_proto::Principal) -> Option<String> {
    match principal {
        onlyne_proto::Principal::Gateway {
            conversation,
            channel,
            ..
        } => conversation.clone().or_else(|| Some(channel.clone())),
        _ => None,
    }
}

/// The gateway id a principal addresses.
pub fn gateway_of(principal: &onlyne_proto::Principal) -> Option<&str> {
    match principal {
        onlyne_proto::Principal::Gateway { gateway, .. } => Some(gateway.as_str()),
        _ => None,
    }
}

/// Result of pushing one outbound envelope at the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundPlan {
    Pushed {
        gateway: String,
        conversation: String,
    },
    /// The gateway cannot take it now; the ledger row stays queued.
    Held {
        gateway: String,
        reason: String,
    },
}

/// Whether a gateway link can receive a render call right now.
pub fn link_is_ready(state: &State, gateway: &str) -> Option<Arc<AdapterLink>> {
    let table = state.gateways.read().ok()?;
    let link = table.entries.get(gateway)?;
    if link.health == GatewayHealth::Failed {
        return None;
    }
    link.adapter.clone()
}

/// Push one outbound envelope to its gateway through `render_send`.
pub async fn render_outbound(
    state: &Arc<State>,
    envelope: &onlyne_proto::Envelope,
) -> Result<OutboundPlan, RelayReject> {
    let gateway = gateway_of(&envelope.to)
        .ok_or_else(|| {
            RelayReject::new(
                ErrorCode::UnknownRole,
                "outbound rendering needs a gateway target",
                Some("to.gateway"),
            )
        })?
        .to_string();
    let spec = state
        .spec_snapshot()
        .context("the spec is unavailable")
        .map_err(|error| RelayReject::new(ErrorCode::Internal, error.to_string(), None))?;
    if !spec.gateway.iter().any(|entry| entry.id == gateway) {
        return Err(RelayReject::new(
            ErrorCode::UnknownRole,
            format!("unknown gateway {gateway}"),
            Some("to.gateway"),
        ));
    }
    let reply_to = envelope.causality.as_ref().and_then(|c| c.reply_to.clone());
    let route = select_outbound_route(&spec, &gateway, &envelope.from, reply_to.as_deref())
        .ok_or_else(|| {
            RelayReject::new(
                ErrorCode::UnknownRole,
                format!("no [[route]] row selects gateway {gateway}"),
                Some("route"),
            )
        })?;
    let conversation = conversation_of(&envelope.to)
        .or_else(|| route.conversation.clone())
        .unwrap_or_else(|| route.channel.clone());
    let Some(link) = link_is_ready(state, &gateway) else {
        return Ok(OutboundPlan::Held {
            gateway,
            reason: "gateway is not healthy".to_string(),
        });
    };
    let guard = link.io.lock().await;
    let Some(io) = guard.as_ref() else {
        return Ok(OutboundPlan::Held {
            gateway,
            reason: "gateway link has no adapter transport".to_string(),
        });
    };
    let args = RenderSendArgs {
        envelope: Box::new(envelope.clone()),
        conversation: conversation.clone(),
        gateway_ref: reply_to,
    };
    io.notify(AdapterMsg::Host(HostOp::RenderSend(args)))
        .await
        .map_err(|error| {
            RelayReject::new(ErrorCode::Internal, error.to_string(), Some("gateway"))
        })?;
    Ok(OutboundPlan::Pushed {
        gateway,
        conversation,
    })
}

/// Push every queued gateway-targeted row that its gateway can take.
pub async fn pump_outbound(state: &Arc<State>) -> anyhow::Result<usize> {
    let queued = state.ledger.ledger_query(onlyne_proto::LedgerQuery {
        state: Some(onlyne_proto::LedgerState::Queued),
        limit: 512,
        ..onlyne_proto::LedgerQuery::default()
    })?;
    let mut pushed = 0;
    for row in queued {
        let Ok(to) = serde_json::from_str::<onlyne_proto::Principal>(&row.to_json) else {
            continue;
        };
        if gateway_of(&to).is_none() {
            continue;
        }
        let envelope = crate::relay::delivery_from(&row)?.envelope;
        match render_outbound(state, &envelope).await {
            Ok(OutboundPlan::Pushed { .. }) => pushed += 1,
            Ok(OutboundPlan::Held { .. }) => {}
            Err(reject) if reject.code == ErrorCode::UnknownRole => {}
            Err(error) => return Err(anyhow::anyhow!(error.message)),
        }
    }
    Ok(pushed)
}

/// The health observation a gateway reported, mapped onto the wire enum.
pub fn parse_health(state: &str) -> Option<GatewayHealth> {
    match state {
        "online" => Some(GatewayHealth::Online),
        "reconnecting" => Some(GatewayHealth::Reconnecting),
        "failed" => Some(GatewayHealth::Failed),
        _ => None,
    }
}

/// Register one gateway link and announce its presence.
pub fn announce_presence(
    state: &State,
    gateway: &str,
    platform: &str,
    health: GatewayHealth,
    detail: Option<String>,
) -> anyhow::Result<()> {
    state.emit(Event::GatewayPresence {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        state: health,
        detail,
    })?;
    Ok(())
}

/// Refuse a gateway with no usable credentials and record the fault.
pub fn refuse_unconfigured(state: &State, gateway: &str, reason: &str) -> (ErrorCode, String) {
    let _ = record(
        state,
        FaultDraft::new(KIND_GATEWAY_UNCONFIGURED, reason).with_role(gateway),
    );
    (
        ErrorCode::Unauthorized,
        format!("gateway {gateway} is not configured: {reason}"),
    )
}

/// Validate a gateway identity against the spec.
pub fn validate_gateway(
    state: &Arc<State>,
    gateway: &str,
    platform: &str,
    protocol: u16,
    key: Option<&str>,
) -> Result<String, (ErrorCode, String)> {
    if protocol != PROTOCOL_VERSION {
        return Err((
            ErrorCode::ProtocolVersion,
            format!("protocol {protocol} unsupported, expected {PROTOCOL_VERSION}"),
        ));
    }
    let spec = state
        .spec_snapshot()
        .ok_or_else(|| (ErrorCode::Internal, "the spec is unavailable".to_string()))?;
    let Some(entry) = spec.gateway.iter().find(|entry| entry.id == gateway) else {
        return Err(refuse_unconfigured(
            state,
            gateway,
            "no [[gateway]] entry in spec.toml",
        ));
    };
    if !entry.enabled || entry.key.trim().is_empty() {
        return Err(refuse_unconfigured(
            state,
            gateway,
            "the [[gateway]] entry carries no enabled credential",
        ));
    }
    if let Some(presented) = key {
        if !presented.trim().is_empty() && presented != entry.key {
            return Err(refuse_unconfigured(
                state,
                gateway,
                "the presented key is not the registered key",
            ));
        }
    }
    let platform = if platform.is_empty() {
        entry.platform.clone()
    } else {
        platform.to_string()
    };
    Ok(platform)
}

/// Register one validated gateway link and build its welcome.
pub fn register_link(
    state: &Arc<State>,
    gateway: &str,
    platform: &str,
    adapter: Option<Arc<AdapterLink>>,
    capabilities: Vec<Capability>,
) -> HelloAck {
    let (sender, _receiver) = mpsc::channel(8);
    state.register_gateway(GatewayConnection {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        sender,
        connected_at: Utc::now(),
        health: GatewayHealth::Online,
        last_health_at: Some(Utc::now()),
        channels: Vec::new(),
        adapter,
        capabilities,
    });
    let _ = announce_presence(state, gateway, platform, GatewayHealth::Online, None);
    let spec = state.spec_snapshot();
    HelloAck {
        protocol: PROTOCOL_VERSION,
        role: gateway.to_string(),
        session_id: None,
        generation: 1,
        prose: platform.to_string(),
        server: ServerInfo {
            connected: true,
            cluster: spec
                .as_ref()
                .map(|spec| spec.server.name.clone())
                .unwrap_or_default(),
            name: crate::version().to_string(),
        },
        host_capabilities: vec![Capability::Report, Capability::Typing],
    }
}

/// Validate a gateway `hello` and compute its welcome.
pub fn welcome(
    state: &Arc<State>,
    link: Arc<AdapterLink>,
    args: &HelloArgs,
) -> Result<HelloAck, (ErrorCode, String)> {
    let (gateway, platform) = match &args.mount {
        Some(Mount::Gateway(GatewayMount { gateway, platform })) => {
            (gateway.clone(), platform.clone())
        }
        _ => {
            return Err((
                ErrorCode::Unauthorized,
                "gateway mount data is required".to_string(),
            ));
        }
    };
    let platform = validate_gateway(state, &gateway, &platform, args.protocol, None)?;
    Ok(register_link(
        state,
        &gateway,
        &platform,
        Some(link),
        args.capabilities.clone(),
    ))
}

/// Record one channel declaration and answer the gateway.
pub fn register_channels(
    state: &Arc<State>,
    gateway: &str,
    platform: &str,
    args: &RegisterChannelArgs,
) -> Result<(), (ErrorCode, String)> {
    let binding = ChannelBinding {
        gateway: gateway.to_string(),
        platform: platform.to_string(),
        channel: args.channel.clone(),
        conversations: args.conversations.clone().unwrap_or_default(),
        registered_at: Utc::now(),
    };
    state.register_channel(binding);
    Ok(())
}

/// Apply one health observation, emitting presence only on a change.
pub fn apply_health(
    state: &Arc<State>,
    gateway: &str,
    platform: &str,
    args: &HealthArgs,
) -> Result<(), (ErrorCode, String)> {
    let Some(health) = parse_health(&args.state) else {
        return Err((
            ErrorCode::Invalid,
            format!("unknown gateway health state {}", args.state),
        ));
    };
    let changed = state.touch_health(gateway, health, Utc::now());
    if changed.is_some() {
        let _ = announce_presence(state, gateway, platform, health, args.detail.clone());
    }
    if health == GatewayHealth::Online {
        let state = state.clone();
        tokio::spawn(async move {
            let _ = pump_outbound(&state).await;
        });
    }
    Ok(())
}

/// Turn one inbound delivery into a routed envelope.
pub fn inbound_envelope(
    gateway: &str,
    platform: &str,
    delivery: &Delivery,
) -> Result<onlyne_proto::Envelope, (ErrorCode, String)> {
    let mut envelope = delivery.envelope.as_ref().clone();
    let (channel, conversation) = match &envelope.from {
        onlyne_proto::Principal::Gateway {
            channel,
            conversation,
            ..
        } => (channel.clone(), conversation.clone()),
        _ => (
            String::new(),
            conversation_of(&envelope.to).map(Some).unwrap_or_default(),
        ),
    };
    envelope.from = onlyne_proto::Principal::gateway(gateway, &channel, conversation);
    if envelope.id != delivery.msg_id {
        envelope.id = delivery.msg_id.clone();
    }
    if let Err(error) = envelope.validate() {
        return Err((
            ErrorCode::Invalid,
            format!("{platform} delivery rejected: {error}"),
        ));
    }
    Ok(envelope)
}

/// Route one inbound delivery through the send path.
pub fn deliver(state: &Arc<State>, gateway: &str, platform: &str, delivery: &Delivery) -> ResBody {
    let envelope = match inbound_envelope(gateway, platform, delivery) {
        Ok(envelope) => envelope,
        Err((code, message)) => return ResBody::err(code, message, Some("envelope".to_string())),
    };
    match send(state, &envelope, false, None) {
        Ok(RelayReply::Accepted(outcome)) => {
            ResBody::ok(crate::relay::receipt_json(&outcome.receipt))
        }
        Ok(RelayReply::Duplicate(outcome)) => {
            ResBody::ok(crate::relay::receipt_json(&outcome.receipt))
        }
        Ok(RelayReply::Rejected(reject)) => reject.body(),
        Err(error) => ResBody::err(ErrorCode::Internal, error.to_string(), None),
    }
}

/// Detect gateways that stopped answering, record a fault, emit presence.
///
/// The queued outbound for that gateway stays in the ledger.
pub fn health_sweep(state: &Arc<State>, now: chrono::DateTime<Utc>) -> Vec<String> {
    let limit_ms = state
        .spec_snapshot()
        .map(|spec| spec.server.heartbeat_timeout_ms)
        .unwrap_or(30_000);
    let mut lost = Vec::new();
    for gateway in state.stale_gateways(now, limit_ms) {
        if state.touch_health(&gateway, GatewayHealth::Failed, now).is_none() {
            continue;
        }
        let queued = state
            .ledger
            .queued_for(&gateway, 512)
            .map(|rows| rows.len())
            .unwrap_or(0);
        let _ = record(
            state,
            FaultDraft::new(
                KIND_GATEWAY_LOSS,
                format!("no health observation within {limit_ms}ms; {queued} queued row(s) retained"),
            )
            .with_role(&gateway)
            .with_observed(serde_json::json!({ "queued": queued })),
        );
        let platform = state
            .gateways
            .read()
            .ok()
            .and_then(|table| table.get(&gateway).map(|link| link.platform.clone()))
            .unwrap_or_default();
        let _ = announce_presence(state, &gateway, &platform, GatewayHealth::Failed, None);
        lost.push(gateway);
    }
    lost
}

/// Drop a gateway link and announce the departure.
pub fn detach(state: &Arc<State>, gateway: &str, reason: &str) {
    let platform = state
        .unregister_gateway(gateway)
        .map(|link| link.platform)
        .unwrap_or_default();
    state.clear_channels(gateway);
    let _ = announce_presence(
        state,
        gateway,
        &platform,
        GatewayHealth::Failed,
        Some(reason.to_string()),
    );
}

/// The `Host` implementation serving the adapter transport of one gateway.
pub struct GatewayHostImpl {
    state: Arc<State>,
    gateway: String,
    platform: String,
    link: Arc<AdapterLink>,
}

impl GatewayHostImpl {
    pub fn new(state: Arc<State>, gateway: String, platform: String, link: Arc<AdapterLink>) -> Self {
        GatewayHostImpl {
            state,
            gateway,
            platform,
            link,
        }
    }
}

#[async_trait]
impl Host for GatewayHostImpl {
    async fn hello(&self, args: &HelloArgs) -> Result<HelloAck, (ErrorCode, String)> {
        welcome(&self.state, self.link.clone(), args)
    }

    async fn deliver(&self, delivery: &Delivery) -> Result<(), (ErrorCode, String)> {
        let body = deliver(&self.state, &self.gateway, &self.platform, delivery);
        if body.ok {
            Ok(())
        } else {
            let error = body.error.unwrap_or_else(|| onlyne_proto::ErrorPayload {
                code: ErrorCode::Internal,
                message: "delivery failed".to_string(),
                field: None,
            });
            Err((error.code, error.message))
        }
    }

    async fn register_channel(&self, args: &RegisterChannelArgs) -> Result<(), (ErrorCode, String)> {
        register_channels(&self.state, &self.gateway, &self.platform, args)
    }

    async fn health(&self, args: &HealthArgs) -> Result<(), (ErrorCode, String)> {
        apply_health(&self.state, &self.gateway, &self.platform, args)
    }

    async fn detach(&self, args: &onlyne_proto::DetachArgs) -> Result<(), (ErrorCode, String)> {
        detach(&self.state, &self.gateway, &args.reason);
        Ok(())
    }
}

/// Serve one adapter-protocol gateway connection whose `hello` was already read.
///
/// The socket dispatcher reads the first frame to tell an adapter `hello` from a
/// request frame, then answers the welcome here and hands the rest to the
/// adapter host loop.
pub async fn serve_adapter(
    state: Arc<State>,
    stream: tokio::net::UnixStream,
    first: onlyne_adapter::WireMessage,
) -> anyhow::Result<()> {
    let hello = match first.msg {
        AdapterMsg::Plugin(onlyne_proto::PluginOp::Hello(args)) => args,
        _ => {
            let mut stream = stream;
            let body = ResBody::err(
                ErrorCode::Unauthorized,
                onlyne_proto::HELLO_REQUIRED_MESSAGE,
                Some("op".to_string()),
            );
            let reply = onlyne_adapter::WireMessage {
                id: None,
                reply_to: Some(first.id.unwrap_or_default()),
                msg: AdapterMsg::Res(body),
            };
            onlyne_frame::write_frame(&mut stream, &reply).await?;
            return Ok(());
        }
    };
    let link = Arc::new(AdapterLink::default());
    let ack = match welcome(&state, link.clone(), &hello) {
        Ok(ack) => ack,
        Err((code, message)) => {
            let mut stream = stream;
            let reply = onlyne_adapter::WireMessage {
                id: None,
                reply_to: Some(first.id.unwrap_or_default()),
                msg: AdapterMsg::Res(ResBody::err(code, message, None)),
            };
            onlyne_frame::write_frame(&mut stream, &reply).await?;
            return Ok(());
        }
    };
    let gateway = match &hello.mount {
        Some(Mount::Gateway(GatewayMount { gateway, .. })) => gateway.clone(),
        _ => "unknown".to_string(),
    };
    let platform = match &hello.mount {
        Some(Mount::Gateway(GatewayMount { platform, .. })) => platform.clone(),
        _ => "unknown".to_string(),
    };
    let mut stream = stream;
    let welcome_frame = onlyne_adapter::WireMessage {
        id: None,
        reply_to: Some(first.id.unwrap_or_default()),
        msg: AdapterMsg::Res(ResBody::ok(
            serde_json::to_value(HostOp::Welcome(ack.clone())).unwrap_or_default(),
        )),
    };
    onlyne_frame::write_frame(&mut stream, &welcome_frame).await?;
    let (io, inbound) = onlyne_adapter::AdapterIo::new_with_inbound(
        stream,
        onlyne_adapter::DEFAULT_READ_TIMEOUT,
        onlyne_adapter::DEFAULT_WRITE_TIMEOUT,
    );
    *link.io.lock().await = Some(io.clone());
    let host = Arc::new(GatewayHostImpl::new(
        state.clone(),
        gateway.clone(),
        platform,
        link,
    ));
    let dispatcher = HostDispatcher::new(MountKind::Gateway, host);
    let result = dispatcher.serve(io, inbound).await;
    detach(&state, &gateway, "adapter connection closed");
    result.map_err(|error| anyhow::anyhow!(error.to_string()))
}

/// Serve one `GatewayOp` request on a raw frame connection.
///
/// `gateway` carries the identity the connection established at `hello`.
pub async fn handle_gateway_op(
    state: &Arc<State>,
    gateway: Option<&str>,
    op: GatewayOp,
) -> ResBody {
    match op {
        GatewayOp::Hello(args) => {
            let platform = match validate_gateway(
                state,
                &args.role,
                "",
                args.protocol,
                Some(&args.key),
            ) {
                Ok(platform) => platform,
                Err((code, message)) => return ResBody::err(code, message, None),
            };
            let ack = register_link(state, &args.role, &platform, None, Vec::new());
            ResBody::ok(serde_json::to_value(HostOp::Welcome(ack)).unwrap_or_default())
        }
        GatewayOp::RegisterChannel(args) => {
            let Some(gateway) = gateway else {
                return ResBody::err(
                    ErrorCode::Unauthorized,
                    onlyne_proto::HELLO_REQUIRED_MESSAGE,
                    Some("op".to_string()),
                );
            };
            match register_channels(state, gateway, &args.platform, &args) {
                Ok(()) => ResBody::ok(serde_json::json!({ "channel": args.channel })),
                Err((code, message)) => ResBody::err(code, message, None),
            }
        }
        GatewayOp::Deliver(delivery) => {
            let Some(gateway) = gateway else {
                return ResBody::err(
                    ErrorCode::Unauthorized,
                    onlyne_proto::HELLO_REQUIRED_MESSAGE,
                    Some("op".to_string()),
                );
            };
            let platform = platform_of(state, gateway);
            deliver(state, gateway, &platform, &delivery)
        }
        GatewayOp::Health(args) => {
            let Some(gateway) = gateway else {
                return ResBody::err(
                    ErrorCode::Unauthorized,
                    onlyne_proto::HELLO_REQUIRED_MESSAGE,
                    Some("op".to_string()),
                );
            };
            let platform = platform_of(state, gateway);
            match apply_health(state, gateway, &platform, &args) {
                Ok(()) => ResBody::ok(serde_json::json!({ "state": args.state })),
                Err((code, message)) => ResBody::err(code, message, None),
            }
        }
        GatewayOp::Bye(args) => {
            let Some(gateway) = gateway else {
                return ResBody::err(
                    ErrorCode::Unauthorized,
                    onlyne_proto::HELLO_REQUIRED_MESSAGE,
                    Some("op".to_string()),
                );
            };
            detach(state, gateway, &args.reason);
            ResBody::ok(serde_json::json!({ "bye": args.reason }))
        }
    }
}

/// The platform of one registered gateway.
pub fn platform_of(state: &State, gateway: &str) -> String {
    state
        .gateways
        .read()
        .ok()
        .and_then(|table| table.get(gateway).map(|link| link.platform.clone()))
        .unwrap_or_default()
}
