use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use onlyne_proto::{
    Envelope, Event, FaultEvent, LedgerQuery, LedgerState, LedgerStateEvent, MsgKind, Principal,
    QueryFaultsArgs, QuerySessionsArgs,
};
use rusqlite::types::{Type, Value as SqlValue};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;

use crate::error::{StoreError, StoreResult};
use crate::transition_allowed;

const SCHEMA_VERSION: i64 = 1;
const PROTOCOL_VERSION: i64 = 1;
const SERVER_MARKER: &str = "onlyne-server";
const DEFAULT_LIMIT: i64 = 100;

pub const SERVER_DDL: &str = r#"CREATE TABLE IF NOT EXISTS roles(
  name TEXT PRIMARY KEY,
  key TEXT NOT NULL,
  admin INTEGER NOT NULL,
  max_sessions INTEGER NOT NULL,
  spec_hash TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions(
  task_id TEXT PRIMARY KEY,
  role TEXT NOT NULL,
  session_id TEXT NOT NULL,
  generation INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  public_lifecycle TEXT NOT NULL,
  agent_state TEXT NOT NULL,
  delivery_state TEXT NOT NULL,
  resource_state TEXT NOT NULL,
  recovery_substate TEXT NOT NULL,
  -- docs/v1-PLAN.md:356 requires the (generation, seq) gate and the kernel's isolate-after-N and terminate-after-N policy needs a persisted counter, so this pair carries DEFAULT_ISOLATE_AFTER and DEFAULT_TERMINATE_AFTER from crates/onlyne-session/src/reconcile.rs; the fence at line 354 omits both columns.
  desired_json TEXT NOT NULL,
  observed_json TEXT NOT NULL,
  mismatch_count INTEGER NOT NULL,
  -- docs/v1-PLAN.md:354 types this column TEXT while SessionRecord.updated_at in crates/onlyne-session/src/reconcile.rs is i64 unix seconds, and the store encodes those seconds through its own helper on every write.
  updated_at TEXT NOT NULL
);
-- Secondary index for session-addressed reads; task_id is the primary key per docs/v1-PLAN.md:354, and a re-keyed session can appear on two task rows.
CREATE INDEX IF NOT EXISTS sessions_session_id_idx ON sessions(session_id);
CREATE TABLE IF NOT EXISTS ledger(
  msg_id TEXT PRIMARY KEY,
  op_id TEXT UNIQUE,
  fingerprint TEXT,
  kind TEXT NOT NULL,
  from_json TEXT NOT NULL,
  to_json TEXT NOT NULL,
  task TEXT,
  parent_task TEXT,
  attempt INTEGER NOT NULL,
  state TEXT NOT NULL,
  out_head TEXT,
  reason TEXT,
  enqueued_at TEXT NOT NULL,
  acked_at TEXT,
  -- docs/v1-PLAN.md:358 declares body_json TEXT NOT NULL while line 360 retains the body until acked plus retention_days and then sets NULL, so the document argues with itself and the code keeps the retention rule.
  body_json TEXT
);
CREATE INDEX IF NOT EXISTS ledger_state_enqueued_idx ON ledger(state,enqueued_at);
CREATE INDEX IF NOT EXISTS ledger_task_idx ON ledger(task);
CREATE INDEX IF NOT EXISTS ledger_kind_state_idx ON ledger(kind,state);
CREATE TABLE IF NOT EXISTS events(
  seq INTEGER PRIMARY KEY,
  type TEXT NOT NULL,
  data_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_type_idx ON events(type);
CREATE TABLE IF NOT EXISTS faults(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT,
  role TEXT,
  session_id TEXT,
  generation INTEGER,
  seq INTEGER,
  desired_json TEXT,
  observed_json TEXT,
  intent TEXT,
  attempt INTEGER,
  backend_ref TEXT,
  kind TEXT NOT NULL,
  reason TEXT NOT NULL,
  state TEXT NOT NULL,
  -- docs/v1-PLAN.md:364 types this column TEXT while FaultRecord.created_at in crates/onlyne-session/src/reconcile.rs is i64 unix seconds, and the store encodes those seconds through its own helper on every write.
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS faults_task_kind_generation_idx ON faults(task_id,kind,generation);
CREATE INDEX IF NOT EXISTS faults_state_idx ON faults(state);
CREATE TABLE IF NOT EXISTS inbox_cursors(
  role TEXT PRIMARY KEY,
  last_msg_id TEXT,
  last_seq INTEGER NOT NULL,
  updated_at TEXT NOT NULL
);"#;

const SCHEMA_MARKER_DDL: &str = "CREATE TABLE IF NOT EXISTS schema_marker(name TEXT PRIMARY KEY, version INTEGER NOT NULL, protocol_version INTEGER NOT NULL);";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleRow {
    pub name: String,
    pub key: String,
    pub admin: bool,
    pub max_sessions: i64,
    pub spec_hash: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWrite {
    pub task_id: String,
    pub role: String,
    pub session_id: String,
    pub generation: i64,
    pub seq: i64,
    pub public_lifecycle: String,
    pub agent_state: String,
    pub delivery_state: String,
    pub resource_state: String,
    pub recovery_substate: String,
    pub desired_json: String,
    pub observed_json: String,
    pub mismatch_count: i64,
    pub updated_at: i64,
}

pub type ServerSessionRow = SessionWrite;

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
}

impl LedgerRow {
    pub fn from_envelope(envelope: &Envelope, fingerprint: &str) -> StoreResult<Self> {
        let body_json = serde_json::to_string(&envelope.body)?;
        let causality = envelope.causality.as_ref();
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
            state: LedgerState::Queued,
            out_head: Some(body_head(envelope, &body_json)),
            reason: None,
            enqueued_at: rfc3339(envelope.ts),
            acked_at: None,
            body_json: Some(body_json),
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
fn sender_column(from: &Principal, admin: bool) -> StoreResult<String> {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorRow {
    pub role: String,
    pub last_msg_id: Option<String>,
    pub last_seq: i64,
    pub updated_at: String,
}

#[derive(Clone, Debug)]
pub struct ServerLedger {
    path: PathBuf,
    retention_days: i64,
    inner: Arc<Mutex<Connection>>,
}

impl ServerLedger {
    pub fn open(path: impl AsRef<Path>, retention_days: u32) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = open_connection(&path, SERVER_MARKER, SERVER_DDL)?;
        Ok(Self {
            path,
            retention_days: i64::from(retention_days).max(1),
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn retention_days(&self) -> i64 {
        self.retention_days
    }

    pub fn upsert_role(&self, role: &RoleRow) -> StoreResult<bool> {
        let conn = self.conn()?;
        let changed = conn.execute(
            "INSERT INTO roles(name,key,admin,max_sessions,spec_hash,updated_at) VALUES(?,?,?,?,?,?)
             ON CONFLICT(name) DO UPDATE SET key=excluded.key,admin=excluded.admin,max_sessions=excluded.max_sessions,spec_hash=excluded.spec_hash,updated_at=excluded.updated_at",
            params![
                role.name,
                role.key,
                bool_int(role.admin),
                role.max_sessions,
                role.spec_hash,
                unix_to_rfc3339(rfc3339_to_unix(&role.updated_at))
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn list_roles(&self) -> StoreResult<Vec<RoleRow>> {
        let conn = self.conn()?;
        let rows = conn
            .prepare(
                "SELECT name,key,admin,max_sessions,spec_hash,updated_at FROM roles ORDER BY name",
            )?
            .query_map([], role_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn remove_role_missing_from(&self, names: &[String]) -> StoreResult<usize> {
        let conn = self.conn()?;
        if names.is_empty() {
            return Ok(conn.execute("DELETE FROM roles", [])?);
        }
        let placeholders = repeat_placeholders(names.len());
        let sql = format!("DELETE FROM roles WHERE name NOT IN ({placeholders})");
        let args = names
            .iter()
            .cloned()
            .map(SqlValue::Text)
            .collect::<Vec<_>>();
        Ok(conn.execute(&sql, params_from_iter(args))?)
    }

    pub fn project_session(&self, write: &SessionWrite) -> StoreResult<bool> {
        let conn = self.conn()?;
        let changed = conn.execute(
            "INSERT INTO sessions(task_id,role,session_id,generation,seq,public_lifecycle,agent_state,delivery_state,resource_state,recovery_substate,desired_json,observed_json,mismatch_count,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(task_id) DO UPDATE SET role=excluded.role,session_id=excluded.session_id,generation=excluded.generation,seq=excluded.seq,public_lifecycle=excluded.public_lifecycle,agent_state=excluded.agent_state,delivery_state=excluded.delivery_state,resource_state=excluded.resource_state,recovery_substate=excluded.recovery_substate,desired_json=excluded.desired_json,observed_json=excluded.observed_json,mismatch_count=excluded.mismatch_count,updated_at=excluded.updated_at
             WHERE excluded.generation > sessions.generation OR (excluded.generation = sessions.generation AND excluded.seq > sessions.seq)",
            params![
                write.task_id,
                write.role,
                write.session_id,
                write.generation,
                write.seq,
                write.public_lifecycle,
                write.agent_state,
                write.delivery_state,
                write.resource_state,
                write.recovery_substate,
                write.desired_json,
                write.observed_json,
                write.mismatch_count,
                unix_to_rfc3339(write.updated_at)
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn get_session_row(&self, task_id: &str) -> StoreResult<Option<ServerSessionRow>> {
        let conn = self.conn()?;
        let row = conn
            .query_row(
                "SELECT task_id,role,session_id,generation,seq,public_lifecycle,agent_state,delivery_state,resource_state,recovery_substate,desired_json,observed_json,mismatch_count,updated_at FROM sessions WHERE task_id=?",
                params![task_id],
                session_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_sessions(&self, filter: QuerySessionsArgs) -> StoreResult<Vec<ServerSessionRow>> {
        let conn = self.conn()?;
        let mut clauses = Vec::new();
        let mut args = Vec::new();
        if let Some(task_id) = filter.task_id {
            clauses.push("task_id=?".to_string());
            args.push(SqlValue::Text(task_id));
        }
        if let Some(role) = filter.role {
            clauses.push("role=?".to_string());
            args.push(SqlValue::Text(role));
        }
        if let Some(lifecycle) = filter.lifecycle {
            clauses.push("public_lifecycle=?".to_string());
            args.push(SqlValue::Text(string_tag(&lifecycle)?));
        }
        let where_sql = where_sql(&clauses);
        let limit = sql_limit(filter.limit);
        args.push(SqlValue::Integer(limit));
        let sql = format!(
            "SELECT task_id,role,session_id,generation,seq,public_lifecycle,agent_state,delivery_state,resource_state,recovery_substate,desired_json,observed_json,mismatch_count,updated_at FROM sessions{where_sql} ORDER BY updated_at DESC,rowid DESC LIMIT ?"
        );
        let rows = conn
            .prepare(&sql)?
            .query_map(params_from_iter(args), session_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn append_ledger(&self, row: &LedgerRow) -> StoreResult<Append> {
        let conn = self.conn()?;
        if let Some(op_id) = row.op_id.as_deref() {
            if let Some(existing) = ledger_by_op_id(&conn, op_id)? {
                return Ok(Append::Duplicate {
                    fingerprint_matches: existing.fingerprint == row.fingerprint,
                    existing,
                });
            }
        }
        insert_ledger_row(&conn, row)?;
        Ok(Append::Accepted(row.clone()))
    }

    pub fn mark_in_flight(&self, msg_id: &str) -> StoreResult<bool> {
        self.transition_msg(msg_id, LedgerState::InFlight, None, None)
    }

    pub fn mark_acked(&self, msg_id: &str, at: DateTime<Utc>) -> StoreResult<bool> {
        self.transition_msg(msg_id, LedgerState::Acked, Some(rfc3339(at)), None)
    }

    pub fn mark_rejected(&self, msg_id: &str, reason: &str) -> StoreResult<bool> {
        self.transition_msg(
            msg_id,
            LedgerState::Rejected,
            None,
            Some(reason.to_string()),
        )
    }

    pub fn expire_queued_before(&self, now: DateTime<Utc>) -> StoreResult<usize> {
        ensure_transition_allowed(LedgerState::Queued, LedgerState::Expired)?;
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE ledger SET state='expired',reason='expired' WHERE state='queued' AND enqueued_at < ?",
            params![rfc3339(now)],
        )?;
        Ok(changed)
    }

    pub fn queued_for(&self, role: &str, limit: u32) -> StoreResult<Vec<LedgerRow>> {
        self.ledger_for_role(role, LedgerState::Queued, limit)
    }

    pub fn in_flight_for(&self, role: &str) -> StoreResult<Vec<LedgerRow>> {
        self.ledger_for_role(role, LedgerState::InFlight, 500)
    }

    pub fn requeue_in_flight(&self, role: &str) -> StoreResult<usize> {
        ensure_transition_allowed(LedgerState::InFlight, LedgerState::Queued)?;
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE ledger SET state='queued' WHERE state='in_flight' AND json_extract(to_json,'$.role.role')=?",
            params![role],
        )?;
        Ok(changed)
    }

    /// Move one in-flight row back to `queued` and publish its `ledger_state`
    /// event in the same transaction. A row already `queued` is returned
    /// unchanged, so a disconnect path that fires twice settles once.
    pub fn requeue_one(&self, msg_id: &str) -> StoreResult<LedgerRow> {
        let conn = self.conn()?;
        let current = ledger_by_msg_id(&conn, msg_id)?;
        if current.state == LedgerState::Queued {
            return Ok(current);
        }
        ensure_transition_allowed(current.state, LedgerState::Queued)?;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE ledger SET state='queued' WHERE msg_id=?",
            params![msg_id],
        )?;
        let updated = ledger_by_msg_id(&tx, msg_id)?;
        let event = ledger_state_event(&updated, LedgerState::Queued, None)?;
        append_event_conn(&tx, "ledger_state", &event)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Move one queued row to `expired` and publish its `ledger_state` event in
    /// the same transaction.
    /// Settle one row whose deadline passed.
    ///
    /// A row reaches this from `queued` while it waits for its role, and from
    /// `in_flight` when the role held it past the deadline: the sender asked for
    /// a deadline, so the sweep answers with `expired` in both states.
    pub fn expire_one(&self, msg_id: &str, reason: &str) -> StoreResult<LedgerRow> {
        let conn = self.conn()?;
        let current = ledger_by_msg_id(&conn, msg_id)?;
        if !matches!(current.state, LedgerState::Queued | LedgerState::InFlight) {
            ensure_transition_allowed(current.state, LedgerState::Expired)?;
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE ledger SET state='expired',reason=? WHERE msg_id=?",
            params![reason, msg_id],
        )?;
        let updated = ledger_by_msg_id(&tx, msg_id)?;
        let event = ledger_state_event(&updated, LedgerState::Expired, Some(reason.to_string()))?;
        append_event_conn(&tx, "ledger_state", &event)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Move every `open` fault of a task to `next_state`, publishing one `fault`
    /// event per moved row in the same transaction. Returns the moved rows.
    pub fn update_fault_state(
        &self,
        task_id: &str,
        next_state: &str,
        reason: &str,
    ) -> StoreResult<Vec<ServerFaultRow>> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let ids = {
            let mut stmt =
                tx.prepare("SELECT id FROM faults WHERE task_id=? AND state='open' ORDER BY id")?;
            stmt.query_map(params![task_id], |r| r.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut moved = Vec::with_capacity(ids.len());
        for id in ids {
            tx.execute(
                "UPDATE faults SET state=?,reason=? WHERE id=?",
                params![next_state, reason, id],
            )?;
            let row = fault_row_by_id(&tx, id)?;
            let event = fault_event(&row)?;
            append_event_conn(&tx, "fault", &event)?;
            moved.push(row);
        }
        tx.commit()?;
        Ok(moved)
    }

    pub fn ledger_query(&self, query: LedgerQuery) -> StoreResult<Vec<LedgerRow>> {
        let conn = self.conn()?;
        let mut clauses = Vec::new();
        let mut args = Vec::new();
        if let Some(task) = query.task {
            clauses.push("task=?".to_string());
            args.push(SqlValue::Text(task));
        }
        if let Some(op_id) = query.op_id {
            clauses.push("op_id=?".to_string());
            args.push(SqlValue::Text(op_id));
        }
        if let Some(msg_id) = query.msg_id {
            clauses.push("msg_id=?".to_string());
            args.push(SqlValue::Text(msg_id));
        }
        if let Some(role) = query.role {
            clauses.push("(json_extract(from_json,'$.principal.role.role')=? OR json_extract(to_json,'$.role.role')=?)".to_string());
            args.push(SqlValue::Text(role.clone()));
            args.push(SqlValue::Text(role));
        }
        if let Some(state) = query.state {
            clauses.push("state=?".to_string());
            args.push(SqlValue::Text(state.as_str().to_string()));
        }
        if let Some(kind) = query.kind {
            clauses.push("kind=?".to_string());
            args.push(SqlValue::Text(kind.as_str().to_string()));
        }
        args.push(SqlValue::Integer(sql_limit(query.limit)));
        let sql = format!(
            "SELECT {LEDGER_COLUMNS} FROM ledger{} ORDER BY enqueued_at DESC,rowid DESC LIMIT ?",
            where_sql(&clauses)
        );
        let rows = conn
            .prepare(&sql)?
            .query_map(params_from_iter(args), ledger_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The plan's task-keyed ledger read: every row of one task across roles, in
    /// insertion order. `docs/v1-PLAN.md` line 501 reads three rows back in order
    /// after a reconnect and line 508 reads one task's rows after a relocation.
    /// The ledger carries no monotonic column of its own, so the order is
    /// `enqueued_at` with SQLite's `rowid` breaking ties between rows written in
    /// one second: `rowid` is the insertion counter, and it makes the order the
    /// ledger's write order without a second copy of that fact.
    pub fn ledger_task(&self, task: &str, limit: u32) -> StoreResult<Vec<LedgerRow>> {
        let conn = self.conn()?;
        let rows = conn
            .prepare(&format!(
                "SELECT {LEDGER_COLUMNS} FROM ledger WHERE task=? ORDER BY enqueued_at,rowid LIMIT ?"
            ))?
            .query_map(params![task, sql_limit(limit)], ledger_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn append_event(&self, kind: &str, data: &Value) -> StoreResult<i64> {
        let conn = self.conn()?;
        append_event_conn(&conn, kind, data)
    }

    pub fn events_since(&self, seq: i64, limit: u32) -> StoreResult<Vec<EventRecord>> {
        let conn = self.conn()?;
        events_since_conn(&conn, seq, limit)
    }

    pub fn event_head(&self) -> StoreResult<i64> {
        let conn = self.conn()?;
        event_head_conn(&conn)
    }

    pub fn prune(&self, older_than: DateTime<Utc>) -> StoreResult<usize> {
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE ledger SET body_json=NULL WHERE state='acked' AND acked_at IS NOT NULL AND acked_at < ?",
            params![rfc3339(older_than)],
        )?;
        Ok(changed)
    }

    pub fn record_fault(&self, fault: &ServerFaultRow) -> StoreResult<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO faults(task_id,role,session_id,generation,seq,desired_json,observed_json,intent,attempt,backend_ref,kind,reason,state,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                fault.task_id,
                fault.role,
                fault.session_id,
                fault.generation,
                fault.seq,
                fault.desired_json,
                fault.observed_json,
                fault.intent,
                fault.attempt,
                fault.backend_ref,
                fault.kind,
                fault.reason,
                fault.state,
                unix_to_rfc3339(fault.created_at)
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn open_faults(&self) -> StoreResult<Vec<ServerFaultRow>> {
        self.faults_query(FaultQuery {
            open_only: true,
            limit: 500,
            ..FaultQuery::default()
        })
    }

    pub fn ack_fault(&self, fault_id: i64) -> StoreResult<bool> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "UPDATE faults SET state='acked' WHERE id=? AND state<>'acked'",
            params![fault_id],
        )? == 1)
    }

    pub fn faults_query(&self, query: FaultQuery) -> StoreResult<Vec<ServerFaultRow>> {
        let conn = self.conn()?;
        let mut clauses = Vec::new();
        let mut args = Vec::new();
        if let Some(task_id) = query.task_id {
            clauses.push("task_id=?".to_string());
            args.push(SqlValue::Text(task_id));
        }
        if let Some(role) = query.role {
            clauses.push("role=?".to_string());
            args.push(SqlValue::Text(role));
        }
        if let Some(kind) = query.kind {
            clauses.push("kind=?".to_string());
            args.push(SqlValue::Text(kind));
        }
        if query.open_only {
            clauses.push("state='open'".to_string());
        }
        args.push(SqlValue::Integer(sql_limit(query.limit)));
        let sql = format!(
            "SELECT {FAULT_COLUMNS} FROM faults{} ORDER BY id LIMIT ?",
            where_sql(&clauses)
        );
        let rows = conn
            .prepare(&sql)?
            .query_map(params_from_iter(args), server_fault_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn faults_query_proto(&self, query: QueryFaultsArgs) -> StoreResult<Vec<ServerFaultRow>> {
        self.faults_query(FaultQuery {
            task_id: query.task_id,
            role: query.role,
            kind: query.kind,
            open_only: query.open_only,
            limit: query.limit,
        })
    }

    pub fn cursor_for(&self, role: &str) -> StoreResult<Option<CursorRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT role,last_msg_id,last_seq,updated_at FROM inbox_cursors WHERE role=?",
                params![role],
                cursor_row,
            )
            .optional()?)
    }

    pub fn set_cursor(&self, role: &str, msg_id: Option<&str>, seq: i64) -> StoreResult<bool> {
        let conn = self.conn()?;
        let updated_at = rfc3339(Utc::now());
        Ok(conn.execute(
            "INSERT INTO inbox_cursors(role,last_msg_id,last_seq,updated_at) VALUES(?,?,?,?)
             ON CONFLICT(role) DO UPDATE SET last_msg_id=excluded.last_msg_id,last_seq=excluded.last_seq,updated_at=excluded.updated_at",
            params![role, msg_id, seq, updated_at],
        )? == 1)
    }

    fn transition_msg(
        &self,
        msg_id: &str,
        to: LedgerState,
        acked_at: Option<String>,
        reason: Option<String>,
    ) -> StoreResult<bool> {
        let conn = self.conn()?;
        let from = current_ledger_state(&conn, msg_id)?;
        ensure_transition_allowed(from, to)?;
        let changed = conn.execute(
            "UPDATE ledger SET state=?,acked_at=COALESCE(?,acked_at),reason=COALESCE(?,reason) WHERE msg_id=?",
            params![to.as_str(), acked_at, reason, msg_id],
        )?;
        Ok(changed == 1)
    }

    fn ledger_for_role(
        &self,
        role: &str,
        state: LedgerState,
        limit: u32,
    ) -> StoreResult<Vec<LedgerRow>> {
        let conn = self.conn()?;
        let rows = conn
            .prepare(&format!(
                "SELECT {LEDGER_COLUMNS} FROM ledger WHERE state=? AND json_extract(to_json,'$.role.role')=? ORDER BY enqueued_at,rowid LIMIT ?"
            ))?
            .query_map(params![state.as_str(), role, sql_limit(limit)], ledger_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn conn(&self) -> StoreResult<MutexGuard<'_, Connection>> {
        self.inner
            .lock()
            .map_err(|_| StoreError::Sqlite("database mutex poisoned".to_string()))
    }
}

pub(crate) fn open_connection(path: &Path, marker: &str, ddl: &str) -> StoreResult<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::Sqlite(e.to_string()))?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    ensure_schema(&conn, marker, ddl)?;
    Ok(conn)
}

fn ensure_schema(conn: &Connection, marker: &str, ddl: &str) -> StoreResult<()> {
    let tables = user_tables(conn)?;
    if tables.iter().any(|name| is_legacy_table(name)) {
        return Err(StoreError::unsupported_schema());
    }
    conn.execute_batch(SCHEMA_MARKER_DDL)?;
    let marker_row = conn
        .query_row(
            "SELECT version,protocol_version FROM schema_marker WHERE name=?",
            params![marker],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    match marker_row {
        Some((SCHEMA_VERSION, PROTOCOL_VERSION)) => {}
        Some(_) => return Err(StoreError::unsupported_schema()),
        None => {
            let marker_count: i64 =
                conn.query_row("SELECT COUNT(*) FROM schema_marker", [], |r| r.get(0))?;
            if marker_count > 0 || !tables.is_empty() {
                return Err(StoreError::unsupported_schema());
            }
            conn.execute(
                "INSERT INTO schema_marker(name,version,protocol_version) VALUES(?,?,?)",
                params![marker, SCHEMA_VERSION, PROTOCOL_VERSION],
            )?;
        }
    }
    conn.execute_batch(ddl)?;
    Ok(())
}

fn user_tables(conn: &Connection) -> StoreResult<Vec<String>> {
    let rows = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// Rejection-path markers only: these names identify pre-v1 tables and the `swarm` prefix that the schema gate refuses (plan §10 line 370).
fn is_legacy_table(name: &str) -> bool {
    matches!(
        name,
        "io_cursors" | "loopback_idempotency" | "pending_replies"
    ) || name.starts_with("swarm")
}

/// One moment as RFC 3339 text.
///
/// Milliseconds rather than whole seconds: three acks inside one second are a
/// normal run, and plan §Verification case 4 reads the stamps of a reconnect
/// burst as the order they were settled in, which whole seconds collapse.
pub fn rfc3339(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn append_event_conn(conn: &Connection, kind: &str, data: &Value) -> StoreResult<i64> {
    conn.execute(
        "INSERT INTO events(type,data_json,created_at) VALUES(?,?,?)",
        params![kind, serde_json::to_string(data)?, rfc3339(Utc::now())],
    )?;
    Ok(conn.last_insert_rowid())
}

pub(crate) fn events_since_conn(
    conn: &Connection,
    seq: i64,
    limit: u32,
) -> StoreResult<Vec<EventRecord>> {
    let rows = conn
        .prepare(
            "SELECT seq,type,data_json,created_at FROM events WHERE seq>? ORDER BY seq LIMIT ?",
        )?
        .query_map(params![seq, sql_limit(limit)], event_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn event_head_conn(conn: &Connection) -> StoreResult<i64> {
    Ok(conn.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))?)
}

fn current_ledger_state(conn: &Connection, msg_id: &str) -> StoreResult<LedgerState> {
    let state = conn
        .query_row(
            "SELECT state FROM ledger WHERE msg_id=?",
            params![msg_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound)?;
    parse_ledger_state(&state).ok_or(StoreError::Serialization(format!(
        "invalid ledger state {state}"
    )))
}

fn ensure_transition_allowed(from: LedgerState, to: LedgerState) -> StoreResult<()> {
    if transition_allowed(from, to) {
        return Ok(());
    }
    Err(StoreError::InvalidState {
        from: from.as_str().to_string(),
        to: to.as_str().to_string(),
    })
}

fn insert_ledger_row(conn: &Connection, row: &LedgerRow) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO ledger(msg_id,op_id,fingerprint,kind,from_json,to_json,task,parent_task,attempt,state,out_head,reason,enqueued_at,acked_at,body_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            row.msg_id,
            row.op_id,
            row.fingerprint,
            row.kind.as_str(),
            row.from_json,
            row.to_json,
            row.task,
            row.parent_task,
            row.attempt,
            row.state.as_str(),
            row.out_head,
            row.reason,
            row.enqueued_at,
            row.acked_at,
            row.body_json
        ],
    )?;
    Ok(())
}

fn ledger_by_op_id(conn: &Connection, op_id: &str) -> StoreResult<Option<LedgerRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {LEDGER_COLUMNS} FROM ledger WHERE op_id=?"),
            params![op_id],
            ledger_row,
        )
        .optional()?)
}

const LEDGER_COLUMNS: &str = "msg_id,op_id,fingerprint,kind,from_json,to_json,task,parent_task,attempt,state,out_head,reason,enqueued_at,acked_at,body_json";
const FAULT_COLUMNS: &str = "id,task_id,role,session_id,generation,seq,desired_json,observed_json,intent,attempt,backend_ref,kind,reason,state,created_at";

fn ledger_row(r: &Row<'_>) -> rusqlite::Result<LedgerRow> {
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
    })
}

fn role_row(r: &Row<'_>) -> rusqlite::Result<RoleRow> {
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

fn session_row(r: &Row<'_>) -> rusqlite::Result<ServerSessionRow> {
    Ok(ServerSessionRow {
        task_id: r.get(0)?,
        role: r.get(1)?,
        session_id: r.get(2)?,
        generation: r.get(3)?,
        seq: r.get(4)?,
        public_lifecycle: r.get(5)?,
        agent_state: r.get(6)?,
        delivery_state: r.get(7)?,
        resource_state: r.get(8)?,
        recovery_substate: r.get(9)?,
        desired_json: r.get(10)?,
        observed_json: r.get(11)?,
        mismatch_count: r.get(12)?,
        updated_at: rfc3339_to_unix(&r.get::<_, String>(13)?),
    })
}

fn event_row(r: &Row<'_>) -> rusqlite::Result<EventRecord> {
    let text: String = r.get(2)?;
    let data = serde_json::from_str(&text).map_err(|e| conversion_error(2, e.to_string()))?;
    Ok(EventRecord {
        seq: r.get(0)?,
        kind: r.get(1)?,
        data,
        created_at: r.get(3)?,
    })
}

fn server_fault_row(r: &Row<'_>) -> rusqlite::Result<ServerFaultRow> {
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

fn ledger_by_msg_id(conn: &Connection, msg_id: &str) -> StoreResult<LedgerRow> {
    conn.query_row(
        &format!("SELECT {LEDGER_COLUMNS} FROM ledger WHERE msg_id=?"),
        params![msg_id],
        ledger_row,
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

fn fault_row_by_id(conn: &Connection, id: i64) -> StoreResult<ServerFaultRow> {
    conn.query_row(
        &format!("SELECT {FAULT_COLUMNS} FROM faults WHERE id=?"),
        params![id],
        server_fault_row,
    )
    .optional()?
    .ok_or(StoreError::NotFound)
}

/// The exact `Event::LedgerState` payload `State::emit` stores, so a store-side
/// transition and a server-side one produce the same event row.
fn ledger_state_event(
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
fn fault_event(row: &ServerFaultRow) -> StoreResult<Value> {
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

fn cursor_row(r: &Row<'_>) -> rusqlite::Result<CursorRow> {
    Ok(CursorRow {
        role: r.get(0)?,
        last_msg_id: r.get(1)?,
        last_seq: r.get(2)?,
        updated_at: r.get(3)?,
    })
}

fn bool_int(value: bool) -> i64 {
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

fn parse_msg_kind(value: &str) -> Option<MsgKind> {
    match value {
        "task" => Some(MsgKind::Task),
        "completion" => Some(MsgKind::Completion),
        "note" => Some(MsgKind::Note),
        "control" => Some(MsgKind::Control),
        _ => None,
    }
}

fn parse_ledger_state(value: &str) -> Option<LedgerState> {
    match value {
        "queued" => Some(LedgerState::Queued),
        "in_flight" => Some(LedgerState::InFlight),
        "acked" => Some(LedgerState::Acked),
        "rejected" => Some(LedgerState::Rejected),
        "expired" => Some(LedgerState::Expired),
        _ => None,
    }
}

fn string_tag<T: Serialize>(value: &T) -> StoreResult<String> {
    match serde_json::to_value(value)? {
        Value::String(text) => Ok(text),
        other => Err(StoreError::Serialization(format!(
            "state serialized as {other}"
        ))),
    }
}

fn sql_limit(limit: u32) -> i64 {
    if limit == 0 {
        DEFAULT_LIMIT
    } else {
        i64::from(limit.min(500))
    }
}

fn repeat_placeholders(len: usize) -> String {
    std::iter::repeat_n("?", len).collect::<Vec<_>>().join(",")
}

fn where_sql(clauses: &[String]) -> String {
    if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    }
}

fn conversion_error(index: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        Type::Text,
        Box::new(StoreError::Serialization(message)),
    )
}
