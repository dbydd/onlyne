//! The op vocabularies (§6, §8).
//!
//! Three closed sets: role/gateway traffic into the server ([`ClientOp`]), the
//! local admin surface ([`AdminOp`]), and the adapter protocol's payloads, which
//! live in [`crate::adapter`].

use crate::envelope::{Envelope, MsgKind, Outcome, Principal};
use crate::event::{EventTier, LedgerState, Lifecycle};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Server's durable answer to an accepted send (§8 `send`, §10 `ledger`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Receipt {
    pub msg_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,
    pub kind: MsgKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub state: LedgerState,
    pub enqueued_at: DateTime<Utc>,
    /// Set when the server replayed an existing receipt for a duplicate
    /// `op_id`; the payload is byte-identical to the first answer.
    pub duplicate: bool,
}

/// Agent-side lifecycle fact, as reported by the plugin and stored verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    #[default]
    Booting,
    Ready,
    Running,
    Idle,
    Gone,
}

/// Intent delivery fact for the current turn exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPhase {
    #[serde(rename = "none")]
    #[default]
    NoIntent,
    Pending,
    Retrying,
    Accepted,
    Exhausted,
}

/// Backend resource fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePhase {
    #[default]
    Detached,
    Attached,
    Closing,
    Closed,
}

/// Recovery substate of an idle or draining session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPhase {
    #[serde(rename = "none")]
    #[default]
    NoRecovery,
    IdleWaiting,
    IdleFault,
    Draining,
}

/// The full projection the client publishes for one session (§10 `sessions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SessionProjection {
    pub lifecycle: Lifecycle,
    pub agent: AgentPhase,
    pub delivery: DeliveryPhase,
    pub resource: ResourcePhase,
    pub recovery: RecoveryPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// Raw reducer observation, retained for repair and forensics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<Value>,
}

impl SessionProjection {
    /// A freshly created session with nothing attached yet.
    pub fn default_working() -> Self {
        SessionProjection {
            lifecycle: Lifecycle::Created,
            agent: AgentPhase::Booting,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Detached,
            recovery: RecoveryPhase::NoRecovery,
            outcome: None,
            observed: None,
        }
    }
}

/// The report envelope kinds a client pushes (§6, §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum Report {
    /// The ready barrier passed; the server may now deliver the payload.
    Ready {
        task_id: String,
        session_id: String,
        generation: u64,
        seq: u64,
    },
    /// Liveness plus the reducer's own view of the world.
    Heartbeat {
        task_id: String,
        generation: u64,
        seq: u64,
        observed: Value,
    },
    /// Terminal result for a task; `head` is the summary the ledger keeps.
    Complete {
        task_id: String,
        outcome: Outcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
    },
    /// Something needs a supervisor decision. The server records and forwards.
    Fault {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        generation: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        kind: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        desired: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed: Option<Value>,
    },
}

impl Report {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Report::Ready { .. } => "ready",
            Report::Heartbeat { .. } => "heartbeat",
            Report::Complete { .. } => "complete",
            Report::Fault { .. } => "fault",
        }
    }

    pub fn task_id(&self) -> Option<&str> {
        match self {
            Report::Ready { task_id, .. }
            | Report::Heartbeat { task_id, .. }
            | Report::Complete { task_id, .. } => Some(task_id),
            Report::Fault { task_id, .. } => task_id.as_deref(),
        }
    }

    /// `(generation, seq)` watermark this report asserts, when it carries one.
    pub fn version(&self) -> Option<(u64, u64)> {
        match self {
            Report::Ready {
                generation, seq, ..
            } => Some((*generation, *seq)),
            Report::Heartbeat { generation, seq, .. } => Some((*generation, *seq)),
            Report::Fault {
                generation,
                seq,
                ..
            } => Some((
                (*generation).unwrap_or_default(),
                (*seq).unwrap_or_default(),
            )),
            Report::Complete { .. } => None,
        }
    }
}

/// `pull` request: drain what the server has queued for this connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct PullArgs {
    /// Narrow to one role on a multi-role aggregate connection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub limit: u32,
    /// Server-side long-poll window in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<u64>,
}

/// One delivered envelope plus its ledger handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Delivery {
    pub msg_id: String,
    pub envelope: Box<Envelope>,
}

/// `pull` reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PullReply {
    pub deliveries: Vec<Delivery>,
    /// Event cursor to carry into the next `subscribe`.
    pub seq: u64,
}

/// `ack` request: this delivery reached a terminal local decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AckArgs {
    pub msg_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,
    /// False settles the row `rejected` with `reason` recorded.
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `subscribe` request: start (or resume) the observation stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct Subscribe {
    /// Resume after this cursor; `0` starts from the current head.
    pub since_seq: u64,
    pub tiers: Vec<EventTier>,
    /// Restrict to these event type names; empty means all.
    pub kinds: Vec<String>,
    /// Restrict to these roles; empty means the whole cluster.
    pub roles: Vec<String>,
}

/// `query_ledger` filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct LedgerQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<LedgerState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<MsgKind>,
    pub limit: u32,
}

/// `query_sessions` filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct QuerySessionsArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<Lifecycle>,
    pub limit: u32,
}

/// `query_roles` filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct QueryRolesArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// `query_faults` filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct QueryFaultsArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Only faults still needing a decision.
    pub open_only: bool,
    pub limit: u32,
}

/// `control` request: a control op aimed at a role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ControlArgs {
    /// Target role; omitted means the role that owns the task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub op: crate::envelope::ControlOp,
}

/// `bye` request: the client says goodbye with a reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ByeArgs {
    pub reason: String,
    /// Seconds the client intends to keep draining before it disconnects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drain_ms: Option<u64>,
}

/// `session_sync` request: publish a full projection for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SessionSyncArgs {
    pub task_id: String,
    pub session_id: String,
    pub generation: u64,
    pub seq: u64,
    pub projection: SessionProjection,
}

/// Handshake request on a role connection (§5, §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct HandshakeArgs {
    pub protocol: u16,
    /// The role this connection serves, or the aggregate role it represents.
    pub role: String,
    /// `ed25519/<base64>` public key the connection presents.
    pub key: String,
    /// Base64 signature over the server challenge (§5 handshake).
    pub signature: String,
    /// Software name and version, retained for fault triage.
    pub agent: String,
    pub version: String,
    /// True when the connection serves an aggregate role for a sub-cluster.
    pub aggregate: bool,
}

/// Client-to-server vocabulary (§8). One `match` in the server router.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum ClientOp {
    /// Establish identity and pull the role's spec slice.
    Hello(HandshakeArgs),
    /// Submit an envelope for routing.
    Send(Box<Envelope>),
    /// Drain queued deliveries.
    Pull(PullArgs),
    /// Settle a delivery.
    Ack(AckArgs),
    /// Push a lifecycle report.
    Report(Report),
    /// Publish a session projection mirror.
    SessionSync(SessionSyncArgs),
    /// Start or resume the observation stream.
    Subscribe(Subscribe),
    /// Read the ledger.
    QueryLedger(LedgerQuery),
    /// Read the session projection table.
    QuerySessions(QuerySessionsArgs),
    /// Read registered roles and their presence.
    QueryRoles(QueryRolesArgs),
    /// Read recorded faults.
    QueryFaults(QueryFaultsArgs),
    /// Issue recycle / probe / snapshot / cancel.
    Control(ControlArgs),
    /// Ordered shutdown.
    Bye(ByeArgs),
}

impl ClientOp {
    pub fn name(&self) -> &'static str {
        match self {
            ClientOp::Hello(_) => "hello",
            ClientOp::Send(_) => "send",
            ClientOp::Pull(_) => "pull",
            ClientOp::Ack(_) => "ack",
            ClientOp::Report(_) => "report",
            ClientOp::SessionSync(_) => "session_sync",
            ClientOp::Subscribe(_) => "subscribe",
            ClientOp::QueryLedger(_) => "query_ledger",
            ClientOp::QuerySessions(_) => "query_sessions",
            ClientOp::QueryRoles(_) => "query_roles",
            ClientOp::QueryFaults(_) => "query_faults",
            ClientOp::Control(_) => "control",
            ClientOp::Bye(_) => "bye",
        }
    }

    /// Ops usable before the handshake completes.
    pub fn is_pre_auth(self) -> bool {
        matches!(self, ClientOp::Hello(_))
    }

    /// Read-only ops; anything else mutates cluster state.
    pub fn is_readonly(self) -> bool {
        matches!(
            self,
            ClientOp::QueryLedger(_)
                | ClientOp::QuerySessions(_)
                | ClientOp::QueryRoles(_)
                | ClientOp::QueryFaults(_)
                | ClientOp::Subscribe(_)
                | ClientOp::Pull(_)
        )
    }
}

/// The `hello` reply: the role's slice of the spec (§5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Welcome {
    pub cluster: String,
    pub server: String,
    pub role: String,
    pub admin: bool,
    pub max_sessions: u32,
    pub reuse: bool,
    pub prose: String,
    pub spec_hash: String,
    pub allowed_targets: Vec<String>,
    pub allowed_senders: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ready_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_running_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_idle_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_backoff_ms: Option<Vec<u64>>,
    /// Server event cursor at handshake time.
    pub seq: u64,
}

/// Repair verbs on the admin surface (§8). Each one is a transactional ledger
/// edit; none of them runs automatically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum AdminOp {
    /// Cluster summary: roles, gateways, queue depth, event head.
    Status(Value),
    Roles(QueryRolesArgs),
    Sessions(QuerySessionsArgs),
    Ledger(LedgerQuery),
    Faults(QueryFaultsArgs),
    /// Stream events until the connection closes.
    Watch(Subscribe),
    /// Paged history over `events`.
    History(HistoryArgs),
    /// Diff the in-memory spec against the file on disk.
    SpecDiff(Value),
    /// Re-read and validate `spec.toml`.
    Reload(Value),
    /// Submit an envelope on behalf of `--from <role>`.
    Send(AdminSend),
    /// Issue a control op as `--from <role>`.
    Control(AdminControl),
    /// Read a session's reducer state without changing it.
    RepairInspect(RepairTarget),
    /// Attach an existing live resource to a session row.
    RepairAdopt(RepairAdopt),
    /// Point a session row at a new backend reference.
    RepairRebind(RepairRebind),
    /// Re-queue a settled or faulted task once.
    RepairRetry(RepairTarget),
    /// Settle a task as failed.
    RepairFail(RepairFail),
    /// Close a session's resource and settle its task.
    RepairClose(RepairTarget),
    /// Mark a fault handled.
    RepairAck(RepairAck),
    /// Ordered shutdown of the server.
    Shutdown(ShutdownArgs),
}

impl AdminOp {
    pub fn name(&self) -> &'static str {
        match self {
            AdminOp::Status(_) => "status",
            AdminOp::Roles(_) => "roles",
            AdminOp::Sessions(_) => "sessions",
            AdminOp::Ledger(_) => "ledger",
            AdminOp::Faults(_) => "faults",
            AdminOp::Watch(_) => "watch",
            AdminOp::History(_) => "history",
            AdminOp::SpecDiff(_) => "spec_diff",
            AdminOp::Reload(_) => "reload",
            AdminOp::Send(_) => "send",
            AdminOp::Control(_) => "control",
            AdminOp::RepairInspect(_) => "repair_inspect",
            AdminOp::RepairAdopt(_) => "repair_adopt",
            AdminOp::RepairRebind(_) => "repair_rebind",
            AdminOp::RepairRetry(_) => "repair_retry",
            AdminOp::RepairFail(_) => "repair_fail",
            AdminOp::RepairClose(_) => "repair_close",
            AdminOp::RepairAck(_) => "repair_ack",
            AdminOp::Shutdown(_) => "shutdown",
        }
    }

    pub fn is_readonly(self) -> bool {
        matches!(
            self,
            AdminOp::Status(_)
                | AdminOp::Roles(_)
                | AdminOp::Sessions(_)
                | AdminOp::Ledger(_)
                | AdminOp::Faults(_)
                | AdminOp::Watch(_)
                | AdminOp::History(_)
                | AdminOp::SpecDiff(_)
                | AdminOp::RepairInspect(_)
        )
    }
}

/// `history` request: read persisted events by cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct HistoryArgs {
    pub since_seq: u64,
    pub limit: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

/// Admin `send`: the operator names the sender, and the row records `admin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AdminSend {
    /// Must be a role registered in `spec.toml`.
    pub from: String,
    pub envelope: Box<Envelope>,
}

/// Admin `control`: same ownership rule as [`AdminSend`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AdminControl {
    pub from: String,
    pub op: crate::envelope::ControlOp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// Task-addressed repair verb.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct RepairTarget {
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Adopt an already-running resource into a session row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct RepairAdopt {
    pub task_id: String,
    pub session_id: String,
    pub backend: String,
    pub backend_ref: Value,
    pub reason: String,
}

/// Replace a session's backend reference and bump its generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct RepairRebind {
    pub task_id: String,
    pub session_id: String,
    pub backend: String,
    pub backend_ref: Value,
    pub reason: String,
}

/// Settle a task failed, with an outcome reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct RepairFail {
    pub task_id: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<Principal>,
}

/// Close a fault record as handled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct RepairAck {
    pub fault_id: i64,
    pub reason: String,
}

/// Shutdown request on the admin socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ShutdownArgs {
    pub reason: String,
    /// Let connected clients drain for this long before the listener closes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_ms: Option<u64>,
}

/// The gateway-to-server vocabulary (§8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum GatewayOp {
    /// Establish gateway identity from `[[gateway]]`.
    Hello(HandshakeArgs),
    /// Declare the platform channels this gateway serves.
    RegisterChannel(RegisterChannelArgs),
    /// Push an inbound platform event into routing.
    Deliver(Delivery),
    /// Report platform health.
    Health(HealthArgs),
    /// Ordered shutdown.
    Bye(ByeArgs),
}

impl GatewayOp {
    pub fn name(&self) -> &'static str {
        match self {
            GatewayOp::Hello(_) => "hello",
            GatewayOp::RegisterChannel(_) => "register_channel",
            GatewayOp::Deliver(_) => "deliver",
            GatewayOp::Health(_) => "health",
            GatewayOp::Bye(_) => "bye",
        }
    }
}

/// Channel declarations plus the optional conversation list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct RegisterChannelArgs {
    pub platform: String,
    pub channel: String,
    /// Absent when the platform cannot enumerate conversations (§10.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversations: Option<Vec<ConversationInfo>>,
}

/// One externally addressable conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ConversationInfo {
    pub conversation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Gateway health report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct HealthArgs {
    /// `online`, `reconnecting`, `failed`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Seconds since the gateway process started.
    pub uptime_s: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Body, Causality, new_task_id, MsgKind};
    use crate::{PROTOCOL_VERSION, envelope};

    fn task() -> Envelope {
        envelope::new_envelope(
            MsgKind::Task,
            Principal::role("planner"),
            Principal::role("builder"),
            Body::text("work"),
            Some(Causality::root(new_task_id())),
        )
        .expect("task")
    }

    #[test]
    fn client_ops_encode_as_op_and_args() {
        let cases: Vec<(ClientOp, &str)> = vec![
            (
                ClientOp::Hello(HandshakeArgs {
                    protocol: PROTOCOL_VERSION,
                    role: "planner".into(),
                    key: "ed25519/AAA".into(),
                    signature: "sig".into(),
                    agent: "onlyne-client".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    aggregate: false,
                }),
                "hello",
            ),
            (ClientOp::Send(Box::new(task())), "send"),
            (
                ClientOp::Pull(PullArgs {
                    role: None,
                    limit: 32,
                    hold_ms: Some(250),
                }),
                "pull",
            ),
            (
                ClientOp::Ack(AckArgs {
                    msg_id: "m1".into(),
                    op_id: None,
                    accepted: true,
                    reason: None,
                }),
                "ack",
            ),
            (
                ClientOp::Report(Report::Ready {
                    task_id: new_task_id(),
                    session_id: "s".into(),
                    generation: 1,
                    seq: 3,
                }),
                "report",
            ),
            (
                ClientOp::SessionSync(SessionSyncArgs {
                    task_id: new_task_id(),
                    session_id: "s".into(),
                    generation: 1,
                    seq: 4,
                    projection: SessionProjection::default_working(),
                }),
                "session_sync",
            ),
            (
                ClientOp::Subscribe(Subscribe {
                    since_seq: 7,
                    tiers: vec![EventTier::Durable],
                    kinds: vec![],
                    roles: vec![],
                }),
                "subscribe",
            ),
            (ClientOp::QueryLedger(LedgerQuery::default()), "query_ledger"),
            (
                ClientOp::QuerySessions(QuerySessionsArgs::default()),
                "query_sessions",
            ),
            (ClientOp::QueryRoles(QueryRolesArgs::default()), "query_roles"),
            (ClientOp::QueryFaults(QueryFaultsArgs::default()), "query_faults"),
            (
                ClientOp::Control(ControlArgs {
                    to: Some("builder".into()),
                    op: crate::envelope::ControlOp::Probe { task_id: new_task_id() },
                }),
                "control",
            ),
            (
                ClientOp::Bye(ByeArgs {
                    reason: "shutdown".into(),
                    drain_ms: None,
                }),
                "bye",
            ),
        ];
        for (op, name) in &cases {
            assert_eq!(op.name(), *name);
            let value = serde_json::to_value(op).expect("encode");
            assert_eq!(value["op"], *name, "{op:?}");
            assert!(value.get("args").is_some(), "{op:?} carries args");
            let back: ClientOp = serde_json::from_value(value).expect("decode");
            assert_eq!(&back, op);
        }
        assert_eq!(cases.len(), 13);
    }

    #[test]
    fn unknown_op_name_is_a_decode_failure() {
        let err = serde_json::from_value::<ClientOp>(serde_json::json!({"op":"loopback","args":{}}))
            .expect_err("closed vocabulary");
        assert!(err.to_string().contains("loopback"), "err = {err}");
    }

    #[test]
    fn pre_auth_and_readonly_gates_are_exact() {
        let hello = ClientOp::Hello(HandshakeArgs::default());
        assert!(hello.clone().is_pre_auth());
        assert!(!hello.is_readonly());
        assert!(ClientOp::Pull(PullArgs::default()).is_readonly());
        assert!(!ClientOp::Send(Box::new(task())).is_readonly());
        assert!(!ClientOp::Report(Report::Ready {
            task_id: new_task_id(),
            session_id: "s".into(),
            generation: 1,
            seq: 1
        })
        .is_readonly());
    }

    #[test]
    fn report_versions_expose_the_monotonic_pair() {
        let ready = Report::Ready {
            task_id: new_task_id(),
            session_id: "s".into(),
            generation: 2,
            seq: 9,
        };
        assert_eq!(ready.version(), Some((2, 9)));
        assert_eq!(ready.kind_name(), "ready");
        let fault = Report::Fault {
            task_id: None,
            session_id: None,
            generation: None,
            seq: None,
            kind: "intent_exhausted".into(),
            reason: "peer gone".into(),
            desired: None,
            observed: None,
        };
        assert_eq!(fault.version(), Some((0, 0)));
        assert_eq!(fault.task_id(), None);
        let value = serde_json::to_value(&fault).expect("encode");
        assert_eq!(value["kind"], "fault");
        assert_eq!(value["data"]["kind"], "intent_exhausted");
    }

    #[test]
    fn session_projection_defaults_to_created_and_detached() {
        let value =
            serde_json::to_value(SessionProjection::default_working()).expect("encode");
        assert_eq!(value["lifecycle"], "created");
        assert_eq!(value["agent"], "booting");
        assert_eq!(value["delivery"], "none");
        assert_eq!(value["resource"], "detached");
        assert_eq!(value["recovery"], "none");
        assert!(value.get("outcome").is_none());
    }

    #[test]
    fn admin_vocabulary_is_closed_and_named() {
        let ops: Vec<(AdminOp, &str)> = vec![
            (AdminOp::Status(Value::Null), "status"),
            (
                AdminOp::Roles(QueryRolesArgs::default()),
                "roles",
            ),
            (
                AdminOp::Sessions(QuerySessionsArgs::default()),
                "sessions",
            ),
            (AdminOp::Ledger(LedgerQuery::default()), "ledger"),
            (AdminOp::Faults(QueryFaultsArgs::default()), "faults"),
            (AdminOp::Watch(Subscribe::default()), "watch"),
            (AdminOp::History(HistoryArgs::default()), "history"),
            (AdminOp::SpecDiff(Value::Null), "spec_diff"),
            (AdminOp::Reload(Value::Null), "reload"),
            (
                AdminOp::Send(AdminSend {
                    from: "planner".into(),
                    envelope: Box::new(task()),
                }),
                "send",
            ),
            (
                AdminOp::Control(AdminControl {
                    from: "planner".into(),
                    op: crate::envelope::ControlOp::Snapshot { task_id: new_task_id() },
                    to: None,
                }),
                "control",
            ),
            (
                AdminOp::RepairInspect(RepairTarget {
                    task_id: new_task_id(),
                    reason: None,
                }),
                "repair_inspect",
            ),
            (
                AdminOp::RepairAdopt(RepairAdopt {
                    task_id: new_task_id(),
                    session_id: "s".into(),
                    backend: "orca".into(),
                    backend_ref: serde_json::json!({"pane": 3}),
                    reason: "attested".into(),
                }),
                "repair_adopt",
            ),
            (
                AdminOp::RepairRebind(RepairRebind {
                    task_id: new_task_id(),
                    session_id: "s".into(),
                    backend: "orca".into(),
                    backend_ref: serde_json::json!({"pane": 4}),
                    reason: "moved".into(),
                }),
                "repair_rebind",
            ),
            (
                AdminOp::RepairRetry(RepairTarget {
                    task_id: new_task_id(),
                    reason: Some("operator".into()),
                }),
                "repair_retry",
            ),
            (
                AdminOp::RepairFail(RepairFail {
                    task_id: new_task_id(),
                    reason: "dead".into(),
                    notify: None,
                }),
                "repair_fail",
            ),
            (
                AdminOp::RepairClose(RepairTarget {
                    task_id: new_task_id(),
                    reason: None,
                }),
                "repair_close",
            ),
            (
                AdminOp::RepairAck(RepairAck {
                    fault_id: 12,
                    reason: "handled".into(),
                }),
                "repair_ack",
            ),
            (
                AdminOp::Shutdown(ShutdownArgs {
                    reason: "operator".into(),
                    grace_ms: Some(500),
                }),
                "shutdown",
            ),
        ];
        for (op, name) in &ops {
            assert_eq!(op.name(), *name);
            let value = serde_json::to_value(op).expect("encode");
            assert_eq!(value["op"], *name);
            let back: AdminOp = serde_json::from_value(value).expect("decode");
            assert_eq!(&back, op);
        }
        assert!(AdminOp::Status(Value::Null).is_readonly());
        assert!(!AdminOp::Reload(Value::Null).is_readonly());
    }

    #[test]
    fn gateway_vocabulary_covers_the_five_ops() {
        let ops = [
            GatewayOp::Hello(HandshakeArgs::default()),
            GatewayOp::RegisterChannel(RegisterChannelArgs {
                platform: "telegram".into(),
                channel: "telegram".into(),
                conversations: None,
            }),
            GatewayOp::Deliver(Delivery {
                msg_id: "m".into(),
                envelope: Box::new(task()),
            }),
            GatewayOp::Health(HealthArgs {
                state: "online".into(),
                detail: None,
                uptime_s: 12,
            }),
            GatewayOp::Bye(ByeArgs {
                reason: "shutdown".into(),
                drain_ms: None,
            }),
        ];
        for op in ops {
            let value = serde_json::to_value(&op).expect("encode");
            assert_eq!(value["op"], op.name());
            let back: GatewayOp = serde_json::from_value(value).expect("decode");
            assert_eq!(back, op);
        }
    }

    #[test]
    fn receipt_state_and_kind_use_wire_names() {
        let receipt = Receipt {
            msg_id: "m1".into(),
            op_id: Some("o-x".into()),
            kind: MsgKind::Task,
            task: Some(new_task_id()),
            state: LedgerState::InFlight,
            enqueued_at: Utc::now(),
            duplicate: false,
        };
        let value = serde_json::to_value(&receipt).expect("encode");
        assert_eq!(value["state"], "in_flight");
        assert_eq!(value["kind"], "task");
        assert_eq!(value["duplicate"], false);
    }
}
