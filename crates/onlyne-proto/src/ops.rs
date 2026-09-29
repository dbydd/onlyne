//! The op vocabularies (§6, §8).
//!
//! Three closed sets: role/gateway traffic into the server ([`ClientOp`]), the
//! local admin surface ([`AdminOp`]), and the adapter protocol's payloads, which
//! live in [`crate::adapter`].

use crate::envelope::{Envelope, MsgKind, Outcome, Principal};
use crate::event::{EventTier, LedgerState, Lifecycle, Presence};
use crate::lifecycle::{AgentPhase, DeliveryPhase, RecoveryPhase, ResourcePhase};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Whether a count is zero, so its key stays off the wire. A count that says
/// nothing — no queued work, no hops owed — costs a client a key it has to
/// ignore, and an old client never learns the field exists.
fn is_zero(count: &u32) -> bool {
    *count == 0
}

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
}

/// The full projection the client publishes for one session (§10 `sessions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SessionProjection {
    #[serde(alias = "public_lifecycle")]
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
///
/// `cluster_ref` names the origin cluster on a relayed report, so a federated
/// projection can say where it came from. Only the three state-carrying kinds
/// hold it: `Fault` carries no projection, and a ninth optional field there
/// would push `Report` and `ClientOp` from 192 to 224 bytes on the hot path.
///
/// `heartbeat` is the one frame that carries a session's state to the server.
/// Two shapes travel under the same kind, and both are optional-skipped so
/// neither writes a field the other has no use for:
/// - the adapter's beat — a plugin telling its host it is alive — holds
///   `observed` alone. The host reduces that tuple into the session row it
///   keeps, and `session_id`/`projection` stay absent.
/// - the client's publish to the server holds `projection`, the whole
///   [`SessionProjection`] it stores for the task, beside the same `observed`
///   tuple so the two never disagree. The server mirrors that projection
///   verbatim; `session_id` names the row it belongs to.
///
/// A beat holding no `projection` is liveness only: the server keeps the
/// working/running/pending/attached tuple it has always inferred from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum Report {
    /// The ready barrier passed; the server may now deliver the payload.
    Ready {
        task_id: String,
        session_id: String,
        generation: u64,
        seq: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cluster_ref: Option<String>,
    },
    /// Liveness plus, on the client-to-server path, the whole projection.
    Heartbeat {
        task_id: String,
        /// The session the published projection belongs to. Absent on the
        /// adapter's beat, which names no session.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        session_id: String,
        generation: u64,
        seq: u64,
        observed: Value,
        /// The client's full session projection, published for the server to
        /// mirror. Absent on a liveness-only beat.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        projection: Option<SessionProjection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cluster_ref: Option<String>,
    },
    /// Terminal result for a task; `head` is the summary the ledger keeps.
    Complete {
        task_id: String,
        outcome: Outcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<String>,
        /// The full result, delivered verbatim to the next hop and the originator.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<String>,
        /// Absolute paths of the files the result names.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cluster_ref: Option<String>,
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
            Report::Heartbeat {
                generation, seq, ..
            } => Some((*generation, *seq)),
            Report::Fault {
                generation, seq, ..
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
    /// Hand back control rows only.
    ///
    /// A role at `max_sessions` stops pulling work it has nowhere to run, and a
    /// control command is exactly what its operator wants to send at that moment
    /// (`recycle` to free a slot, `focus` to look at the session that filled it).
    /// Without this filter the capacity gate and the control plane share one
    /// queue, so a saturated role becomes unreachable for its own recovery.
    /// Absent or false keeps the whole vocabulary, which is what a role with free
    /// capacity asks for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_only: Option<bool>,
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
    /// Opt-in freshness: ask the named task's owning client to probe its plugin
    /// and wait up to these many milliseconds for that task's row to move past
    /// the watermark the read started at. Absent is the plain read — the stored
    /// mirror, answered with no control frame and nothing waited on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_wait_ms: Option<u64>,
}

/// How the opt-in fresh read of one session row ended (§10 `sessions`).
///
/// The marker travels with the row it describes, because the row alone cannot
/// say whether it is the mirror or the answer a probe just produced: a mirror
/// is a legal answer either way, and `updated_at` dates it without saying who
/// wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FreshRead {
    /// The owning client probed its plugin and republished: the row carries
    /// the observation that probe produced, and `updated_at` dates it.
    Probed,
    /// Nothing was asked — no task was named, the task has no row, no client
    /// owns it, that client is not connected, or the ask was refused on the way
    /// out — so the row is the stored mirror and `updated_at` says how old it is.
    Offline,
    /// The probe went out and the row did not move inside the read's own bound:
    /// the row is the stored mirror, as of `updated_at`.
    Unanswered,
}

/// One `query_sessions` answer row: the stored projection with its address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SessionRow {
    /// The session this row answers for, which is the mirror table's key.
    pub session_id: String,
    /// The delivery this session is currently serving, read off its open
    /// `session_tasks` binding. A session a client holds but has not bound to a
    /// delivery has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub generation: u64,
    pub seq: u64,
    pub public_lifecycle: Lifecycle,
    pub projection: SessionProjection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// When the projection content last moved. A heartbeat that only refreshes
    /// `last_seen` leaves this alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// When this session was last seen at all, which is what a reader judges
    /// the row's freshness by: the mirror of a pane that died hours ago reads
    /// exactly like a live one, and this is the field that separates them. The
    /// server never decides staleness from it; the caller prints it and judges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
    /// True when a working row the server has seen is silent past heartbeat grace.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub heartbeat_stale: bool,
    /// What the opt-in fresh read of this row did. Absent on a plain read,
    /// which answers the stored mirror and makes no claim about its freshness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh: Option<FreshRead>,
}
/// One `query_ledger` answer row: the observable ledger projection for one send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct LedgerEntry {
    pub msg_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,
    pub kind: MsgKind,
    pub from: Principal,
    pub to: Principal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task: Option<String>,
    /// Hop count from the root task, read off the stored envelope's causality.
    /// `parent_task` names the parent link; this is the depth it sits at, which
    /// is what `onlyne handoff` extends and what `onlyne ledger` shows.
    pub hop: u32,
    /// The family's root task id, read off the same causality. Every handoff of one run
    /// carries it unchanged, so a supervisor reads a run's whole arc off this column
    /// without walking `parent_task` links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// The hops the family may spend, read off the same causality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hop_budget: Option<u32>,
    /// The role the family reports home to, read off the same causality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Wall-clock bound for the whole family, read off the same causality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DateTime<Utc>>,
    /// Free-form metadata the core carries and never interprets, read off the same
    /// causality. One handoff inherits the whole map, which is the point of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::BTreeMap<String, String>>,
    pub attempt: u32,
    pub state: LedgerState,
    /// Why this row last settled: the receiver's refusal on a rejection, the
    /// budget or age that ended the attempts on `requeue_exhausted` /
    /// `requeue_ttl`. An `acked` row carries `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_head: Option<String>,
    /// The stored envelope body, which retention clears after the ack window
    /// (plan §8 line 360). An auditor reads this field to see what the row
    /// carried, so it travels with the row rather than in a side channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_json: Option<String>,
    pub enqueued_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_at: Option<DateTime<Utc>>,
}
/// One `query_ghost_sweeps` answer row: a settlement the server's ghost sweep
/// recorded in its own audit table.
///
/// `seq_before` and `seq_after` are the mirror row's two versions, so the pair
/// names the exact write the sweep made. `evidence` carries the evidence tag
/// plus the ledger state that justified the sweep, `task_settled:acked` for a
/// task whose own ledger row reached `acked`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct GhostSweep {
    /// `ghost_sweeps.id` in the ledger.
    pub id: i64,
    pub task_id: String,
    pub role: String,
    pub session_id: String,
    /// The mirror row's generation, held constant across the sweep.
    pub generation: u64,
    /// The mirror row's version before the sweep, and after it.
    pub seq_before: u64,
    pub seq_after: u64,
    /// The verdict written onto the mirror row, read off the task's ledger row.
    pub outcome: Outcome,
    pub evidence: String,
    /// Unix seconds.
    pub swept_at: i64,
}
/// How a role's client talks to the role's runtime: the spec's
/// `[client.runtime]` table as the wire carries it (`docs/v2-PLAN.md`
/// §"驱动与放置").
///
/// The drive and the argv are properties of the runtime, so they belong to the
/// role and travel with it. Where the runtime process is displayed is the
/// placement, a property of the machine that runs the client, and never
/// travels on this wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RoleRuntime {
    /// `plugin` | `acp` | `exec`. An absent key reads as [`Drive::Plugin`],
    /// which is what every role in the tree is today.
    #[serde(default)]
    pub drive: Drive,
    /// The argv one session runs, with `{session}` and `{task}` substituted by
    /// the client. An absent key reads as an empty list, which no drive can
    /// start a session with.
    #[serde(default)]
    pub command: Vec<String>,
}

impl Default for RoleRuntime {
    fn default() -> Self {
        Self {
            drive: Drive::Plugin,
            command: Vec::new(),
        }
    }
}

/// The way a client talks to a role's runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Drive {
    /// The client starts the runtime and a plugin inside it dials back.
    #[default]
    Plugin,
    /// The client runs the agent as its own child and speaks the Agent Client
    /// Protocol on that child's stdio.
    Acp,
    /// The client runs the command and reads its exit code.
    Exec,
}

/// One `roles` answer row: the registry record plus live presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RoleInfo {
    pub name: String,
    pub admin: bool,
    pub max_sessions: u32,
    /// The drive and the argv one role session runs (`[client.runtime]`).
    #[serde(default)]
    pub runtime: RoleRuntime,
    pub spec_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prose: Option<String>,
    pub state: Presence,
    pub sessions: u32,
    /// The deliveries queued for this role's inbox, counted exactly by the
    /// server. It is the depth a session of this role would have to drain: the
    /// queued ledger rows addressed to the role that `pull` would hand out,
    /// `note` rows excluded because `pull` never offers one (§3 line 152 gives
    /// them no session). A row from a server that predates the field omits the
    /// key, which reads as nothing waiting.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub queued: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The entry's `allowed_targets` verbatim: a `*` stays unexpanded and a
    /// name with no registered role still appears.
    #[serde(default)]
    pub edges: Vec<String>,
    /// The entry's `aggregate` label; a plain role carries no key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<String>,
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
///
/// The task is named inside [`crate::envelope::ControlOp`], and every variant
/// names it the same way: one required `task_id: String` holding a task-family
/// uuid. There is no absent-task control op — the field is never an empty
/// string, and a caller holding no task id has nothing to aim at. An empty
/// `task_id` is a defect, not a wildcard. `Option<String>` task fields beside
/// this one in this module ([`LedgerQuery`], [`HistoryArgs`],
/// [`QueryFaultsArgs`]) are query filters, where absent means "do not filter",
/// and [`Report::Fault`] keeps its `Option` because a fault can genuinely name
/// no task (an exhausted intent never reached a session to be carried by).
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

/// `publish_event` request: one settlement class the client owns, plus its
/// payload, delivered to the server's event stream (`docs/v2-CONTRACT.md` §
/// "Slice 7"). `class` is from [`onlyne_proto::CLIENT_EVENT_CLASSES`]; the
/// server refuses anything else by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PublishEventArgs {
    /// The event class, one of [`crate::CLIENT_EVENT_CLASSES`].
    pub class: String,
    /// The event payload, carried verbatim. A hook reads it off stdin and a
    /// subscriber sees it in the event's `data`.
    #[serde(default)]
    pub payload: serde_json::Value,
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
    /// The sessions the client still holds on this role. A reconnecting client
    /// sends this list at `hello`, and the adoption requeue leaves the
    /// deliveries those sessions are bound to `in_flight` with their tickets
    /// rebound to the new link: the work is alive in a pane the successor
    /// connection inherits, so a re-delivery would hand the same task to a
    /// second session. An empty or absent list requeues every unacknowledged
    /// row, which is what a client from an earlier build sends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub live_sessions: Vec<LiveSession>,
}

/// One session a client claims it still holds, as `hello` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct LiveSession {
    pub session_id: String,
    /// The delivery this session is bound to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// A session the client holds but has released its process for.
    pub suspended: bool,
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
    /// Publish one of the settlement classes to the server's event stream.
    /// Carries no `(generation, seq)`, no report gate, and no session state:
    /// it is a client's word about a turn it witnessed, so a hook bound to
    /// the class can serve the plan's own example (`docs/v2-CONTRACT.md` §
    /// "Slice 7"). The server appends the event and answers nothing.
    PublishEvent(PublishEventArgs),
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
            ClientOp::Subscribe(_) => "subscribe",
            ClientOp::QueryLedger(_) => "query_ledger",
            ClientOp::QuerySessions(_) => "query_sessions",
            ClientOp::QueryRoles(_) => "query_roles",
            ClientOp::QueryFaults(_) => "query_faults",
            ClientOp::Control(_) => "control",
            ClientOp::PublishEvent(_) => "publish_event",
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
    pub prose: String,
    pub spec_hash: String,
    /// The cluster this authenticated role represents, or `None` for a plain
    /// role; the server fills it from the role's own `[[client]]` entry, whose
    /// `aggregate` key (line 248) is the only source, and a plain role's frame
    /// omits the key entirely. The spec stays the only truth about topology
    /// (§5 line 216, decision D13 at line 27), so no process may claim a cluster
    /// identity from a command-line flag. A client stamps its reports with the
    /// value beside `Principal::Cluster` (line 122), which the federation rule
    /// at line 462 keeps to aggregate roles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<String>,
    /// The entry's `allowed_targets` verbatim, and the whole of this role's
    /// policy: the server reads it as the ACL, and a client reads it as the
    /// obligation — a session of this role must have delivered to every name
    /// here before it may report a terminal outcome. A `*` stays unexpanded and
    /// a name with no registered role still appears; an empty list is a role
    /// that owes nothing (`docs/v2-CONTRACT.md` §"Slice 6").
    pub allowed_targets: Vec<String>,
    pub allowed_senders: Vec<String>,
    /// The drive and the argv one session of this role runs, as the spec's
    /// `[client.runtime]` table wrote them. An absent key is a server that
    /// predates the split, and a client reads it as the default runtime: a
    /// plugin drive with no command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RoleRuntime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ready_ms: Option<u64>,
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
/// `Shutdown` goes beyond the §8 line 320 list — the graceful stop an operator
/// asks for over the admin socket — because that line's zero-policy rule bars
/// automatic repair, not an operator-requested stop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum AdminOp {
    /// Cluster summary: roles, gateways, queue depth, event head.
    Status(Value),
    Roles(QueryRolesArgs),
    Sessions(QuerySessionsArgs),
    Ledger(LedgerQuery),
    Faults(QueryFaultsArgs),
    /// Read the ghost sweep's audit rows, newest first. The number is the row
    /// limit, and `0` asks for the store's own default.
    QueryGhostSweeps(usize),
    /// Stream events until the connection closes.
    Watch(Subscribe),
    /// Paged history over `events`.
    History(HistoryArgs),
    /// Diff the in-memory spec against the file on disk.
    SpecDiff(Value),
    /// Re-read and validate `spec.toml`.
    Reload(Value),
    /// Read the structured spec, the file it was read from, and the hash of
    /// that file's bytes.
    SpecGet(Value),
    /// Apply typed edits to `spec.toml` and reload the cluster.
    SpecApply(SpecApply),
    /// Stream events from a cursor until the connection closes. `watch` answers
    /// one page; this op keeps the connection carrying every event after it.
    Subscribe(Subscribe),
    /// Submit an envelope on behalf of `--from <role>`.
    Send(AdminSend),
    /// Issue a control op as `--from <role>`.
    Control(AdminControl),
    /// File a session's report as `--from <role>`.
    Report(AdminReport),
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
            AdminOp::QueryGhostSweeps(_) => "query_ghost_sweeps",
            AdminOp::Watch(_) => "watch",
            AdminOp::History(_) => "history",
            AdminOp::SpecDiff(_) => "spec_diff",
            AdminOp::Reload(_) => "reload",
            AdminOp::SpecGet(_) => "spec_get",
            AdminOp::SpecApply(_) => "spec_apply",
            AdminOp::Subscribe(_) => "subscribe",
            AdminOp::Send(_) => "send",
            AdminOp::Control(_) => "control",
            AdminOp::Report(_) => "report",
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
                | AdminOp::QueryGhostSweeps(_)
                | AdminOp::Watch(_)
                | AdminOp::History(_)
                | AdminOp::SpecDiff(_)
                | AdminOp::SpecGet(_)
                | AdminOp::Subscribe(_)
                | AdminOp::RepairInspect(_)
        )
    }
}

/// The `spec_get` answer: the spec `spec.toml` parses to, the file it was read
/// from, and the hash of that file's **bytes**.
///
/// The hash is the concurrency token a later [`SpecApply`] is checked against,
/// not a semantic fingerprint: a comment-only edit moves it, which is the
/// point (`docs/v2-CONTRACT.md` §"Slice 4"). The spec travels as a JSON object
/// rather than as a typed field because `onlyne-proto` owns the wire vocabulary
/// and `onlyne-config` owns the document's shape, the same boundary
/// [`RoleRuntime`] keeps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SpecView {
    /// The absolute path of the file these bytes came from.
    pub path: String,
    /// Lowercase SHA-256 over the file's bytes as they were read.
    pub source_hash: String,
    /// The parsed document, in `onlyne-config::Spec`'s own field names.
    pub spec: Value,
}

/// `spec_apply` request: typed edits over `spec.toml`, checked against the
/// `source_hash` the caller read.
///
/// The server applies them to the document as text, validates the result with
/// the parser that reads the file, writes it atomically, and reloads. A
/// `base_hash` that does not match the file answers `conflict` and writes
/// nothing; a result that does not parse answers `invalid` with
/// `spec.toml:<line>` and writes nothing too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SpecApply {
    /// The `source_hash` [`SpecView`] answered. An empty string is refused
    /// rather than matched: a caller that read nothing cannot be checked.
    pub base_hash: String,
    /// The edits, applied in order. An empty list is refused: a request that
    /// changes nothing must not rewrite the file.
    pub edits: Vec<SpecEdit>,
}

/// One typed edit of `spec.toml`, one shape per field group it writes.
///
/// Every edit names its role; the edits that rewrite a role that does not
/// exist answer `unknown_role` instead of declaring one, so a typo cannot
/// create an entry. `upsert_role` is the one edit that declares a role, and
/// the only one that may.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "edit", content = "args")]
pub enum SpecEdit {
    UpsertRole(UpsertRole),
    RemoveRole(RemoveRole),
    SetTargets(SetTargets),
    SetSenders(SetSenders),
    SetProse(SetProse),
    SetSession(SetSession),
    SetRuntime(SetRuntime),
}

impl SpecEdit {
    /// The edit's wire name, as it appears in the `edit` tag.
    pub fn name(&self) -> &'static str {
        match self {
            SpecEdit::UpsertRole(_) => "upsert_role",
            SpecEdit::RemoveRole(_) => "remove_role",
            SpecEdit::SetTargets(_) => "set_targets",
            SpecEdit::SetSenders(_) => "set_senders",
            SpecEdit::SetProse(_) => "set_prose",
            SpecEdit::SetSession(_) => "set_session",
            SpecEdit::SetRuntime(_) => "set_runtime",
        }
    }

    /// The role this edit writes, whoever's entry it is.
    pub fn role(&self) -> &str {
        match self {
            SpecEdit::UpsertRole(edit) => &edit.role,
            SpecEdit::RemoveRole(edit) => &edit.role,
            SpecEdit::SetTargets(edit) => &edit.role,
            SpecEdit::SetSenders(edit) => &edit.role,
            SpecEdit::SetProse(edit) => &edit.role,
            SpecEdit::SetSession(edit) => &edit.role,
            SpecEdit::SetRuntime(edit) => &edit.role,
        }
    }
}

/// `upsert_role`: declare a role, or rewrite the fields this edit names.
///
/// Only the keys that are present move. An absent `key` keeps the key an
/// existing entry carries; an absent `key` on a role that is not declared yet
/// is refused, because a role entry without one can never authenticate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct UpsertRole {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prose: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin: Option<bool>,
    /// Sessions this role may run at once. It is enforced off the role row,
    /// which the reload rewrites, so it takes effect with the reload and
    /// reaches a client at its next `hello`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_sessions: Option<u32>,
}

/// `remove_role`: drop one `[[client]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct RemoveRole {
    pub role: String,
}

/// `set_targets`: replace one role's `allowed_targets` wholesale. An empty list
/// is a role that reaches no other role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SetTargets {
    pub role: String,
    pub targets: Vec<String>,
}

/// `set_senders`: replace one role's `allowed_senders` wholesale. An empty list
/// is a role no other role reaches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SetSenders {
    pub role: String,
    pub senders: Vec<String>,
}

/// `set_prose`: replace one role's prose, which is what the next session that
/// opens reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SetProse {
    pub role: String,
    pub prose: String,
}

/// `set_session`: replace the policy one role's sessions run under — the
/// `[client.timeout]` and `[client.intent]` halves of its entry. Only the keys
/// that are present move, and the values reach sessions opened after the
/// reload. `max_sessions` is not here: it is enforced off the role row and
/// belongs to [`UpsertRole`], which is the edit whose effect is immediate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SetSession {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff_ms: Option<Vec<u64>>,
}

/// `set_runtime`: replace one role's `[client.runtime]` table, the drive and
/// the argv one of its sessions runs. Every role in the tree starts its
/// sessions from this table, so the values reach sessions opened after the
/// reload while a running session keeps the command it was started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SetRuntime {
    pub role: String,
    pub runtime: RoleRuntime,
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

/// Admin `report`: the operator files a session's report on its behalf. The
/// server settles it through the path a session's own report takes, and the
/// `session_state` event it publishes names `from` as the admin principal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AdminReport {
    pub from: String,
    pub report: Box<Report>,
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
///
/// This enum keeps five inbound verbs. §7 line 308 puts `render_send` in the
/// host-to-plugin direction and §7 line 322 marks `typing` an optional
/// capability, so both travel on the adapter socket, whose `AdapterMsg` is
/// untagged over `PluginOp` and `HostOp`: this enum is the gateway-to-server
/// half, and the host-to-plugin half is `HostOp::RenderSend` with `typing` as
/// `PluginOp::Typing`. Adding either verb here would give one socket two
/// spellings of the same capability.
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
    use crate::envelope::{Body, Causality, MsgKind, new_task_id};
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
                    live_sessions: Vec::new(),
                }),
                "hello",
            ),
            (ClientOp::Send(Box::new(task())), "send"),
            (
                ClientOp::Pull(PullArgs {
                    role: None,
                    limit: 32,
                    hold_ms: Some(250),
                    control_only: None,
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
                    cluster_ref: Some("cluster-b".into()),
                }),
                "report",
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
            (
                ClientOp::QueryLedger(LedgerQuery::default()),
                "query_ledger",
            ),
            (
                ClientOp::QuerySessions(QuerySessionsArgs::default()),
                "query_sessions",
            ),
            (
                ClientOp::QueryRoles(QueryRolesArgs::default()),
                "query_roles",
            ),
            (
                ClientOp::QueryFaults(QueryFaultsArgs::default()),
                "query_faults",
            ),
            (
                ClientOp::Control(ControlArgs {
                    to: Some("builder".into()),
                    op: crate::envelope::ControlOp::Probe {
                        task_id: new_task_id(),
                    },
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
            (
                ClientOp::PublishEvent(crate::PublishEventArgs {
                    class: "delivery_blocked".into(),
                    payload: serde_json::json!({"task_id": "t1", "role": "planner"}),
                }),
                "publish_event",
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

    // Rejection-path marker only: this pre-v1 `loopback` op name must stay outside the closed vocabulary (plan §8 line 324).
    #[test]
    fn unknown_op_name_is_a_decode_failure() {
        let err =
            serde_json::from_value::<ClientOp>(serde_json::json!({"op":"loopback","args":{}}))
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
        assert!(
            !ClientOp::Report(Report::Ready {
                task_id: new_task_id(),
                session_id: "s".into(),
                generation: 1,
                seq: 1,
                cluster_ref: None
            })
            .is_readonly()
        );
    }

    #[test]
    fn report_versions_expose_the_monotonic_pair() {
        let ready = Report::Ready {
            task_id: new_task_id(),
            session_id: "s".into(),
            generation: 2,
            seq: 9,
            cluster_ref: None,
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
    fn the_two_heartbeat_shapes_write_only_their_own_keys() {
        let beat = Report::Heartbeat {
            task_id: new_task_id(),
            session_id: String::new(),
            generation: 1,
            seq: 14,
            observed: serde_json::json!({"state": "alive"}),
            projection: None,
            cluster_ref: None,
        };
        let value = serde_json::to_value(&beat).expect("encode");
        assert_eq!(value["kind"], "heartbeat");
        assert_eq!(beat.version(), Some((1, 14)));
        // The adapter's beat: no session, no projection, so neither key moves
        // the bytes the plugin socket is pinned to.
        assert!(value["data"].get("session_id").is_none(), "{value}");
        assert!(value["data"].get("projection").is_none(), "{value}");
        let back: Report = serde_json::from_value(value).expect("decode");
        assert_eq!(back, beat);

        let publish = Report::Heartbeat {
            task_id: new_task_id(),
            session_id: "sess-1".into(),
            generation: 2,
            seq: 3,
            observed: serde_json::json!({"state": "closed"}),
            projection: Some(SessionProjection {
                observed: Some(serde_json::json!({"state": "closed"})),
                ..SessionProjection::default_working()
            }),
            cluster_ref: None,
        };
        let value = serde_json::to_value(&publish).expect("encode");
        assert_eq!(value["data"]["session_id"], "sess-1");
        assert_eq!(value["data"]["projection"]["lifecycle"], "created");
        let back: Report = serde_json::from_value(value).expect("decode");
        assert_eq!(back, publish);
    }

    #[test]
    fn session_projection_defaults_to_created_and_detached() {
        let value = serde_json::to_value(SessionProjection::default_working()).expect("encode");
        assert_eq!(value["lifecycle"], "created");
        assert_eq!(value["agent"], "booting");
        assert_eq!(value["delivery"], "none");
        assert_eq!(value["resource"], "detached");
        assert_eq!(value["recovery"], "none");
        assert!(value.get("outcome").is_none());
    }

    #[test]
    fn session_projection_accepts_public_lifecycle_alias() {
        let value = serde_json::json!({
            "public_lifecycle": "exited",
            "agent": "gone",
            "delivery": "accepted",
            "resource": "closed",
            "recovery": "none",
            "outcome": "done"
        });
        let projection: SessionProjection = serde_json::from_value(value).expect("decode alias");
        assert_eq!(projection.lifecycle, Lifecycle::Exited);
        assert_eq!(projection.outcome, Some(Outcome::Done));
    }

    /// The `[client.runtime]` table travels with the role and reads back as
    /// written. A frame that omits the key lands on the default runtime: a
    /// plugin drive with no command, which is what a server predating the split
    /// sends and what every role in the tree was.
    #[test]
    fn a_role_runtime_decodes_and_an_absent_one_is_a_plugin_drive() {
        let written: RoleRuntime = serde_json::from_value(
            serde_json::json!({"drive": "acp", "command": ["python3", "agent.py"]}),
        )
        .expect("the runtime table decodes");
        assert_eq!(written.drive, Drive::Acp);
        assert_eq!(written.command, ["python3", "agent.py"]);

        let absent: RoleRuntime = serde_json::from_value(serde_json::json!({}))
            .expect("an empty table is the default runtime");
        assert_eq!(absent, RoleRuntime::default());
        assert_eq!(absent.drive, Drive::Plugin);
        assert!(absent.command.is_empty());
    }

    /// The relay keys are off the wire, and `allowed_targets` is the list the
    /// client's check reads. A frame a server wrote while the keys still existed
    /// still lands — serde drops a key no field declares — and an empty list
    /// decodes as a role that owes nothing.
    #[test]
    fn a_welcome_carries_allowed_targets_and_no_relay_keys() {
        let mut frame = serde_json::json!({
            "cluster": "cluster-a",
            "server": "srv",
            "role": "planner",
            "admin": false,
            "max_sessions": 3,
            "prose": "Read the incoming task",
            "spec_hash": "abc123",
            "aggregate": null,
            "allowed_targets": ["builder"],
            "allowed_senders": ["*"],
            "runtime": {"drive": "plugin", "command": ["pi", "--session-id", "{session}"]},
            "timeout_ready_ms": 30_000,
            "timeout_idle_ms": 60_000,
            "intent_attempts": 3,
            "intent_backoff_ms": [1000, 2000, 4000],
            "seq": 41,
        });
        let welcome: Welcome =
            serde_json::from_value(frame.clone()).expect("the welcome frame lands");
        assert_eq!(welcome.allowed_targets, vec!["builder".to_string()]);
        let encoded = serde_json::to_value(&welcome).expect("encode the welcome");
        assert_eq!(encoded["allowed_targets"], serde_json::json!(["builder"]));
        assert!(
            encoded.get("relay_required").is_none() && encoded.get("relay_count").is_none(),
            "the removed keys have no reader left on the frame: {encoded}"
        );

        // A server that wrote the frame while the keys existed still lands: the
        // list that decides the policy is the one field both ends read.
        frame["relay_required"] = serde_json::json!(["writer"]);
        frame["relay_count"] = serde_json::json!(2);
        let tolerant: Welcome = serde_json::from_value(frame.clone())
            .expect("a stale relay key is not a decode failure");
        assert_eq!(tolerant.allowed_targets, welcome.allowed_targets);

        frame["allowed_targets"] = serde_json::json!([]);
        let owes_nothing: Welcome = serde_json::from_value(frame).expect("an empty list lands");
        assert!(owes_nothing.allowed_targets.is_empty());
    }

    /// The reason column is additive, so a ledger row a server wrote before the
    /// key existed still lands, with `reason` staying `None` — and re-encoding
    /// such a row omits the key rather than sending null, keeping the answer's
    /// bytes what an older reader parses.
    #[test]
    fn a_ledger_row_without_the_reason_key_decodes_as_no_reason() {
        let raw = serde_json::json!({
            "msg_id": "3f2a1c4e-5b6d-4e7f-8a90-1b2c3d4e5f60",
            "kind": "task",
            "from": {"role": {"role": "planner"}},
            "to": {"role": {"role": "builder"}},
            "hop": 0,
            "attempt": 1,
            "state": "acked",
            "out_head": "hello v1",
            "enqueued_at": "2026-09-10T12:00:00Z",
            "acked_at": "2026-09-10T12:00:00Z",
        });
        let entry: LedgerEntry = serde_json::from_value(raw).expect("the pre-reason row lands");
        assert_eq!(entry.reason, None);
        let encoded = serde_json::to_value(&entry).expect("encode the row");
        assert!(
            encoded.get("reason").is_none(),
            "an absent reason omits the key rather than sending null: {encoded}"
        );
    }

    #[test]
    fn admin_vocabulary_is_closed_and_named() {
        let ops: Vec<(AdminOp, &str)> = vec![
            (AdminOp::Status(Value::Null), "status"),
            (AdminOp::Roles(QueryRolesArgs::default()), "roles"),
            (AdminOp::Sessions(QuerySessionsArgs::default()), "sessions"),
            (AdminOp::Ledger(LedgerQuery::default()), "ledger"),
            (AdminOp::Faults(QueryFaultsArgs::default()), "faults"),
            (AdminOp::QueryGhostSweeps(50), "query_ghost_sweeps"),
            (AdminOp::Watch(Subscribe::default()), "watch"),
            (AdminOp::History(HistoryArgs::default()), "history"),
            (AdminOp::SpecDiff(Value::Null), "spec_diff"),
            (AdminOp::Reload(Value::Null), "reload"),
            (AdminOp::SpecGet(Value::Null), "spec_get"),
            (
                AdminOp::SpecApply(SpecApply {
                    base_hash: "e5".into(),
                    edits: vec![SpecEdit::SetProse(SetProse {
                        role: "planner".into(),
                        prose: "plan".into(),
                    })],
                }),
                "spec_apply",
            ),
            (AdminOp::Subscribe(Subscribe::default()), "subscribe"),
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
                    op: crate::envelope::ControlOp::Snapshot {
                        task_id: new_task_id(),
                    },
                    to: None,
                }),
                "control",
            ),
            (
                AdminOp::Report(AdminReport {
                    from: "planner".into(),
                    report: Box::new(Report::Complete {
                        task_id: new_task_id(),
                        outcome: Outcome::Done,
                        head: Some("done".into()),
                        details: None,
                        files: Vec::new(),
                        reply_to: None,
                        cluster_ref: None,
                    }),
                }),
                "report",
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
        assert!(AdminOp::SpecGet(Value::Null).is_readonly());
        assert!(AdminOp::Subscribe(Subscribe::default()).is_readonly());
        assert!(!AdminOp::SpecApply(SpecApply::default()).is_readonly());
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
        };
        let value = serde_json::to_value(&receipt).expect("encode");
        assert_eq!(value["state"], "in_flight");
        assert_eq!(value["kind"], "task");
    }
}
