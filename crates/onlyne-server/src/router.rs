//! Frame router: one `match` per vocabulary (§6, §8).
//!
//! Every arm calls a landed handler. A frame that arrives before the handshake
//! is refused with `HELLO_REQUIRED_MESSAGE` under `ErrorCode::Invalid`, which is
//! the plan's spelling at §7 line 310, and an op outside a connection's
//! vocabulary answers `ErrorCode::UnknownOp`.

use crate::events;
use crate::faults::{self, FaultDraft};
use crate::gateway_host;
use crate::projection;
use crate::relay::{self, RelayReply};
use crate::state::{self as server_state, State};
use chrono::Utc;
use onlyne_config::Spec;
use onlyne_layout::ServerRoot;
use onlyne_proto::{
    AdminOp, Body, Causality, ClientOp, ControlOp, ErrorCode, Event, Frame, GatewayOp,
    HELLO_REQUIRED_MESSAGE, LedgerEntry, MsgKind, Presence, Principal, QueryRolesArgs, ResBody,
    RoleInfo, RolePresence, SpecReloaded,
};
use onlyne_store::RoleRow;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Per-connection state carried by the router.
#[derive(Debug, Default)]
pub struct Session {
    pub role: Option<String>,
    /// The role the transport handshake verified against the presented key.
    /// `onlyne_net::handshake` checks the key against the registered key of the
    /// claimed role, so this name is the sole authority for role identity on a
    /// TLS connection.
    pub authorised: Option<String>,
    pub admin: bool,
    pub authenticated: bool,
    pub session_id: Option<String>,
    pub generation: u64,
    pub sender: Option<mpsc::Sender<Frame>>,
    pub gateway: Option<String>,
}

impl Session {
    /// A client session whose role identity the transport already settled.
    pub fn with_sender(sender: mpsc::Sender<Frame>, authorised: &str) -> Self {
        Session {
            authorised: Some(authorised.to_string()),
            sender: Some(sender),
            ..Session::default()
        }
    }

    fn role_or_reject(&self) -> Result<&str, ResBody> {
        self.role.as_deref().ok_or_else(hello_required)
    }

    fn welcome(&mut self, role: &str, admin: bool) {
        self.role = Some(role.to_string());
        self.admin = admin;
        self.authenticated = true;
    }
}

/// The refusal a frame earns when it arrives before its `hello`.
///
/// §7 line 310 spells this `invalid` with the message below. `Unauthorized`
/// belongs to the retryable set of `ErrorCode::is_permanent`, so answering that
/// code would tell a plugin to keep retrying a handshake it cannot complete.
pub(crate) fn hello_required() -> ResBody {
    ResBody::err(
        ErrorCode::Invalid,
        HELLO_REQUIRED_MESSAGE,
        Some("op".to_string()),
    )
}

/// Route one client frame body.
pub async fn dispatch_client(state: &Arc<State>, session: &mut Session, op: ClientOp) -> ResBody {
    let pre_auth = matches!(op, ClientOp::Hello(_));
    if !session.authenticated && !pre_auth {
        return hello_required();
    }
    match op {
        ClientOp::Hello(args) => hello(state, session, args),
        ClientOp::Send(envelope) => {
            let role = match session.role_or_reject() {
                Ok(role) => role.to_string(),
                Err(body) => return body,
            };
            let mut envelope = *envelope;
            envelope.from = Principal::role(role.clone());
            envelope.admin = session.admin;
            let owner = envelope
                .task_id()
                .and_then(|task| relay::task_owner(state, task));
            reply_body(
                state,
                relay::send(state, &envelope, session.admin, owner.as_deref()),
            )
        }
        ClientOp::Pull(args) => {
            let role = match session.role_or_reject() {
                Ok(role) => role.to_string(),
                Err(body) => return body,
            };
            match relay::pull(state, &role, session.session_id.as_deref(), &args) {
                Ok(reply) => ResBody::ok(serde_json::to_value(reply).unwrap_or_default()),
                Err(error) => internal(error),
            }
        }
        ClientOp::Ack(args) => match relay::ack(state, &args) {
            Ok(Ok(event)) => ResBody::ok(serde_json::to_value(event).unwrap_or_default()),
            Ok(Err(reject)) => reject.body(),
            Err(error) => internal(error),
        },
        ClientOp::Report(report) => {
            let role = match session.role_or_reject() {
                Ok(role) => role.to_string(),
                Err(body) => return body,
            };
            match projection::report(state, &role, &report) {
                Ok(outcome) => ResBody::ok(json!({
                    "applied": outcome.applied,
                    "kind": report.kind_name(),
                    "task_id": report.task_id(),
                })),
                Err(error) => internal(error),
            }
        }
        ClientOp::SessionSync(args) => {
            let role = match session.role_or_reject() {
                Ok(role) => role.to_string(),
                Err(body) => return body,
            };
            match projection::session_sync(state, &role, &args) {
                Ok(outcome) => ResBody::ok(json!({
                    "applied": outcome.applied,
                    "task_id": args.task_id,
                })),
                Err(error) => internal(error),
            }
        }
        ClientOp::Subscribe(subscribe) => match events::page_for(state, &subscribe) {
            Ok(page) => {
                if let Some(sender) = session.sender.clone() {
                    events::spawn_forwarder(
                        state,
                        events::EventFilter::from_subscribe(&subscribe),
                        sender,
                    );
                }
                ResBody::ok(events::page_json(&page))
            }
            Err(error) => internal(error),
        },
        ClientOp::QueryLedger(query) => match state.ledger.ledger_query(query) {
            Ok(rows) => ResBody::ok(json!({
                "ledger": rows.iter().map(relay::entry_from_row).collect::<Vec<LedgerEntry>>(),
            })),
            Err(error) => internal(error.into()),
        },
        ClientOp::QuerySessions(query) => match projection::sessions(state, query) {
            Ok(rows) => ResBody::ok(json!({ "sessions": rows })),
            Err(error) => internal(error),
        },
        ClientOp::QueryRoles(query) => match roles(state, &query) {
            Ok(rows) => ResBody::ok(json!({ "roles": rows })),
            Err(error) => internal(error),
        },
        ClientOp::QueryFaults(query) => match faults::query(state, &query) {
            Ok(rows) => ResBody::ok(json!({ "faults": rows })),
            Err(error) => internal(error),
        },
        ClientOp::Control(args) => {
            let role = match session.role_or_reject() {
                Ok(role) => role.to_string(),
                Err(body) => return body,
            };
            let envelope = control_envelope(&role, &args.op, args.to.as_deref());
            let owner = relay::task_owner(state, args.op.task_id());
            reply_body(
                state,
                relay::send(state, &envelope, session.admin, owner.as_deref()),
            )
        }
        ClientOp::Bye(args) => {
            if let Some(role) = session.role.clone() {
                relay::disconnect(state, &role).ok();
            }
            session.authenticated = false;
            ResBody::ok(json!({ "bye": args.reason }))
        }
    }
}

/// Route one admin frame body.
pub async fn dispatch_admin(state: &Arc<State>, session: &mut Session, op: AdminOp) -> ResBody {
    let _ = session;
    match op {
        AdminOp::Status(_) => status(state),
        AdminOp::Roles(query) => match roles(state, &query) {
            Ok(rows) => ResBody::ok(json!({ "roles": rows })),
            Err(error) => internal(error),
        },
        AdminOp::Sessions(query) => match projection::sessions(state, query) {
            Ok(rows) => ResBody::ok(json!({ "sessions": rows })),
            Err(error) => internal(error),
        },
        AdminOp::Ledger(query) => match state.ledger.ledger_query(query) {
            Ok(rows) => ResBody::ok(json!({
                "ledger": rows.iter().map(relay::entry_from_row).collect::<Vec<LedgerEntry>>(),
            })),
            Err(error) => internal(error.into()),
        },
        AdminOp::Faults(query) => match faults::query(state, &query) {
            Ok(rows) => ResBody::ok(json!({ "faults": rows })),
            Err(error) => internal(error),
        },
        AdminOp::Watch(subscribe) => match events::page_for(state, &subscribe) {
            Ok(page) => ResBody::ok(events::page_json(&page)),
            Err(error) => internal(error),
        },
        AdminOp::History(args) => match events::history(state, &args) {
            Ok(page) => ResBody::ok(events::page_json(&page)),
            Err(error) => internal(error),
        },
        AdminOp::SpecDiff(_) => match spec_diff(state) {
            Ok((diff, _hash)) => ResBody::ok(json!({
                "render": diff.render(),
                "empty": diff.is_empty(),
            })),
            Err(error) => internal(error),
        },
        AdminOp::Reload(_) => reload(state),
        AdminOp::Send(admin_send) => {
            let mut envelope = *admin_send.envelope;
            envelope.from = Principal::role(admin_send.from.clone());
            envelope.admin = true;
            let owner = envelope
                .task_id()
                .and_then(|task| relay::task_owner(state, task));
            reply_body(state, relay::send(state, &envelope, true, owner.as_deref()))
        }
        AdminOp::Control(admin_control) => {
            let envelope = control_envelope(
                &admin_control.from,
                &admin_control.op,
                admin_control.to.as_deref(),
            );
            let owner = relay::task_owner(state, admin_control.op.task_id());
            reply_body(state, relay::send(state, &envelope, true, owner.as_deref()))
        }
        AdminOp::RepairInspect(_)
        | AdminOp::RepairAdopt(_)
        | AdminOp::RepairRebind(_)
        | AdminOp::RepairRetry(_)
        | AdminOp::RepairFail(_)
        | AdminOp::RepairClose(_)
        | AdminOp::RepairAck(_) => match faults::repair(state, &op) {
            Ok(Ok(value)) => ResBody::ok(value),
            Ok(Err(reject)) => reject.body(),
            Err(error) => internal(error),
        },
        AdminOp::Shutdown(args) => {
            state.request_shutdown();
            ResBody::ok(json!({ "shutdown": args.reason }))
        }
    }
}

/// Route one gateway frame body.
pub async fn dispatch_gateway(state: &Arc<State>, session: &mut Session, op: GatewayOp) -> ResBody {
    if let GatewayOp::Hello(args) = &op {
        session.gateway = Some(args.role.clone());
    }
    let body = gateway_host::handle_gateway_op(state, session.gateway.as_deref(), op).await;
    if !body.ok {
        session.gateway = None;
    }
    body
}

fn hello(state: &Arc<State>, session: &mut Session, args: onlyne_proto::HandshakeArgs) -> ResBody {
    let Some(spec) = state.spec_snapshot() else {
        return internal(anyhow::anyhow!("the spec is unavailable"));
    };
    // The transport verdict is the sole source of role identity on a role
    // connection: `onlyne_net::handshake` admitted this socket because the
    // presented key equals the registered key of `authorised`, so a typed
    // `hello` naming a second role is refused here, and the welcome below
    // records the authorised name.
    let authorised = session
        .authorised
        .clone()
        .unwrap_or_else(|| args.role.clone());
    if authorised != args.role {
        return ResBody::err(
            ErrorCode::Unauthorized,
            format!(
                "the transport authenticated role {authorised}; this hello claims {}",
                args.role
            ),
            Some("role".to_string()),
        );
    }
    let Some(entry) = spec
        .client
        .iter()
        .find(|entry| entry.role == authorised)
        .cloned()
    else {
        return ResBody::err(
            ErrorCode::Unauthorized,
            format!("unregistered role {authorised}"),
            Some("role".to_string()),
        );
    };
    session.welcome(&authorised, entry.admin);
    if let Some(sender) = session.sender.clone() {
        state.register_role(crate::state::RoleConnection {
            role: entry.role.clone(),
            sender,
            last_seq: state.event_head().max(0) as u64,
            connected_at: Utc::now(),
            draining: false,
        });
        let _ = state.emit(Event::RolePresence(RolePresence {
            role: entry.role.clone(),
            state: Presence::Online,
            aggregate: if entry.aggregate.is_empty() {
                None
            } else {
                Some(entry.aggregate.clone())
            },
            sessions: 0,
            detail: Some(args.agent.clone()),
        }));
    }
    let welcome = onlyne_proto::Welcome {
        cluster: spec.server.name.clone(),
        server: crate::version().to_string(),
        role: entry.role.clone(),
        admin: entry.admin,
        // The label the plan puts on a supervisor's `[[client]]` entry (plan
        // §5 line 248): a pure annotation. The core delivery path carries no
        // aggregate branch (plan §5 line 274), so no ACL, routing, or ledger
        // decision reads it, and `spec.toml` stays the only truth about which
        // role represents which child cluster (plan §5 line 243).
        aggregate: (!entry.aggregate.is_empty()).then(|| entry.aggregate.clone()),
        max_sessions: entry.max_sessions,
        reuse: entry.reuse,
        prose: entry.prose.clone(),
        spec_hash: spec.semantic_hash(),
        allowed_targets: entry.allowed_targets.clone(),
        allowed_senders: entry.allowed_senders.clone(),
        session_command: (!entry.session_command.is_empty()).then(|| entry.session_command.clone()),
        timeout_ready_ms: Some(entry.timeout.ready_ms),
        timeout_running_ms: Some(entry.timeout.running_ms),
        timeout_idle_ms: Some(entry.timeout.idle_ms),
        intent_attempts: Some(entry.intent.attempts),
        intent_backoff_ms: Some(entry.intent.backoff_ms.clone()),
        seq: state.event_head().max(0) as u64,
    };
    ResBody::ok(serde_json::to_value(welcome).unwrap_or_default())
}

/// The cluster summary `wait-ready` polls.
pub fn status(state: &Arc<State>) -> ResBody {
    let Some(spec) = state.spec_snapshot() else {
        return internal(anyhow::anyhow!("the spec is unavailable"));
    };
    let connected_roles = state.roles.read().map(|table| table.len()).unwrap_or(0);
    let gateways = gateway_rows(state);
    let connected_gateways = gateways
        .iter()
        .filter(|row| row["state"] != json!("offline"))
        .count();
    ResBody::ok(json!({
        "ok": true,
        "cluster": spec.server.name,
        "version": crate::version(),
        "spec_hash": spec.semantic_hash(),
        "roles": spec.client.len(),
        "role_count": spec.client.len(),
        "gateway_count": spec.gateway.len(),
        "gateways": gateways,
        "routes": spec.route.len(),
        "channels": state.channel_count(),
        "connected_roles": connected_roles,
        "connected_gateways": connected_gateways,
        "event_head": state.event_head(),
        "uptime_s": (Utc::now() - state.start_at).num_seconds().max(0),
    }))
}

/// One row per configured gateway with its live state and capabilities.
pub fn gateway_rows(state: &Arc<State>) -> Vec<Value> {
    let spec = match state.spec_snapshot() {
        Some(spec) => spec,
        None => return Vec::new(),
    };
    let links = state.gateways.read().ok();
    spec.gateway
        .iter()
        .map(|entry| {
            let link = links.as_ref().and_then(|table| table.get(&entry.id));
            json!({
                "id": entry.id,
                "platform": entry.platform,
                "enabled": entry.enabled,
                "state": link
                    .map(|link| link.health.as_str())
                    .unwrap_or("offline"),
                "capabilities": link
                    .map(|link| link.capabilities.iter().map(|cap| cap.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default(),
                "channels": state
                    .channels_for(&entry.id)
                    .iter()
                    .map(|binding| binding.channel.clone())
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// Role registry rows with live presence.
pub fn roles(state: &Arc<State>, query: &QueryRolesArgs) -> anyhow::Result<Vec<RoleInfo>> {
    let spec = state
        .spec_snapshot()
        .ok_or_else(|| anyhow::anyhow!("the spec is unavailable"))?;
    let stored = state.ledger.list_roles()?;
    let mut rows = Vec::new();
    for entry in &spec.client {
        if let Some(wanted) = &query.role {
            if wanted != &entry.role {
                continue;
            }
        }
        let (presence, detail) = match state.roles.read().ok().and_then(|table| {
            table
                .get(&entry.role)
                .map(|link| (link.draining, link.connected_at))
        }) {
            Some((true, _)) => (Presence::Draining, None),
            Some((false, _)) => (Presence::Online, None),
            None => (Presence::Offline, None),
        };
        let sessions = state
            .ledger
            .list_sessions(onlyne_proto::QuerySessionsArgs {
                role: Some(entry.role.clone()),
                limit: 500,
                ..onlyne_proto::QuerySessionsArgs::default()
            })?
            .len() as u32;
        rows.push(RoleInfo {
            name: entry.role.clone(),
            admin: entry.admin,
            max_sessions: entry.max_sessions,
            spec_hash: stored
                .iter()
                .find(|row| row.name == entry.role)
                .map(|row| row.spec_hash.clone())
                .unwrap_or_else(|| spec.semantic_hash()),
            prose: Some(entry.prose.clone()),
            state: presence,
            sessions,
            detail,
            edges: entry.allowed_targets.clone(),
            aggregate: (!entry.aggregate.is_empty()).then(|| entry.aggregate.clone()),
        });
    }
    Ok(rows)
}

/// Diff the in-memory spec against the file on disk.
pub fn spec_diff(state: &Arc<State>) -> anyhow::Result<(onlyne_config::SpecDiff, String)> {
    let spec = state
        .spec_snapshot()
        .ok_or_else(|| anyhow::anyhow!("the spec is unavailable"))?;
    let layout = ServerRoot::resolve(&state.root);
    let diff = spec.load_validate(layout.spec_path())?;
    Ok((diff, spec.semantic_hash()))
}

/// What one successful spec reload produced.
#[derive(Debug, Clone)]
pub struct ReloadOutcome {
    /// `SpecDiff::render()` text for the operator log.
    pub render: String,
    pub spec_hash: String,
    pub roles: usize,
    pub gateways: usize,
    pub routes: usize,
}

/// Re-read `spec.toml`, rebuild the ACL, and announce the reload.
///
/// Both `AdminOp::Reload` and the SIGHUP listener call this one body. A failed
/// reload keeps the in-memory spec and records `fault{kind:"spec_reload_failed"}`.
pub fn reload_spec(state: &Arc<State>) -> Result<ReloadOutcome, String> {
    let layout = ServerRoot::resolve(&state.root);
    let Some(current) = state.spec_snapshot() else {
        return Err("the spec is unavailable".to_string());
    };
    let diff = match current.load_validate(layout.spec_path()) {
        Ok(diff) => diff,
        Err(error) => return Err(reload_failure(state, &error.to_string())),
    };
    let next = match Spec::load(layout.spec_path()) {
        Ok(next) => next,
        Err(error) => return Err(reload_failure(state, &error.to_string())),
    };
    let acl = match server_state::acl_from_spec(&next) {
        Ok(acl) => acl,
        Err(error) => return Err(reload_failure(state, &error.to_string())),
    };
    let hash = next.semantic_hash();
    let names: Vec<String> = next.client.iter().map(|entry| entry.role.clone()).collect();
    let updated_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    for entry in &next.client {
        if let Err(error) = state.ledger.upsert_role(&RoleRow {
            name: entry.role.clone(),
            key: entry.key.clone(),
            admin: entry.admin,
            max_sessions: i64::from(entry.max_sessions),
            spec_hash: hash.clone(),
            updated_at: updated_at.clone(),
        }) {
            return Err(reload_failure(state, &error.to_string()));
        }
    }
    if let Err(error) = state.ledger.remove_role_missing_from(&names) {
        return Err(reload_failure(state, &error.to_string()));
    }
    state.replace_spec(next.clone());
    state.replace_acl(acl);
    let event = Event::SpecReloaded(SpecReloaded {
        spec_hash: hash.clone(),
        roles: next.client.len() as u32,
        gateways: next.gateway.len() as u32,
        routes: next.route.len() as u32,
    });
    if let Err(error) = state.emit(event) {
        return Err(reload_failure(state, &error.to_string()));
    }
    Ok(ReloadOutcome {
        render: diff.render(),
        spec_hash: hash,
        roles: next.client.len(),
        gateways: next.gateway.len(),
        routes: next.route.len(),
    })
}

/// Record the failed reload and hand the message back.
fn reload_failure(state: &State, message: &str) -> String {
    let _ = faults::record(state, FaultDraft::spec_reload_failed(message));
    message.to_string()
}

/// The `AdminOp::Reload` answer.
pub fn reload(state: &Arc<State>) -> ResBody {
    match reload_spec(state) {
        Ok(outcome) => ResBody::ok(json!({
            "render": outcome.render,
            "spec_hash": outcome.spec_hash,
            "roles": outcome.roles,
            "gateways": outcome.gateways,
            "routes": outcome.routes,
        })),
        Err(message) => ResBody::err(ErrorCode::Invalid, message, Some("spec.toml".to_string())),
    }
}

/// Build one control envelope from a role.
pub fn control_envelope(from: &str, op: &ControlOp, to: Option<&str>) -> onlyne_proto::Envelope {
    let task_id = op.task_id().to_string();
    let target = to.unwrap_or(from).to_string();
    let mut envelope = onlyne_proto::new_envelope(
        MsgKind::Control,
        Principal::role(from),
        Principal::role(target.clone()),
        Body::text(op.name()),
        Some(Causality::root(task_id.clone())),
    )
    .unwrap_or_else(|_| onlyne_proto::Envelope {
        protocol: onlyne_proto::PROTOCOL_VERSION,
        id: onlyne_proto::new_id(),
        op_id: Some(onlyne_proto::new_op_id()),
        kind: MsgKind::Control,
        from: Principal::role(from),
        to: Principal::role(target.clone()),
        control: None,
        causality: Some(Causality::root(task_id.clone())),
        body: Body::text(op.name()),
        ts: Utc::now(),
        ttl_ms: None,
        admin: false,
    });
    envelope.control = Some(op.clone());
    envelope
}

/// Map one relay reply onto a response body.
///
/// A reply whose row targets a gateway conversation also schedules the outbound
/// pump: the row is durable by then, and a push to a mounted gateway is the only
/// delivery a platform conversation can take (plan §7 line 293).
pub fn reply_body(
    state: &std::sync::Arc<crate::state::State>,
    reply: anyhow::Result<RelayReply>,
) -> ResBody {
    match reply {
        Ok(RelayReply::Accepted(outcome)) => {
            if outcome.pump_outbound {
                crate::gateway_host::schedule_pump(state);
            }
            ResBody::ok(relay::receipt_json(&outcome.receipt))
        }
        Ok(RelayReply::Duplicate(outcome)) => {
            if outcome.pump_outbound {
                crate::gateway_host::schedule_pump(state);
            }
            ResBody::err_with_data(
                ErrorCode::Duplicate,
                format!("duplicate op_id: replaying {}", outcome.receipt.msg_id),
                Some("op_id".to_string()),
                relay::receipt_json(&outcome.receipt),
            )
        }
        Ok(RelayReply::Rejected(reject)) => reject.body(),
        Err(error) => internal(error),
    }
}

/// The response for an internal failure.
pub fn internal(error: anyhow::Error) -> ResBody {
    ResBody::err(ErrorCode::Internal, error.to_string(), None)
}
