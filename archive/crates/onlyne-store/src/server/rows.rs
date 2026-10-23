//! Row types, row mappers, and the timestamp/text conversion helpers the
//! server store writes its columns with.

use chrono::{DateTime, SecondsFormat, Utc};
use onlyne_proto::{
    Envelope, Event, FaultEvent, LedgerState, LedgerStateEvent, MsgKind, Outcome, Principal,
};
use rusqlite::types::Type;
use rusqlite::{OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;

use crate::error::{StoreError, StoreResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleRow {
    pub name: String,
    pub key: String,
    pub admin: bool,
    pub max_sessions: i64,
    pub spec_hash: String,
    pub updated_at: String,
}

/// One mirror row as a writer hands it over, and as a reader gets it back.
///
/// The address is `session_id`, which is the table's key: a session serves one
/// delivery at a time, and the deliveries it serves live in `session_tasks`
/// rather than in this row. `task_id` is the delivery the write is about — the
/// binding a writer opens — and on a read it is the delivery the session is
/// serving now, derived from its open `session_tasks` row and absent when the
/// session is on no delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWrite {
    pub session_id: String,
    pub task_id: Option<String>,
    pub role: String,
    pub generation: i64,
    pub seq: i64,
    pub agent_state: String,
    pub delivery_state: String,
    pub resource_state: String,
    pub recovery_substate: String,
    pub desired_json: String,
    pub observed_json: String,
    pub mismatch_count: i64,
    /// When the mirror last saw this session, in unix seconds. Stored as the
    /// RFC 3339 text the crate's helpers produce.
    pub last_seen: i64,
    /// When the projection content last moved, in unix seconds.
    pub updated_at: i64,
}

pub type ServerSessionRow = SessionWrite;

/// One `session_tasks` row: which delivery a session serves, and whether it
/// still serves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBindingRow {
    pub session_id: String,
    pub task_id: String,
    /// When this session took the delivery, in unix seconds.
    pub bound_at: i64,
    /// When it stopped serving it, in unix seconds. `None` while the delivery
    /// is the one the session is on.
    pub released_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerRow {
    pub msg_id: String,
    pub op_id: Option<String>,
    pub fingerprint: Option<String>,
    pub kind: MsgKind,
    pub from_json: String,
    pub to_json: String,
    pub task: Option<String>,
    pub parent_task: Option<String>,
    pub attempt: i64,
    pub state: LedgerState,
    pub out_head: Option<String>,
    pub reason: Option<String>,
    pub enqueued_at: String,
    pub acked_at: Option<String>,
    pub body_json: Option<String>,
    /// Causality's hop count from the root task, which is what turns the
    /// `parent_task` links into a measurable depth.
    pub hop: i64,
    /// The persisted expiry deadline of a ttl note, so a restarted server can
    /// re-arm its sweep.
    pub expires_at: Option<String>,
    /// Times this row has moved from in_flight back to queued.
    #[serde(default)]
    pub requeued: i64,
    /// The family's root task id, read off the envelope's causality. Every row
    /// of one run carries it unchanged, and `onlyne handoff` reads it back to
    /// mint the next hop into the same family.
    #[serde(default)]
    pub family: Option<String>,
    /// The hops the family may spend, carried on every row of the run.
    #[serde(default)]
    pub hop_budget: Option<i64>,
    /// The role the family reports home to, carried on every row of the run.
    #[serde(default)]
    pub origin: Option<String>,
    /// Wall-clock bound for the whole family, in the RFC 3339 text this
    /// crate's helpers produce.
    #[serde(default)]
    pub deadline: Option<String>,
    /// The causality's labels as JSON text, so the column keeps whatever map
    /// the sender attached.
    #[serde(default)]
    pub labels_json: Option<String>,
}

impl LedgerRow {
    pub fn from_envelope(envelope: &Envelope, fingerprint: &str) -> StoreResult<Self> {
        let body_json = serde_json::to_string(&envelope.body)?;
        let causality = envelope.causality.as_ref();
        let expires_at = match (envelope.kind, envelope.ttl_ms) {
            (MsgKind::Note, Some(ttl)) => Some(rfc3339(
                envelope.ts + chrono::Duration::milliseconds(ttl as i64),
            )),
            _ => None,
        };
        Ok(Self {
            msg_id: envelope.id.clone(),
            op_id: envelope.op_id.clone(),
            fingerprint: Some(fingerprint.to_string()),
            kind: envelope.kind,
            from_json: sender_column(&envelope.from, envelope.admin)?,
            to_json: serde_json::to_string(&envelope.to)?,
            task: causality.map(|c| c.task.clone()),
            parent_task: causality.and_then(|c| c.parent_task.clone()),
            attempt: causality.map(|c| i64::from(c.attempt)).unwrap_or(0),
            hop: causality.map(|c| i64::from(c.hop)).unwrap_or(0),
            state: LedgerState::Queued,
            out_head: Some(body_head(envelope, &body_json)),
            reason: None,
            enqueued_at: rfc3339(envelope.ts),
            acked_at: None,
            body_json: Some(body_json),
            expires_at,
            requeued: 0,
            family: causality.and_then(|c| c.family.clone()),
            hop_budget: causality.and_then(|c| c.hop_budget).map(i64::from),
            origin: causality.and_then(|c| c.origin.clone()),
            deadline: causality.and_then(|c| c.deadline).map(rfc3339),
            labels_json: match causality.and_then(|c| c.labels.as_ref()) {
                Some(labels) => Some(serde_json::to_string(labels)?),
                None => None,
            },
        })
    }
}

impl LedgerRow {
    /// The sender principal this row recorded.
    ///
    /// The sender column carries the principal beside the admin marker, so a
    /// row states whether an admin-surface send wrote it (plan §8 line 320)
    /// without a second column. A column holding a bare principal decodes too.
    pub fn sender(&self) -> Result<Principal, serde_json::Error> {
        let value: Value = serde_json::from_str(&self.from_json)?;
        serde_json::from_value(value.get("principal").cloned().unwrap_or(value))
    }

    /// Whether the send behind this row arrived on the admin surface.
    pub fn sender_is_admin(&self) -> bool {
        serde_json::from_str::<Value>(&self.from_json)
            .ok()
            .and_then(|value| value.get("admin").and_then(Value::as_bool))
            .unwrap_or(false)
    }
}

/// Encode the sender column: the principal plus the admin marker.
pub(crate) fn sender_column(from: &Principal, admin: bool) -> StoreResult<String> {
    Ok(serde_json::to_string(&serde_json::json!({
        "admin": admin,
        "principal": from,
    }))?)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Append {
    Accepted(LedgerRow),
    Duplicate {
        existing: LedgerRow,
        fingerprint_matches: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: i64,
    pub kind: String,
    pub data: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerFaultRow {
    pub id: i64,
    pub task_id: Option<String>,
    pub role: Option<String>,
    pub session_id: Option<String>,
    pub generation: Option<i64>,
    pub seq: Option<i64>,
    pub desired_json: Option<String>,
    pub observed_json: Option<String>,
    pub intent: Option<String>,
    pub attempt: Option<i64>,
    pub backend_ref: Option<String>,
    pub kind: String,
    pub reason: String,
    pub state: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultQuery {
    pub task_id: Option<String>,
    pub role: Option<String>,
    pub kind: Option<String>,
    pub open_only: bool,
    pub limit: u32,
}

/// One settlement the server's ghost sweep recorded in its own audit table.
///
/// `seq_before` and `seq_after` are the mirror row's two versions. The sweep
/// writes through the same settlement path a `repair_*` verb uses, and that path
/// bumps `seq` by one, so the pair names the exact write an operator is reading
/// about. `swept_at` holds the kernel's unix seconds and the column holds the
/// RFC 3339 text this crate's conversion helpers produce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhostSweepRow {
    pub id: i64,
    pub task_id: String,
    pub role: String,
    pub session_id: String,
    pub generation: i64,
    pub seq_before: i64,
    pub seq_after: i64,
    /// The verdict written onto the mirror row, read off the task's ledger row.
    pub outcome: Outcome,
    /// What justified the sweep: the evidence tag plus the ledger state it read.
    pub evidence: String,
    pub swept_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorRow {
    pub role: String,
    pub last_msg_id: Option<String>,
    pub last_seq: i64,
    pub updated_at: String,
}

pub(crate) fn ledger_row(r: &Row<'_>) -> rusqlite::Result<LedgerRow> {
    let kind: String = r.get(3)?;
    let state: String = r.get(9)?;
    Ok(LedgerRow {
        msg_id: r.get(0)?,
        op_id: r.get(1)?,
        fingerprint: r.get(2)?,
        kind: parse_msg_kind(&kind)
            .ok_or_else(|| conversion_error(3, format!("invalid message kind {kind}")))?,
        from_json: r.get(4)?,
        to_json: r.get(5)?,
        task: r.get(6)?,
        parent_task: r.get(7)?,
        attempt: r.get(8)?,
        state: parse_ledger_state(&state)
            .ok_or_else(|| conversion_error(9, format!("invalid ledger state {state}")))?,
        out_head: r.get(10)?,
        reason: r.get(11)?,
        enqueued_at: r.get(12)?,
        acked_at: r.get(13)?,
        body_json: r.get(14)?,
        hop: r.get(15)?,
        expires_at: r.get(16)?,
        requeued: r.get(17)?,
        family: r.get(18)?,
        hop_budget: r.get(19)?,
        origin: r.get(20)?,
        deadline: r.get(21)?,
        labels_json: r.get(22)?,
    })
}

pub(crate) fn role_row(r: &Row<'_>) -> rusqlite::Result<RoleRow> {
    let admin: i64 = r.get(2)?;
    Ok(RoleRow {
        name: r.get(0)?,
        key: r.get(1)?,
        admin: admin != 0,
        max_sessions: r.get(3)?,
        spec_hash: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

/// One [`super::session_columns`] row. `task_id` is the derived binding, so it
/// is the second column rather than one of the table's own.
pub(crate) fn session_row(r: &Row<'_>) -> rusqlite::Result<ServerSessionRow> {
    Ok(ServerSessionRow {
        session_id: r.get(0)?,
        task_id: r.get(1)?,
        role: r.get(2)?,
        generation: r.get(3)?,
        seq: r.get(4)?,
        agent_state: r.get(5)?,
        delivery_state: r.get(6)?,
        resource_state: r.get(7)?,
        recovery_substate: r.get(8)?,
        desired_json: r.get(9)?,
        observed_json: r.get(10)?,
        mismatch_count: r.get(11)?,
        last_seen: rfc3339_to_unix(&r.get::<_, String>(12)?),
        updated_at: rfc3339_to_unix(&r.get::<_, String>(13)?),
    })
}

pub(crate) fn session_binding_row(r: &Row<'_>) -> rusqlite::Result<SessionBindingRow> {
    Ok(SessionBindingRow {
        session_id: r.get(0)?,
        task_id: r.get(1)?,
        bound_at: rfc3339_to_unix(&r.get::<_, String>(2)?),
        released_at: r
            .get::<_, Option<String>>(3)?
            .map(|text| rfc3339_to_unix(&text)),
    })
}

pub(crate) fn event_row(r: &Row<'_>) -> rusqlite::Result<EventRecord> {
    let text: String = r.get(2)?;
    let data = serde_json::from_str(&text).map_err(|e| conversion_error(2, e.to_string()))?;
    Ok(EventRecord {
        seq: r.get(0)?,
        kind: r.get(1)?,
        data,
        created_at: r.get(3)?,
    })
}

pub(crate) fn server_fault_row(r: &Row<'_>) -> rusqlite::Result<ServerFaultRow> {
    Ok(ServerFaultRow {
        id: r.get(0)?,
        task_id: r.get(1)?,
        role: r.get(2)?,
        session_id: r.get(3)?,
        generation: r.get(4)?,
        seq: r.get(5)?,
        desired_json: r.get(6)?,
        observed_json: r.get(7)?,
        intent: r.get(8)?,
        attempt: r.get(9)?,
        backend_ref: r.get(10)?,
        kind: r.get(11)?,
        reason: r.get(12)?,
        state: r.get(13)?,
        created_at: rfc3339_to_unix(&r.get::<_, String>(14)?),
    })
}

pub(crate) fn ghost_sweep_row(r: &Row<'_>) -> rusqlite::Result<GhostSweepRow> {
    let outcome: String = r.get(7)?;
    Ok(GhostSweepRow {
        id: r.get(0)?,
        task_id: r.get(1)?,
        role: r.get(2)?,
        session_id: r.get(3)?,
        generation: r.get(4)?,
        seq_before: r.get(5)?,
        seq_after: r.get(6)?,
        outcome: parse_outcome(&outcome)
            .ok_or_else(|| conversion_error(7, format!("invalid outcome {outcome}")))?,
        evidence: r.get(8)?,
        swept_at: rfc3339_to_unix(&r.get::<_, String>(9)?),
    })
}

pub(crate) fn cursor_row(r: &Row<'_>) -> rusqlite::Result<CursorRow> {
    Ok(CursorRow {
        role: r.get(0)?,
        last_msg_id: r.get(1)?,
        last_seq: r.get(2)?,
        updated_at: r.get(3)?,
    })
}

pub(crate) fn bool_int(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

/// Grapheme clusters kept by [`head_preview`].
pub const OUT_HEAD_CLUSTERS: usize = 200;

/// The one-line preview of a message body: the first [`OUT_HEAD_CLUSTERS`]
/// grapheme clusters, cut on a cluster boundary, so a combining sequence, a ZWJ
/// emoji, and a flag survive whole. `docs/v1-PLAN.md` line 358 keeps the first
/// 200 字 of the body in `ledger.out_head` for the operator-facing ledger view,
/// and the unit is grapheme clusters: a code-point cut lands inside a cluster
/// and the preview renders as a broken glyph.
pub fn head_preview(text: &str) -> String {
    text.graphemes(true).take(OUT_HEAD_CLUSTERS).collect()
}

fn body_head(envelope: &Envelope, body_json: &str) -> String {
    // An explicit `head` on the body wins: it is the one display line the
    // sender named, and a body that carries its full result in `text` would
    // otherwise show the first clusters of the result rather than the line the
    // caller wrote. A body with no `head` of its own — the CLI's own
    // completion, a plain delivery — falls back to its text, which is the only
    // content it has.
    if let Some(head) = envelope.body.head.as_deref().filter(|h| !h.is_empty()) {
        return head_preview(head);
    }
    let text = envelope.body.text.as_deref().unwrap_or(body_json);
    head_preview(text)
}

/// Encode a kernel timestamp, unix seconds, as the RFC 3339 text every other
/// column in this schema uses. A value outside the representable range encodes
/// as the epoch.
pub(crate) fn unix_to_rfc3339(seconds: i64) -> String {
    DateTime::from_timestamp(seconds, 0)
        .map(rfc3339)
        .unwrap_or_else(|| rfc3339(DateTime::UNIX_EPOCH))
}

/// Decode an RFC 3339 column back to the kernel's unix seconds.
pub(crate) fn rfc3339_to_unix(text: &str) -> i64 {
    DateTime::parse_from_rfc3339(text)
        .map(|value| value.timestamp())
        .unwrap_or(0)
}

/// One moment as RFC 3339 text.
///
/// Milliseconds rather than whole seconds: three acks inside one second are a
/// normal run, and plan §Verification case 4 reads the stamps of a reconnect
/// burst as the order they were settled in, which whole seconds collapse.
pub fn rfc3339(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn parse_msg_kind(value: &str) -> Option<MsgKind> {
    match value {
        "task" => Some(MsgKind::Task),
        "completion" => Some(MsgKind::Completion),
        "note" => Some(MsgKind::Note),
        "control" => Some(MsgKind::Control),
        _ => None,
    }
}

pub(crate) fn parse_ledger_state(value: &str) -> Option<LedgerState> {
    match value {
        "queued" => Some(LedgerState::Queued),
        "in_flight" => Some(LedgerState::InFlight),
        "acked" => Some(LedgerState::Acked),
        "rejected" => Some(LedgerState::Rejected),
        "expired" => Some(LedgerState::Expired),
        _ => None,
    }
}

pub(crate) fn parse_outcome(value: &str) -> Option<Outcome> {
    match value {
        "done" => Some(Outcome::Done),
        "failed" => Some(Outcome::Failed),
        "cancelled" => Some(Outcome::Cancelled),
        "blocked" => Some(Outcome::Blocked),
        _ => None,
    }
}

pub(crate) fn string_tag<T: Serialize>(value: &T) -> StoreResult<String> {
    match serde_json::to_value(value)? {
        Value::String(text) => Ok(text),
        other => Err(StoreError::Serialization(format!(
            "state serialized as {other}"
        ))),
    }
}

pub(crate) fn conversion_error(index: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        Type::Text,
        Box::new(StoreError::Serialization(message)),
    )
}

pub(crate) fn current_ledger_state(
    conn: &rusqlite::Connection,
    msg_id: &str,
) -> StoreResult<LedgerState> {
    let state = conn
        .query_row(
            "SELECT state FROM ledger WHERE msg_id=?",
            rusqlite::params![msg_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound)?;
    parse_ledger_state(&state).ok_or(StoreError::Serialization(format!(
        "invalid ledger state {state}"
    )))
}

pub(crate) fn ensure_transition_allowed(from: LedgerState, to: LedgerState) -> StoreResult<()> {
    if crate::transition_allowed(from, to) {
        return Ok(());
    }
    Err(StoreError::InvalidState {
        from: from.as_str().to_string(),
        to: to.as_str().to_string(),
    })
}

pub(crate) fn insert_ledger_row(conn: &rusqlite::Connection, row: &LedgerRow) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO ledger(msg_id,op_id,fingerprint,kind,from_json,to_json,task,parent_task,attempt,hop,state,out_head,reason,enqueued_at,acked_at,body_json,expires_at,requeued,family,hop_budget,origin,deadline,labels_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            row.msg_id,
            row.op_id,
            row.fingerprint,
            row.kind.as_str(),
            row.from_json,
            row.to_json,
            row.task,
            row.parent_task,
            row.attempt,
            row.hop,
            row.state.as_str(),
            row.out_head,
            row.reason,
            row.enqueued_at,
            row.acked_at,
            row.body_json,
            row.expires_at,
            row.requeued,
            row.family,
            row.hop_budget,
            row.origin,
            row.deadline,
            row.labels_json
        ],
    )?;
    Ok(())
}

pub(crate) fn ledger_by_op_id(
    conn: &rusqlite::Connection,
    op_id: &str,
) -> StoreResult<Option<LedgerRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {} FROM ledger WHERE op_id=?", super::LEDGER_COLUMNS),
            rusqlite::params![op_id],
            ledger_row,
        )
        .optional()?)
}

pub(crate) fn ledger_by_msg_id(
    conn: &rusqlite::Connection,
    msg_id: &str,
) -> StoreResult<LedgerRow> {
    conn.query_row(
        &format!(
            "SELECT {} FROM ledger WHERE msg_id=?",
            super::LEDGER_COLUMNS
        ),
        rusqlite::params![msg_id],
        ledger_row,
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

pub(crate) fn fault_row_by_id(conn: &rusqlite::Connection, id: i64) -> StoreResult<ServerFaultRow> {
    conn.query_row(
        &format!("SELECT {} FROM faults WHERE id=?", super::FAULT_COLUMNS),
        rusqlite::params![id],
        server_fault_row,
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

/// The exact `Event::LedgerState` payload `State::emit` stores, so a store-side
/// transition and a server-side one produce the same event row.
pub(crate) fn ledger_state_event(
    row: &LedgerRow,
    state: LedgerState,
    reason: Option<String>,
) -> StoreResult<Value> {
    let event = Event::LedgerState(LedgerStateEvent {
        msg_id: row.msg_id.clone(),
        op_id: row.op_id.clone(),
        kind: row.kind,
        from: row.sender().unwrap_or_else(|_| Principal::role("unknown")),
        to: serde_json::from_str(&row.to_json).unwrap_or_else(|_| Principal::role("unknown")),
        task: row.task.clone(),
        state,
        outcome: None,
        reason,
    });
    Ok(serde_json::to_value(&event)?)
}

/// The exact `Event::Fault` payload for one stored fault row.
pub(crate) fn fault_event(row: &ServerFaultRow) -> StoreResult<Value> {
    let event = Event::Fault(FaultEvent {
        id: row.id,
        task_id: row.task_id.clone(),
        role: row.role.clone(),
        session_id: row.session_id.clone(),
        generation: row.generation.map(|value| value.max(0) as u64),
        seq: row.seq.map(|value| value.max(0) as u64),
        kind: row.kind.clone(),
        reason: row.reason.clone(),
        desired: row
            .desired_json
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok()),
        observed: row
            .observed_json
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok()),
        intent: row.intent.clone(),
        attempt: row.attempt.map(|value| value.max(0) as u64),
        backend_ref: row
            .backend_ref
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok()),
        state: Some(row.state.clone()),
        created_at: Some(row.created_at),
    });
    Ok(serde_json::to_value(&event)?)
}
