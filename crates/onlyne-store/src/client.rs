use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use onlyne_session::{FaultRecord, SessionLedger, SessionRecord, VersionedSession};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{StoreError, StoreResult};
use crate::server::{
    EventRecord, append_event_conn, event_head_conn, events_since_conn, open_connection, rfc3339,
};

const CLIENT_MARKER: &str = "onlyne-client";
/// The client DDL's own revision; the server store carries a separate one.
const CLIENT_SCHEMA_VERSION: i64 = 1;
const DEFAULT_LIMIT: i64 = 100;

pub const CLIENT_DDL: &str = r#"CREATE TABLE IF NOT EXISTS sessions(
  task_id TEXT PRIMARY KEY,
  role TEXT,
  session_id TEXT NOT NULL,
  generation INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  public_lifecycle TEXT NOT NULL,
  agent_state TEXT NOT NULL,
  delivery_state TEXT NOT NULL,
  resource_state TEXT NOT NULL,
  recovery_substate TEXT NOT NULL,
  observed_json TEXT NOT NULL,
  backend TEXT,
  backend_ref TEXT,
  -- docs/v1-PLAN.md:356 requires the (generation, seq) gate and the kernel's isolate-after-N and terminate-after-N policy needs a persisted counter, so this pair carries DEFAULT_ISOLATE_AFTER and DEFAULT_TERMINATE_AFTER from crates/onlyne-session/src/reconcile.rs; the fence at line 368 lists neither column.
  desired_json TEXT NOT NULL,
  mismatch_count INTEGER NOT NULL DEFAULT 0,
  -- docs/v1-PLAN.md:368 defers the session fields to the lifecycle store, whose SessionRecord.updated_at in crates/onlyne-session/src/reconcile.rs is i64 unix seconds, and the store encodes those seconds through its own helper on every write.
  updated_at TEXT NOT NULL
);
-- Secondary index for the kernel's session-addressed delete and close paths; task_id stays the primary key so a task holds one projection row.
CREATE INDEX IF NOT EXISTS sessions_session_id_idx ON sessions(session_id);
CREATE TABLE IF NOT EXISTS intents(
  op_id TEXT PRIMARY KEY,
  env_json TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  state TEXT NOT NULL,
  next_attempt_at TEXT NOT NULL,
  receipt_json TEXT,
  last_error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS intents_state_due_idx ON intents(state,next_attempt_at);
CREATE TABLE IF NOT EXISTS out_head_cache(
  task_id TEXT PRIMARY KEY,
  head TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS prose_cache(
  role TEXT PRIMARY KEY,
  prose TEXT NOT NULL,
  spec_hash TEXT NOT NULL,
  cached_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS config_cache(
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
-- docs/v1-PLAN.md:368 lists no client faults table; the bridge records faults locally so a restart still shows them.
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
  -- docs/v1-PLAN.md:287 makes an exhausted intent observable through the local fault and the fault report; the attempt count is what an operator reads before intervening.
  attempt INTEGER,
  backend_ref TEXT,
  kind TEXT NOT NULL,
  reason TEXT NOT NULL,
  state TEXT NOT NULL,
  -- docs/v1-PLAN.md:364 types the server's faults.created_at TEXT while FaultRecord.created_at in crates/onlyne-session/src/reconcile.rs is i64 unix seconds, and the store encodes those seconds through its own helper on every write.
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS faults_task_kind_generation_idx ON faults(task_id,kind,generation);
CREATE TABLE IF NOT EXISTS events(
  seq INTEGER PRIMARY KEY,
  type TEXT NOT NULL,
  data_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_type_idx ON events(type);"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentRow {
    pub op_id: String,
    pub env_json: Value,
    pub attempt: i64,
    pub state: String,
    pub next_attempt_at: String,
    pub receipt_json: Option<Value>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug)]
pub struct ClientStore {
    path: PathBuf,
    inner: Arc<Mutex<Connection>>,
}

impl ClientStore {
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = open_connection(&path, CLIENT_MARKER, CLIENT_DDL, CLIENT_SCHEMA_VERSION)?;
        Ok(Self {
            path,
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn enqueue_intent(&self, op_id: &str, env_json: &Value) -> StoreResult<bool> {
        let conn = self.conn()?;
        let now = rfc3339(Utc::now());
        let changed = conn.execute(
            "INSERT OR IGNORE INTO intents(op_id,env_json,attempt,state,next_attempt_at,receipt_json,last_error,created_at,updated_at) VALUES(?,?,0,'pending',?,NULL,NULL,?,?)",
            params![op_id, serde_json::to_string(env_json)?, now, now, now],
        )?;
        Ok(changed == 1)
    }

    pub fn due_intents(&self, now: DateTime<Utc>, limit: u32) -> StoreResult<Vec<IntentRow>> {
        let conn = self.conn()?;
        let rows = conn
            .prepare(
                "SELECT op_id,env_json,attempt,state,next_attempt_at,receipt_json,last_error,created_at,updated_at FROM intents WHERE state IN ('pending','retrying') AND next_attempt_at<=? ORDER BY next_attempt_at,created_at,rowid LIMIT ?",
            )?
            .query_map(params![rfc3339(now), sql_limit(limit)], intent_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Store a caller-supplied retry time and move the intent to `retrying`.
    /// The caller owns the attempt ceiling; the client's `IntentMachine` checks `attempts` and calls `exhaust_intent` when the ceiling is reached.
    pub fn bump_intent(
        &self,
        op_id: &str,
        next_attempt_at: DateTime<Utc>,
        error: &str,
    ) -> StoreResult<bool> {
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE intents SET state='retrying',attempt=attempt+1,next_attempt_at=?,last_error=?,updated_at=? WHERE op_id=? AND state IN ('pending','retrying')",
            params![rfc3339(next_attempt_at), error, rfc3339(Utc::now()), op_id],
        )?;
        Ok(changed == 1)
    }

    /// Push an intent's next attempt forward without consuming its budget.
    ///
    /// A transport failure is not the server refusing the message, so the plan's
    /// disconnect rule keeps the queue intact and the reconnect flushes it
    /// (plan §6 line 289). Counting those against `intent.attempts` would drop a
    /// completion that was written while the link was down.
    pub fn defer_intent(
        &self,
        op_id: &str,
        next_attempt_at: DateTime<Utc>,
        error: &str,
    ) -> StoreResult<bool> {
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE intents SET state='retrying',next_attempt_at=?,last_error=?,updated_at=? WHERE op_id=? AND state IN ('pending','retrying')",
            params![rfc3339(next_attempt_at), error, rfc3339(Utc::now()), op_id],
        )?;
        Ok(changed == 1)
    }

    pub fn accept_intent(&self, op_id: &str, receipt_json: &Value) -> StoreResult<bool> {
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE intents SET state='accepted',receipt_json=?,updated_at=? WHERE op_id=? AND state IN ('pending','retrying')",
            params![serde_json::to_string(receipt_json)?, rfc3339(Utc::now()), op_id],
        )?;
        Ok(changed == 1)
    }

    pub fn exhaust_intent(&self, op_id: &str, error: &str) -> StoreResult<bool> {
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE intents SET state='exhausted',last_error=?,updated_at=? WHERE op_id=? AND state IN ('pending','retrying')",
            params![error, rfc3339(Utc::now()), op_id],
        )?;
        Ok(changed == 1)
    }

    pub fn pending_intent_count(&self) -> StoreResult<i64> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM intents WHERE state IN ('pending','retrying')",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn flush_order(&self) -> StoreResult<Vec<IntentRow>> {
        let conn = self.conn()?;
        let rows = conn
            .prepare(
                "SELECT op_id,env_json,attempt,state,next_attempt_at,receipt_json,last_error,created_at,updated_at FROM intents WHERE state IN ('pending','retrying') ORDER BY created_at,rowid",
            )?
            .query_map([], intent_row)?
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

    pub fn put_out_head(&self, task_id: &str, head: &str) -> StoreResult<bool> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "INSERT INTO out_head_cache(task_id,head) VALUES(?,?) ON CONFLICT(task_id) DO UPDATE SET head=excluded.head",
            params![task_id, crate::server::head_preview(head)],
        )? == 1)
    }

    pub fn out_head(&self, task_id: &str) -> StoreResult<Option<String>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT head FROM out_head_cache WHERE task_id=?",
                params![task_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn put_prose(&self, role: &str, prose: &str, spec_hash: &str) -> StoreResult<bool> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "INSERT INTO prose_cache(role,prose,spec_hash,cached_at) VALUES(?,?,?,?) ON CONFLICT(role) DO UPDATE SET prose=excluded.prose,spec_hash=excluded.spec_hash,cached_at=excluded.cached_at",
            params![role, prose, spec_hash, rfc3339(Utc::now())],
        )? == 1)
    }

    pub fn prose(&self, role: &str) -> StoreResult<Option<(String, String)>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT prose,spec_hash FROM prose_cache WHERE role=?",
                params![role],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn put_config(&self, key: &str, value: &str) -> StoreResult<bool> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "INSERT INTO config_cache(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )? == 1)
    }

    pub fn config(&self, key: &str) -> StoreResult<Option<String>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT value FROM config_cache WHERE key=?",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn conn(&self) -> StoreResult<MutexGuard<'_, Connection>> {
        self.inner
            .lock()
            .map_err(|_| StoreError::Sqlite("database mutex poisoned".to_string()))
    }
}

impl SessionLedger for ClientStore {
    fn get_session(&self, task_id: &str) -> anyhow::Result<Option<SessionRecord>> {
        let conn = self.conn()?;
        let row = conn
            .query_row(
                "SELECT task_id,agent_state,delivery_state,resource_state,public_lifecycle,recovery_substate,desired_json,observed_json,generation,seq,backend_ref,mismatch_count,updated_at FROM sessions WHERE task_id=?",
                params![task_id],
                session_record_row,
            )
            .optional()?;
        Ok(row)
    }

    fn upsert_session(&self, task_id: &str, version: &VersionedSession) -> anyhow::Result<bool> {
        let conn = self.conn()?;
        let (backend, session_id) = backend_parts(task_id, &version.backend_ref);
        let changed = conn.execute(
            "INSERT INTO sessions(task_id,role,session_id,generation,seq,public_lifecycle,agent_state,delivery_state,resource_state,recovery_substate,observed_json,backend,backend_ref,desired_json,mismatch_count,updated_at) VALUES(?,NULL,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(task_id) DO UPDATE SET session_id=excluded.session_id,generation=excluded.generation,seq=excluded.seq,public_lifecycle=excluded.public_lifecycle,agent_state=excluded.agent_state,delivery_state=excluded.delivery_state,resource_state=excluded.resource_state,recovery_substate=excluded.recovery_substate,observed_json=excluded.observed_json,backend=COALESCE(excluded.backend,sessions.backend),backend_ref=excluded.backend_ref,desired_json=excluded.desired_json,mismatch_count=excluded.mismatch_count,updated_at=excluded.updated_at
             WHERE excluded.generation > sessions.generation OR (excluded.generation = sessions.generation AND excluded.seq > sessions.seq)",
            params![
                task_id,
                session_id,
                version.generation,
                version.seq,
                version.public_lifecycle,
                version.agent_state,
                version.delivery_state,
                version.resource_state,
                version.recovery_substate,
                version.observed_json,
                backend,
                version.backend_ref,
                version.desired_json,
                version.mismatch_count,
                crate::server::unix_to_rfc3339(version.updated_at)
            ],
        )?;
        Ok(changed == 1)
    }

    fn task_is_known(&self, task_id: &str) -> anyhow::Result<bool> {
        let conn = self.conn()?;
        let session_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE task_id=?",
            params![task_id],
            |r| r.get(0),
        )?;
        if session_count > 0 {
            return Ok(true);
        }
        Ok(intent_stats(&conn, task_id)?.is_some())
    }

    fn task_attempt(&self, task_id: &str) -> anyhow::Result<i64> {
        let conn = self.conn()?;
        Ok(intent_stats(&conn, task_id)?.unwrap_or(0))
    }

    fn list_faults(&self, task_id: &str) -> anyhow::Result<Vec<FaultRecord>> {
        let conn = self.conn()?;
        let rows = conn
            .prepare(
                "SELECT id,task_id,session_id,generation,seq,desired_json,observed_json,intent,attempt,backend_ref,kind,reason,state,created_at FROM faults WHERE task_id=? ORDER BY id",
            )?
            .query_map(params![task_id], fault_record_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn insert_fault(&self, fault: &FaultRecord) -> anyhow::Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO faults(task_id,role,session_id,generation,seq,desired_json,observed_json,intent,attempt,backend_ref,kind,reason,state,created_at) VALUES(?,NULL,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                fault.task_id,
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
                crate::server::unix_to_rfc3339(fault.created_at)
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    fn emit(&self, kind: &str, data: Value) {
        if let Err(err) = self.append_event(kind, &data) {
            tracing::warn!(kind, error = %err, "session ledger event was not stored");
        }
    }

    fn note_alert(&self, line: String) {
        tracing::warn!(alert = %line, "session ledger alert");
    }
}

fn intent_stats(conn: &Connection, task_id: &str) -> StoreResult<Option<i64>> {
    let mut stmt = conn.prepare("SELECT env_json,attempt FROM intents")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut max_attempt: Option<i64> = None;
    for row in rows {
        let (env_json, attempt) = row?;
        if intent_task_matches(&env_json, task_id) {
            max_attempt = Some(max_attempt.map_or(attempt, |current| current.max(attempt)));
        }
    }
    Ok(max_attempt)
}

fn intent_task_matches(env_json: &str, task_id: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(env_json) else {
        return false;
    };
    value
        .get("causality")
        .and_then(|v| v.get("task"))
        .and_then(Value::as_str)
        == Some(task_id)
}

fn backend_parts(task_id: &str, backend_ref: &str) -> (Option<String>, String) {
    let parsed = serde_json::from_str::<Value>(backend_ref).ok();
    let backend = parsed
        .as_ref()
        .and_then(|v| v.get("backend"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let session_id = parsed
        .as_ref()
        .and_then(|v| v.get("task_id"))
        .and_then(Value::as_str)
        .unwrap_or(task_id)
        .to_string();
    (backend, session_id)
}

fn session_record_row(r: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        task_id: r.get(0)?,
        agent_state: r.get(1)?,
        delivery_state: r.get(2)?,
        resource_state: r.get(3)?,
        public_lifecycle: r.get(4)?,
        recovery_substate: r.get(5)?,
        desired_json: r.get(6)?,
        observed_json: r.get(7)?,
        generation: r.get(8)?,
        seq: r.get(9)?,
        backend_ref: r.get(10)?,
        mismatch_count: r.get(11)?,
        updated_at: crate::server::rfc3339_to_unix(&r.get::<_, String>(12)?),
    })
}

fn fault_record_row(r: &Row<'_>) -> rusqlite::Result<FaultRecord> {
    Ok(FaultRecord {
        id: r.get(0)?,
        task_id: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        session_id: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        generation: r.get::<_, Option<i64>>(3)?.unwrap_or_default(),
        seq: r.get::<_, Option<i64>>(4)?.unwrap_or_default(),
        desired_json: r
            .get::<_, Option<String>>(5)?
            .unwrap_or_else(|| "{}".to_string()),
        observed_json: r
            .get::<_, Option<String>>(6)?
            .unwrap_or_else(|| "{}".to_string()),
        intent: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
        attempt: r.get::<_, Option<i64>>(8)?.unwrap_or_default(),
        backend_ref: r
            .get::<_, Option<String>>(9)?
            .unwrap_or_else(|| "{}".to_string()),
        kind: r.get(10)?,
        reason: r.get(11)?,
        state: r.get(12)?,
        created_at: r
            .get::<_, Option<String>>(13)?
            .map(|text| crate::server::rfc3339_to_unix(&text))
            .unwrap_or_default(),
    })
}

fn intent_row(r: &Row<'_>) -> rusqlite::Result<IntentRow> {
    let env_text: String = r.get(1)?;
    let receipt_text: Option<String> = r.get(5)?;
    let env_json =
        serde_json::from_str(&env_text).map_err(|e| conversion_error(1, e.to_string()))?;
    let receipt_json = receipt_text
        .map(|text| serde_json::from_str(&text).map_err(|e| conversion_error(5, e.to_string())))
        .transpose()?;
    Ok(IntentRow {
        op_id: r.get(0)?,
        env_json,
        attempt: r.get(2)?,
        state: r.get(3)?,
        next_attempt_at: r.get(4)?,
        receipt_json,
        last_error: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn sql_limit(limit: u32) -> i64 {
    if limit == 0 {
        DEFAULT_LIMIT
    } else {
        i64::from(limit.min(500))
    }
}

fn conversion_error(index: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        Type::Text,
        Box::new(StoreError::Serialization(message)),
    )
}
