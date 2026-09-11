//! The send path and the pull/ack delivery path (§8, §10).
//!
//! One accepted send runs the same fixed step order: envelope validation,
//! recipient resolution, ACL, the offline note gate, idempotency, one ledger
//! row, delivery. A refusal writes no ledger row, so a sender keeps its
//! `op_id` usable for the retry that succeeds once the recipient is online.

use crate::events;
use crate::gateway_host::{resolve_inbound_route, select_outbound_route};
use crate::state::{DeliveryTicket, State};
use anyhow::Context;
use chrono::{DateTime, Utc};
use onlyne_config::Spec;
use onlyne_net::{MsgClass, acl_allows};
use onlyne_proto::{
    AckArgs, Body, Causality, Delivery, Envelope, ErrorCode, Event, LedgerQuery, LedgerState,
    LedgerStateEvent, MsgKind, OP_ID_CONFLICT_MESSAGE, Outcome, PROTOCOL_VERSION, Presence,
    Principal, PullArgs, PullReply, Receipt, ResBody, RolePresence,
};
use onlyne_store::{Append, LedgerRow};
use serde_json::Value;

/// How often [`spawn_expiry_sweep`] settles queued notes past their deadline.
///
/// A note carries its lifetime in `Envelope::ttl_ms`, a millisecond budget, and
/// the plan settles an elapsed one as `expired` (plan line 505). One second
/// bounds the overshoot of the shortest ttl a sender can usefully express.
pub const SWEEP_INTERVAL_MS: u64 = 1000;

/// A send the server accepted and recorded.
#[derive(Debug, Clone)]
pub struct SendOutcome {
    pub receipt: Receipt,
    pub row: LedgerRow,
    pub online: bool,
    /// The row targets a platform conversation, so the gateway pump covers it.
    pub pump_outbound: bool,
}

/// A send the server refused, carrying the wire error it answers with.
#[derive(Debug, Clone)]
pub struct RelayReject {
    pub code: ErrorCode,
    pub message: String,
    pub field: Option<String>,
    pub data: Option<Value>,
}

impl RelayReject {
    pub fn new(code: ErrorCode, message: impl Into<String>, field: Option<&str>) -> Self {
        RelayReject {
            code,
            message: message.into(),
            field: field.map(str::to_string),
            data: None,
        }
    }

    pub fn body(&self) -> ResBody {
        match &self.data {
            Some(data) => ResBody::err_with_data(
                self.code,
                self.message.clone(),
                self.field.clone(),
                data.clone(),
            ),
            None => ResBody::err(self.code, self.message.clone(), self.field.clone()),
        }
    }
}

/// The outcome of one send attempt.
#[derive(Debug, Clone)]
pub enum RelayReply {
    Accepted(Box<SendOutcome>),
    /// The same `op_id` and fingerprint were already accepted.
    Duplicate(Box<SendOutcome>),
    Rejected(RelayReject),
}

impl RelayReply {
    pub fn body(&self) -> ResBody {
        match self {
            RelayReply::Accepted(outcome) => ResBody::ok(receipt_json(&outcome.receipt)),
            RelayReply::Duplicate(outcome) => ResBody::err_with_data(
                ErrorCode::Duplicate,
                format!(
                    "duplicate op_id: replaying the durable receipt for {}",
                    outcome.receipt.msg_id
                ),
                Some("op_id".to_string()),
                receipt_json(&outcome.receipt),
            ),
            RelayReply::Rejected(reject) => reject.body(),
        }
    }
}

/// The ACL class of one message kind.
pub fn class_of(kind: MsgKind) -> MsgClass {
    match kind {
        MsgKind::Task | MsgKind::Completion => MsgClass::Any,
        MsgKind::Note => MsgClass::Note,
        MsgKind::Control => MsgClass::Control,
    }
}

/// The role a target principal resolves to, applying `[[route]]` for gateways.
pub fn resolve_target(spec: &Spec, to: &Principal) -> Result<String, RelayReject> {
    match to {
        Principal::Role { role, .. } => {
            if spec.role_names().iter().any(|name| name == role) {
                Ok(role.clone())
            } else {
                Err(RelayReject::new(
                    ErrorCode::UnknownRole,
                    format!("unknown target role {role}"),
                    Some("to.role"),
                ))
            }
        }
        Principal::Gateway {
            gateway,
            channel,
            conversation,
        } => {
            if !spec.gateway.iter().any(|entry| entry.id == *gateway) {
                return Err(RelayReject::new(
                    ErrorCode::UnknownRole,
                    format!("unknown gateway {gateway}"),
                    Some("to.gateway"),
                ));
            }
            resolve_inbound_route(spec, gateway, channel, conversation.as_deref())
                .map(|route| route.to.role.clone())
                .ok_or_else(|| {
                    RelayReject::new(
                        ErrorCode::UnknownRole,
                        format!("no [[route]] row carries gateway {gateway} channel {channel}"),
                        Some("route"),
                    )
                })
        }
        Principal::Cluster { cluster } => Err(RelayReject::new(
            ErrorCode::UnknownRole,
            format!("cluster {cluster} is not a routable target on this server"),
            Some("to.cluster"),
        )),
    }
}

/// Confirm the sender names a role or gateway this server knows.
pub fn resolve_sender(spec: &Spec, from: &Principal) -> Result<(), RelayReject> {
    match from {
        Principal::Role { role, .. } => {
            if spec.role_names().iter().any(|name| name == role) {
                Ok(())
            } else {
                Err(RelayReject::new(
                    ErrorCode::UnknownRole,
                    format!("unknown sender role {role}"),
                    Some("from.role"),
                ))
            }
        }
        Principal::Gateway { gateway, .. } => {
            if spec.gateway.iter().any(|entry| entry.id == *gateway) {
                Ok(())
            } else {
                Err(RelayReject::new(
                    ErrorCode::UnknownRole,
                    format!("unknown gateway {gateway}"),
                    Some("from.gateway"),
                ))
            }
        }
        Principal::Cluster { cluster } => Err(RelayReject::new(
            ErrorCode::NotAdmin,
            format!("cluster {cluster} requires an admin send"),
            Some("from.cluster"),
        )),
    }
}

/// One delivery question for the ACL gate.
#[derive(Debug, Clone, Copy)]
pub struct AclRequest<'a> {
    pub from: &'a Principal,
    pub to: &'a Principal,
    /// Role the target principal resolved to.
    pub to_role: &'a str,
    pub kind: MsgKind,
    pub admin: bool,
    /// Role that owns the task, when the envelope names one.
    pub owner: Option<&'a str>,
}

/// The ACL gate. A denial writes no ledger row.
pub fn check_acl(
    table: &onlyne_net::AclTable,
    spec: &Spec,
    request: &AclRequest<'_>,
) -> Result<(), RelayReject> {
    let AclRequest {
        from,
        to,
        to_role,
        kind,
        admin,
        owner,
    } = *request;
    match (from, to) {
        (Principal::Role { role, .. }, Principal::Role { .. }) => {
            acl_allows(table, role, to_role, class_of(kind), owner).map_err(|deny| {
                let code = match deny.reason {
                    onlyne_net::AclDenyReason::UnknownRole => ErrorCode::UnknownRole,
                    onlyne_net::AclDenyReason::AdminRequired => ErrorCode::Forbidden,
                    onlyne_net::AclDenyReason::SenderNotAllowed
                    | onlyne_net::AclDenyReason::TargetNotAllowed => ErrorCode::AclDenied,
                };
                RelayReject::new(code, deny.detail.clone(), Some(deny.field))
            })
        }
        (
            Principal::Role { role, .. },
            Principal::Gateway {
                gateway,
                conversation,
                ..
            },
        ) => {
            let reply_to = to_role;
            let outbound =
                select_outbound_route(spec, gateway, &Principal::role(role), Some(reply_to));
            match outbound {
                Some(_) => Ok(()),
                None => Err(RelayReject::new(
                    ErrorCode::AclDenied,
                    format!("role {role} has no [[route]] open to gateway {gateway}"),
                    Some("route"),
                )),
            }
            .map(|_| {
                let _ = conversation;
            })
        }
        (
            Principal::Gateway {
                gateway,
                channel,
                conversation,
            },
            Principal::Role { .. },
        ) => resolve_inbound_route(spec, gateway, channel, conversation.as_deref())
            .map(|_| ())
            .ok_or_else(|| {
                RelayReject::new(
                    ErrorCode::AclDenied,
                    format!("no [[route]] row carries gateway {gateway} channel {channel}"),
                    Some("route"),
                )
            }),
        (Principal::Gateway { gateway, .. }, Principal::Gateway { .. }) => Err(RelayReject::new(
            ErrorCode::AclDenied,
            format!("gateway {gateway} may not address another gateway"),
            Some("to.gateway"),
        )),
        (_, Principal::Cluster { cluster }) => Err(RelayReject::new(
            ErrorCode::UnknownRole,
            format!("cluster {cluster} is not reachable from this server"),
            Some("to.cluster"),
        )),
        (Principal::Cluster { .. }, _) => {
            if admin {
                Ok(())
            } else {
                Err(RelayReject::new(
                    ErrorCode::NotAdmin,
                    "a cluster principal requires admin = true",
                    Some("admin"),
                ))
            }
        }
    }
}

/// Run the full send path for one envelope.
pub fn send(
    state: &State,
    envelope: &Envelope,
    admin: bool,
    owner: Option<&str>,
) -> anyhow::Result<RelayReply> {
    if let Err(error) = envelope.validate() {
        return Ok(RelayReply::Rejected(RelayReject::new(
            ErrorCode::Invalid,
            error.message(),
            Some(error.field()),
        )));
    }
    let spec = state.spec_snapshot().context("the spec is unavailable")?;
    if let Err(reject) = resolve_sender(&spec, &envelope.from) {
        return Ok(RelayReply::Rejected(reject));
    }
    // A gateway process carries no routing policy (plan §5 line 243): the
    // `[[route]]` table owns the target role, so an inbound delivery resolves
    // through it even when its envelope names no role. Every other sender names
    // its target itself.
    let (to_role, routed) = match &envelope.from {
        Principal::Gateway {
            gateway,
            channel,
            conversation,
        } => {
            let Some(route) =
                resolve_inbound_route(&spec, gateway, channel, conversation.as_deref())
            else {
                return Ok(RelayReply::Rejected(RelayReject::new(
                    ErrorCode::AclDenied,
                    format!("no [[route]] row carries gateway {gateway} channel {channel}"),
                    Some("route"),
                )));
            };
            let role = route.to.role.clone();
            // The route decided the target, so the row records that decision:
            // the plugin's own `to` names the conversation it serves, and a row
            // keeping it would leave the resolved role unable to pull its work.
            let mut routed = envelope.clone();
            routed.to = match route.to.session.clone() {
                Some(session) => Principal::role_session(role.as_str(), session),
                None => Principal::role(role.as_str()),
            };
            (role, routed)
        }
        _ => match resolve_target(&spec, &envelope.to) {
            Ok(role) => (role, envelope.clone()),
            Err(reject) => return Ok(RelayReply::Rejected(reject)),
        },
    };
    let envelope = &routed;
    let table = state.acl_table();
    if let Err(reject) = check_acl(
        &table,
        &spec,
        &AclRequest {
            from: &envelope.from,
            to: &envelope.to,
            to_role: &to_role,
            kind: envelope.kind,
            admin,
            owner,
        },
    ) {
        return Ok(RelayReply::Rejected(reject));
    }
    let online = state.is_connected(to_role.as_str());
    if envelope.kind == MsgKind::Note && !spec.server.note_queue && !online {
        return Ok(RelayReply::Rejected(RelayReject::new(
            ErrorCode::RecipientOffline,
            format!("recipient role {to_role} is offline"),
            Some("to.role"),
        )));
    }
    // A platform target leaves through the gateway pump, so the row waits in
    // `queued` until that push happens: presence on the role side says nothing
    // about a conversation (plan §7 line 293).
    let to_gateway = matches!(envelope.to, Principal::Gateway { .. });
    let fingerprint = envelope.fingerprint();
    let row = LedgerRow::from_envelope(envelope, &fingerprint)?;
    match state.ledger.append_ledger(&row)? {
        Append::Duplicate {
            existing,
            fingerprint_matches,
        } => {
            if fingerprint_matches {
                Ok(RelayReply::Duplicate(Box::new(SendOutcome {
                    pump_outbound: to_gateway,
                    receipt: receipt_from(&existing),
                    online,
                    row: existing,
                })))
            } else {
                Ok(RelayReply::Rejected(RelayReject::new(
                    ErrorCode::Conflict,
                    OP_ID_CONFLICT_MESSAGE,
                    Some("op_id"),
                )))
            }
        }
        Append::Accepted(row) => {
            if let Some(ttl_ms) = envelope.ttl_ms {
                let deadline = envelope.ts + chrono::Duration::milliseconds(ttl_ms as i64);
                state.note_expiry(&row.msg_id, deadline);
            }
            let deliverable = online && !to_gateway;
            let delivered_state = if deliverable {
                state.ledger.mark_in_flight(&row.msg_id)?;
                LedgerState::InFlight
            } else {
                LedgerState::Queued
            };
            let event = ledger_event(&row, delivered_state, None, None);
            let seq = events::publish(state, Event::LedgerState(event))?;
            if deliverable {
                push_delivery(state, &to_role, seq, &row, delivered_state);
            }
            // A row aimed at a platform conversation leaves as a rendered push.
            // `gateway_host::pump_outbound` covers it, and the router calls that
            // after every accepted send (plan §7 line 293's outbound direction).
            Ok(RelayReply::Accepted(Box::new(SendOutcome {
                pump_outbound: to_gateway,
                receipt: Receipt {
                    msg_id: row.msg_id.clone(),
                    op_id: row.op_id.clone(),
                    kind: row.kind,
                    task: row.task.clone(),
                    state: delivered_state,
                    enqueued_at: parse_time(&row.enqueued_at),
                },
                row,
                online,
            })))
        }
    }
}

/// Notify a connected role that a row is waiting for its `pull`.
fn push_delivery(state: &State, role: &str, seq: u64, row: &LedgerRow, ledger_state: LedgerState) {
    let Some(sender) = state.role_sender(role) else {
        return;
    };
    let event = Event::LedgerState(ledger_event(row, ledger_state, None, None));
    let _ = sender.try_send(onlyne_proto::Frame::event(seq, event));
}

/// Hand at most one unacknowledged row per session, arming its ticket.
pub fn pull(
    state: &State,
    role: &str,
    session_id: Option<&str>,
    args: &PullArgs,
) -> anyhow::Result<PullReply> {
    let head = state.event_head().max(0) as u64;
    if state.open_delivery(role, session_id).is_some() {
        return Ok(PullReply {
            deliveries: Vec::new(),
            seq: head,
        });
    }
    // `note` rows are logged and expired, never pulled: §3 line 152 gives them
    // no session, and the client's accept path refuses an envelope without a
    // task, which would turn a chat line into a rejection.
    let mut candidates: Vec<_> = state
        .ledger
        .queued_for(role, 8)?
        .into_iter()
        .filter(|row| row.kind != MsgKind::Note)
        .collect();
    for row in state.ledger.in_flight_for(role)? {
        if state.open_delivery(role, session_id).is_none() && candidates.is_empty() {
            candidates.push(row);
            break;
        }
    }
    candidates.sort_by(|left, right| left.enqueued_at.cmp(&right.enqueued_at));
    let Some(row) = candidates.into_iter().next() else {
        return Ok(PullReply {
            deliveries: Vec::new(),
            seq: head,
        });
    };
    if row.state == LedgerState::Queued {
        state.ledger.mark_in_flight(&row.msg_id)?;
    }
    let session_row = row
        .task
        .as_deref()
        .map(|task| state.ledger.get_session_row(task))
        .transpose()?
        .flatten();
    let ticket = DeliveryTicket {
        msg_id: row.msg_id.clone(),
        role: role.to_string(),
        session_id: session_id
            .map(str::to_string)
            .or_else(|| session_row.as_ref().map(|row| row.session_id.clone())),
        generation: session_row
            .as_ref()
            .map(|row| row.generation.max(0) as u64)
            .unwrap_or(0),
        seq: head,
        delivered_at: Utc::now(),
    };
    state.record_delivery(ticket);
    state
        .ledger
        .set_cursor(role, Some(row.msg_id.as_str()), head as i64)?;
    let _ = args;
    Ok(PullReply {
        deliveries: vec![delivery_from(&row)?],
        seq: head,
    })
}

/// Settle one delivery, moving it to `acked` or `rejected`.
pub fn ack(state: &State, args: &AckArgs) -> anyhow::Result<Result<LedgerStateEvent, RelayReject>> {
    let Some(row) = state
        .ledger
        .ledger_query(LedgerQuery {
            msg_id: Some(args.msg_id.clone()),
            limit: 1,
            ..LedgerQuery::default()
        })?
        .into_iter()
        .next()
    else {
        return Ok(Err(RelayReject::new(
            ErrorCode::Invalid,
            format!("unknown msg_id {}", args.msg_id),
            Some("msg_id"),
        )));
    };
    let ticket = state.take_delivery(&args.msg_id);
    // An ack is at-least-once (D11), so a repeated ack of a row already settled
    // answers with the state it holds instead of failing the transition. The
    // client's durable ack intent is what makes the repeat normal.
    if matches!(
        row.state,
        LedgerState::Acked | LedgerState::Rejected | LedgerState::Expired
    ) {
        let event = ledger_event(&row, row.state, None, args.reason.clone());
        return Ok(Ok(event));
    }
    if row.state == LedgerState::Queued {
        state.ledger.mark_in_flight(&row.msg_id)?;
    }
    let settled = if args.accepted {
        state.ledger.mark_acked(&row.msg_id, Utc::now())?;
        LedgerState::Acked
    } else {
        state
            .ledger
            .mark_rejected(&row.msg_id, args.reason.as_deref().unwrap_or("rejected"))?;
        LedgerState::Rejected
    };
    state.forget_expiry(&row.msg_id);
    let generation = ticket.as_ref().map(|ticket| ticket.generation).unwrap_or(0);
    let event = ledger_event(&row, settled, None, args.reason.clone());
    events::publish(state, Event::LedgerState(event.clone()))?;
    let _ = generation;
    Ok(Ok(event))
}

/// A delivery reachable by `pull`, rebuilt from its durable row.
pub fn delivery_from(row: &LedgerRow) -> anyhow::Result<Delivery> {
    let from = row.sender().context("decode ledger sender")?;
    let to: Principal = serde_json::from_str(&row.to_json).context("decode ledger target")?;
    let body: Body = match row.body_json.as_deref() {
        Some(text) => serde_json::from_str(text).context("decode ledger body")?,
        None => Body::default(),
    };
    let causality = row.task.as_ref().map(|task| Causality {
        task: task.clone(),
        parent_task: row.parent_task.clone(),
        reply_to: None,
        hop: row.hop.max(0) as u32,
        attempt: row.attempt.max(0) as u32,
    });
    Ok(Delivery {
        msg_id: row.msg_id.clone(),
        envelope: Box::new(Envelope {
            protocol: PROTOCOL_VERSION,
            id: row.msg_id.clone(),
            op_id: row.op_id.clone(),
            kind: row.kind,
            from,
            to,
            control: None,
            causality,
            body,
            ts: parse_time(&row.enqueued_at),
            ttl_ms: None,
            admin: false,
        }),
    })
}

/// Re-queue a role's in-flight rows when its connection drops.
///
/// `ServerLedger::requeue_one` moves one row and publishes its `ledger_state`
/// event in the same transaction, so an operator watching the observation plane
/// sees the requeue that follows a dropped link. A row that settled between the
/// read and the move is left alone.
pub fn disconnect(state: &State, role: &str) -> anyhow::Result<usize> {
    let mut requeued = 0usize;
    for row in state.ledger.in_flight_for(role)? {
        match state.ledger.requeue_one(&row.msg_id) {
            Ok(_) => requeued += 1,
            Err(onlyne_store::StoreError::InvalidState { .. }) => {}
            Err(error) => return Err(error.into()),
        }
    }
    state.clear_deliveries(role);
    state.unregister_role(role);
    state.emit(Event::RolePresence(RolePresence {
        role: role.to_string(),
        state: Presence::Offline,
        aggregate: None,
        sessions: 0,
        detail: None,
    }))?;
    Ok(requeued)
}

/// Settle every queued note whose deadline passed.
///
/// `ServerLedger::expire_one` writes the row and its `ledger_state` event in one
/// transaction, so the observation plane sees each expiry exactly once.
pub fn sweep_expired(state: &State, now: DateTime<Utc>) -> anyhow::Result<Vec<String>> {
    let mut expired = Vec::new();
    for msg_id in state.due_expiries(now) {
        match state.ledger.expire_one(&msg_id, "expired") {
            Ok(_row) => {
                state.forget_expiry(&msg_id);
                expired.push(msg_id);
            }
            Err(onlyne_store::StoreError::InvalidState { .. }) => {
                state.forget_expiry(&msg_id);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(expired)
}

/// The durable `ledger_state` event with its nine observables.
pub fn ledger_event(
    row: &LedgerRow,
    state: LedgerState,
    outcome: Option<Outcome>,
    reason: Option<String>,
) -> LedgerStateEvent {
    LedgerStateEvent {
        msg_id: row.msg_id.clone(),
        op_id: row.op_id.clone(),
        kind: row.kind,
        from: row.sender().unwrap_or_else(|_| Principal::role("unknown")),
        to: serde_json::from_str(&row.to_json).unwrap_or_else(|_| Principal::role("unknown")),
        task: row.task.clone(),
        state,
        outcome,
        reason,
    }
}

/// Project one durable ledger row onto the observable snapshot.
pub fn entry_from_row(row: &LedgerRow) -> onlyne_proto::LedgerEntry {
    onlyne_proto::LedgerEntry {
        msg_id: row.msg_id.clone(),
        op_id: row.op_id.clone(),
        kind: row.kind,
        from: row.sender().unwrap_or_else(|_| Principal::role("unknown")),
        to: serde_json::from_str(&row.to_json).unwrap_or_else(|_| Principal::role("unknown")),
        task: row.task.clone(),
        parent_task: row.parent_task.clone(),
        hop: row.hop.max(0) as u32,
        attempt: row.attempt.max(0) as u32,
        state: row.state,
        out_head: row.out_head.clone(),
        body_json: row.body_json.clone(),
        enqueued_at: parse_time(&row.enqueued_at),
        acked_at: row
            .acked_at
            .as_deref()
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|value| value.with_timezone(&Utc)),
    }
}

/// Wire form of one receipt.
pub fn receipt_json(receipt: &Receipt) -> Value {
    serde_json::to_value(receipt).unwrap_or(Value::Null)
}

/// Receipt rebuilt from a durable row.
///
/// A replay answers with the stored value field for field, which is what
/// Verification case 3 compares (plan line 502). Both answers therefore
/// serialize identically for one `op_id`.
pub fn receipt_from(row: &LedgerRow) -> Receipt {
    Receipt {
        msg_id: row.msg_id.clone(),
        op_id: row.op_id.clone(),
        kind: row.kind,
        task: row.task.clone(),
        state: row.state,
        enqueued_at: parse_time(&row.enqueued_at),
    }
}

fn parse_time(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

/// The role that owns a task, used as the ACL owner for control ops.
pub fn task_owner(state: &State, task_id: &str) -> Option<String> {
    state
        .ledger
        .get_session_row(task_id)
        .ok()
        .flatten()
        .map(|row| row.role)
}
