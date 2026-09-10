use anyhow::{anyhow, Context};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA_VERSION: i64 = 2;
const PROTOCOL_VERSION: i64 = 2;

#[derive(Debug, Clone, Serialize)]
pub struct LedgerEvent {
    pub task_id: String,
    pub transfer_send_to: String,
    pub from_ws: String,
    pub to_ws: String,
    pub state: String,
    pub out_head: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
    Closed,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::Running => "running",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
            TaskState::Closed => "closed",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "running" => TaskState::Running,
            "done" => TaskState::Done,
            "failed" => TaskState::Failed,
            "cancelled" => TaskState::Cancelled,
            "closed" => TaskState::Closed,
            _ => TaskState::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRow {
    pub task_id: String,
    pub from_ws: String,
    pub to_ws: String,
    pub transfer_send_to: String,
    pub attempt: u32,
    pub state: TaskState,
    pub terminal: String,
    #[serde(skip_serializing)]
    pub payload: String,
    pub out_head: String,
    pub reason: String,
    pub created_at: i64,
    pub hop_state: String,
    pub ledger_state: String,
    /// `'normal'` for scheduled work, `'recovery'` for a task that exists
    /// because a fault needs an owner. `link_fault_recovery` owns the write, so
    /// every recovery row traces back to exactly one fault row.
    pub kind: String,
    /// Fault id this task recovers, `Some` only when `kind == "recovery"`. The
    /// single-layer recovery gate reads this column.
    pub failure_of: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    pub state: Option<String>,
    pub to_ws: Option<String>,
    pub from_ws: Option<String>,
    pub text: Option<String>,
    pub since: Option<i64>,
    pub retry_only: bool,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskPage {
    pub rows: Vec<TaskRow>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskDetailTask {
    pub task_id: String,
    pub from_ws: String,
    pub to_ws: String,
    pub transfer_send_to: String,
    pub attempt: u32,
    pub state: TaskState,
    pub terminal: String,
    pub payload: String,
    pub out_head: String,
    pub reason: String,
    pub created_at: i64,
    pub hop_state: String,
    pub ledger_state: String,
    pub kind: String,
    pub failure_of: Option<String>,
}

impl From<TaskRow> for TaskDetailTask {
    fn from(row: TaskRow) -> Self {
        Self {
            task_id: row.task_id,
            from_ws: row.from_ws,
            to_ws: row.to_ws,
            transfer_send_to: row.transfer_send_to,
            attempt: row.attempt,
            state: row.state,
            terminal: row.terminal,
            payload: row.payload,
            out_head: row.out_head,
            reason: row.reason,
            created_at: row.created_at,
            hop_state: row.hop_state,
            ledger_state: row.ledger_state,
            kind: row.kind,
            failure_of: row.failure_of,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskDetail {
    pub task: TaskDetailTask,
    pub parent: Option<TaskRow>,
    pub children: Vec<TaskRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphCount {
    pub ws: String,
    pub state: String,
    pub n: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub n: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphData {
    pub by_ws: Vec<GraphCount>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub task_id: String,
    pub agent_state: String,
    pub delivery_state: String,
    pub resource_state: String,
    pub public_lifecycle: String,
    pub recovery_substate: String,
    pub desired_json: String,
    pub observed_json: String,
    pub generation: i64,
    pub seq: i64,
    pub backend_ref: String,
    pub mismatch_count: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntentRecord {
    pub op_id: String,
    pub task_id: String,
    pub kind: String,
    pub payload_json: String,
    pub attempt: i64,
    pub next_attempt_at: i64,
    pub state: String,
    pub receipt_json: String,
    pub last_error: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaultRecord {
    pub id: i64,
    pub task_id: String,
    pub session_id: String,
    pub generation: i64,
    pub seq: i64,
    pub desired_json: String,
    pub observed_json: String,
    pub intent: String,
    pub attempt: i64,
    pub backend_ref: String,
    pub kind: String,
    pub reason: String,
    pub state: String,
    pub recovery_task_id: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionedSession {
    pub agent_state: String,
    pub delivery_state: String,
    pub resource_state: String,
    pub public_lifecycle: String,
    pub recovery_substate: String,
    pub desired_json: String,
    pub observed_json: String,
    pub generation: i64,
    pub seq: i64,
    pub backend_ref: String,
    pub mismatch_count: i64,
    pub updated_at: i64,
}

/// Serialize a payload for its JSON text column.
fn json_text<T: Serialize + ?Sized>(value: &T) -> anyhow::Result<String> {
    Ok(serde_json::to_string(value)?)
}

pub struct Db {
    inner: Mutex<Connection>,
}

impl Db {
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        let path = crate::root::swarm_db(root);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let conn = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
        let old_layout: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='pending_replies'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if old_layout > 0 {
            return Err(anyhow!("unsupported legacy tasks schema: pending_replies"));
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_marker(
               name TEXT PRIMARY KEY, version INTEGER NOT NULL, protocol_version INTEGER NOT NULL
             );",
        )?;
        let marker: Option<(i64, i64)> = tx
            .query_row(
                "SELECT version, protocol_version FROM schema_marker WHERE name='swarm'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((version, protocol)) = marker {
            if version != SCHEMA_VERSION || protocol != PROTOCOL_VERSION {
                return Err(anyhow!(
                    "unsupported swarm schema version {version}, protocol {protocol}"
                ));
            }
        }
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks(
               task_id TEXT PRIMARY KEY, from_ws TEXT NOT NULL, to_ws TEXT NOT NULL,
               transfer_send_to TEXT NOT NULL DEFAULT '', attempt INTEGER NOT NULL DEFAULT 1,
               state TEXT NOT NULL DEFAULT 'pending', terminal TEXT NOT NULL DEFAULT '',
               payload TEXT NOT NULL DEFAULT '', out_head TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL DEFAULT '', ledger_state TEXT NOT NULL DEFAULT '',
               created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
               hop_state TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL DEFAULT 'normal',
               failure_of TEXT, protocol_version INTEGER NOT NULL DEFAULT 2,
               operator_revision TEXT NOT NULL DEFAULT ''
             );
             CREATE TABLE IF NOT EXISTS sessions(
               task_id TEXT PRIMARY KEY, agent_state TEXT NOT NULL, delivery_state TEXT NOT NULL,
               resource_state TEXT NOT NULL, public_lifecycle TEXT NOT NULL, recovery_substate TEXT NOT NULL,
               desired_json TEXT NOT NULL, observed_json TEXT NOT NULL, generation INTEGER NOT NULL,
               seq INTEGER NOT NULL, backend_ref TEXT NOT NULL, mismatch_count INTEGER NOT NULL,
               updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS intents(
               op_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, kind TEXT NOT NULL,
               payload_json TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0,
               next_attempt_at INTEGER NOT NULL, state TEXT NOT NULL, receipt_json TEXT NOT NULL,
               last_error TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS faults(
               id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, session_id TEXT NOT NULL,
               generation INTEGER NOT NULL, seq INTEGER NOT NULL, desired_json TEXT NOT NULL,
               observed_json TEXT NOT NULL, intent TEXT NOT NULL, attempt INTEGER NOT NULL,
               backend_ref TEXT NOT NULL, kind TEXT NOT NULL, reason TEXT NOT NULL, state TEXT NOT NULL,
               recovery_task_id TEXT, created_at INTEGER NOT NULL
             );",
        )?;
        for (name, sql) in [
            (
                "hop_state",
                "ALTER TABLE tasks ADD COLUMN hop_state TEXT NOT NULL DEFAULT ''",
            ),
            (
                "kind",
                "ALTER TABLE tasks ADD COLUMN kind TEXT NOT NULL DEFAULT 'normal'",
            ),
            ("failure_of", "ALTER TABLE tasks ADD COLUMN failure_of TEXT"),
            (
                "protocol_version",
                "ALTER TABLE tasks ADD COLUMN protocol_version INTEGER NOT NULL DEFAULT 2",
            ),
            (
                "operator_revision",
                "ALTER TABLE tasks ADD COLUMN operator_revision TEXT NOT NULL DEFAULT ''",
            ),
        ] {
            let exists: i64 = tx.query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='{name}'"),
                [],
                |r| r.get(0),
            )?;
            if exists == 0 {
                tx.execute_batch(sql)?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO schema_marker(name, version, protocol_version) VALUES('swarm', ?, ?)",
            params![SCHEMA_VERSION, PROTOCOL_VERSION],
        )?;
        tx.commit()?;

        Ok(Self {
            inner: Mutex::new(conn),
        })
    }

    pub fn upsert_session(
        &self,
        task_id: &str,
        version: &VersionedSession,
    ) -> anyhow::Result<bool> {
        let c = self.inner.lock().unwrap();
        let changed = c.execute(
            "INSERT INTO sessions(task_id,agent_state,delivery_state,resource_state,public_lifecycle,recovery_substate,desired_json,observed_json,generation,seq,backend_ref,mismatch_count,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(task_id) DO UPDATE SET agent_state=excluded.agent_state,delivery_state=excluded.delivery_state,resource_state=excluded.resource_state,public_lifecycle=excluded.public_lifecycle,recovery_substate=excluded.recovery_substate,desired_json=excluded.desired_json,observed_json=excluded.observed_json,generation=excluded.generation,seq=excluded.seq,backend_ref=excluded.backend_ref,mismatch_count=excluded.mismatch_count,updated_at=excluded.updated_at
             WHERE excluded.generation > sessions.generation OR (excluded.generation = sessions.generation AND excluded.seq > sessions.seq)",
            params![task_id, version.agent_state, version.delivery_state, version.resource_state, version.public_lifecycle, version.recovery_substate, version.desired_json, version.observed_json, version.generation, version.seq, version.backend_ref, version.mismatch_count, version.updated_at],
        )?;
        Ok(changed == 1)
    }

    pub fn get_session(&self, task_id: &str) -> anyhow::Result<Option<SessionRecord>> {
        let c = self.inner.lock().unwrap();
        let mut st = c.prepare("SELECT task_id,agent_state,delivery_state,resource_state,public_lifecycle,recovery_substate,desired_json,observed_json,generation,seq,backend_ref,mismatch_count,updated_at FROM sessions WHERE task_id=?")?;
        let mut rows = st.query(params![task_id])?;
        rows.next()?
            .map(session_row)
            .transpose()
            .map_err(Into::into)
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionRecord>> {
        let c = self.inner.lock().unwrap();
        let rows = c
            .prepare("SELECT task_id,agent_state,delivery_state,resource_state,public_lifecycle,recovery_substate,desired_json,observed_json,generation,seq,backend_ref,mismatch_count,updated_at FROM sessions ORDER BY task_id")?
            .query_map([], session_row)?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn insert_intent<T: Serialize>(
        &self,
        op_id: &str,
        task_id: &str,
        kind: &str,
        payload: &T,
        next_attempt_at: i64,
        now: i64,
    ) -> anyhow::Result<()> {
        let payload_json = json_text(payload)?;
        let c = self.inner.lock().unwrap();
        c.execute("INSERT INTO intents(op_id,task_id,kind,payload_json,attempt,next_attempt_at,state,receipt_json,last_error,created_at,updated_at) VALUES(?,?,?, ?,0,?,'pending','','',?,?)", params![op_id,task_id,kind,payload_json,next_attempt_at,now,now]).map(|_| ()).map_err(|e| anyhow!("insert intent {op_id}: {e}"))
    }

    pub fn claim_intent(&self, now: i64) -> anyhow::Result<Option<IntentRecord>> {
        let c = self.inner.lock().unwrap();
        let tx = c.unchecked_transaction()?;
        let found: Option<String> = tx.query_row("SELECT op_id FROM intents WHERE state='pending' AND next_attempt_at<=? ORDER BY next_attempt_at,created_at LIMIT 1", params![now], |r| r.get(0)).optional()?;
        let Some(op_id) = found else {
            tx.commit()?;
            return Ok(None);
        };
        tx.execute("UPDATE intents SET state='claimed',attempt=attempt+1,updated_at=? WHERE op_id=? AND state='pending'", params![now,op_id])?;
        let record = tx.query_row("SELECT op_id,task_id,kind,payload_json,attempt,next_attempt_at,state,receipt_json,last_error,created_at,updated_at FROM intents WHERE op_id=?", params![op_id], intent_row)?;
        tx.commit()?;
        Ok(Some(record))
    }

    pub fn receipt_intent<T: Serialize>(
        &self,
        op_id: &str,
        receipt: &T,
        now: i64,
    ) -> anyhow::Result<bool> {
        let c = self.inner.lock().unwrap();
        let receipt_json = json_text(receipt)?;
        Ok(c.execute("UPDATE intents SET state='succeeded',receipt_json=?,updated_at=? WHERE op_id=? AND state<>'succeeded'", params![receipt_json,now,op_id])? == 1)
    }

    pub fn exhaust_intent(&self, op_id: &str, error: &str, now: i64) -> anyhow::Result<bool> {
        let c = self.inner.lock().unwrap();
        Ok(c.execute("UPDATE intents SET state='exhausted',last_error=?,updated_at=? WHERE op_id=? AND state<>'succeeded'", params![error,now,op_id])? == 1)
    }

    pub fn get_intent(&self, op_id: &str) -> anyhow::Result<Option<IntentRecord>> {
        let c = self.inner.lock().unwrap();
        let mut st = c.prepare("SELECT op_id,task_id,kind,payload_json,attempt,next_attempt_at,state,receipt_json,last_error,created_at,updated_at FROM intents WHERE op_id=?")?;
        let mut rows = st.query(params![op_id])?;
        rows.next()?.map(intent_row).transpose().map_err(Into::into)
    }

    pub fn list_intents(&self, state: Option<&str>) -> anyhow::Result<Vec<IntentRecord>> {
        let c = self.inner.lock().unwrap();
        let mut st = c.prepare("SELECT op_id,task_id,kind,payload_json,attempt,next_attempt_at,state,receipt_json,last_error,created_at,updated_at FROM intents WHERE (? IS NULL OR state=?) ORDER BY created_at, op_id")?;
        let rows = st
            .query_map(params![state, state], intent_row)?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Claim one specific op when it is pending and due. `claim_intent` walks the
    /// oldest due row (the retry pump's view); a producer that just persisted an
    /// intent and wants to attempt it immediately needs *this* op claimed, and
    /// the `state='pending'` guard keeps a concurrent claimer from running it
    /// twice.
    pub fn claim_intent_op(&self, op_id: &str, now: i64) -> anyhow::Result<Option<IntentRecord>> {
        let c = self.inner.lock().unwrap();
        let taken = c.execute(
            "UPDATE intents SET state='claimed',attempt=attempt+1,updated_at=? WHERE op_id=? AND state='pending' AND next_attempt_at<=?",
            params![now, op_id, now],
        )?;
        if taken != 1 {
            return Ok(None);
        }
        let record = c.query_row(
            "SELECT op_id,task_id,kind,payload_json,attempt,next_attempt_at,state,receipt_json,last_error,created_at,updated_at FROM intents WHERE op_id=?",
            params![op_id],
            intent_row,
        )?;
        Ok(Some(record))
    }

    /// Return a claimed intent to `pending` with its next backoff slot. Guarded
    /// on `state='claimed'` so a receipt that landed during the attempt cannot
    /// be resurrected for another send.
    pub fn retry_intent(
        &self,
        op_id: &str,
        next_attempt_at: i64,
        error: &str,
        now: i64,
    ) -> anyhow::Result<bool> {
        let c = self.inner.lock().unwrap();
        Ok(c.execute(
            "UPDATE intents SET state='pending',next_attempt_at=?,last_error=?,updated_at=? WHERE op_id=? AND state='claimed'",
            params![next_attempt_at, error, now, op_id],
        )? == 1)
    }

    /// Re-arm a settled intent for another operator round (`repair retry`).
    /// The attempt counter resets because the retry is a fresh budget request,
    /// and the guard on the terminal states keeps a live intent's bookkeeping
    /// untouched.
    pub fn reset_intent(&self, op_id: &str, now: i64) -> anyhow::Result<bool> {
        let c = self.inner.lock().unwrap();
        Ok(c.execute(
            "UPDATE intents SET state='pending',attempt=0,next_attempt_at=?,receipt_json='',last_error='',updated_at=? WHERE op_id=? AND state IN ('succeeded','exhausted')",
            params![now, now, op_id],
        )? == 1)
    }

    /// One fault row by id. `None` means the id never existed, which is a
    /// different answer from "already acked" and stays distinguishable.
    pub fn get_fault(&self, fault_id: i64) -> anyhow::Result<Option<FaultRecord>> {
        let c = self.inner.lock().unwrap();
        let mut st = c.prepare(&format!("SELECT {FAULT_COLUMNS} FROM faults WHERE id=?"))?;
        let mut rows = st.query(params![fault_id])?;
        rows.next()?.map(fault_row).transpose().map_err(Into::into)
    }

    /// Operator acknowledgement: `open`/`recovery_created` -> `acked`. The
    /// `state<>'acked'` guard makes a double ack a reported no-op instead of a
    /// fresh write, so `repair ack` can tell the operator the truth.
    pub fn ack_fault(&self, fault_id: i64) -> anyhow::Result<bool> {
        let c = self.inner.lock().unwrap();
        Ok(c.execute(
            "UPDATE faults SET state='acked' WHERE id=? AND state<>'acked'",
            params![fault_id],
        )? == 1)
    }

    /// The last repair stamp written for a task. `None` covers both "the task is
    /// unknown" and "nobody has repaired it", which are the two answers a
    /// caller can act on; the column's own empty-string default is neither.
    pub fn operator_revision(&self, task_id: &str) -> anyhow::Result<Option<String>> {
        let c = self.inner.lock().unwrap();
        Ok(c.query_row(
            "SELECT operator_revision FROM tasks WHERE task_id=?",
            params![task_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .filter(|stamp| !stamp.is_empty()))
    }

    /// Record one operator repair on the task row. The stamp carries the action
    /// and its moment, so a later `inspect` shows what a human already did.
    pub fn bump_operator_revision(
        &self,
        task_id: &str,
        action: &str,
        now: i64,
    ) -> anyhow::Result<Option<String>> {
        let revision = format!("repair {action} at {now}");
        let c = self.inner.lock().unwrap();
        let changed = c.execute(
            "UPDATE tasks SET operator_revision=? WHERE task_id=?",
            params![revision, task_id],
        )?;
        Ok((changed == 1).then(|| revision.clone()))
    }

    pub fn list_faults(&self, task_id: Option<&str>) -> anyhow::Result<Vec<FaultRecord>> {
        let c = self.inner.lock().unwrap();
        let mut st = c.prepare(&format!(
            "SELECT {FAULT_COLUMNS} FROM faults WHERE (? IS NULL OR task_id=?) ORDER BY id"
        ))?;
        let rows = st
            .query_map(params![task_id, task_id], fault_row)?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn insert_fault(&self, fault: &FaultRecord) -> anyhow::Result<i64> {
        let c = self.inner.lock().unwrap();
        c.execute("INSERT INTO faults(task_id,session_id,generation,seq,desired_json,observed_json,intent,attempt,backend_ref,kind,reason,state,recovery_task_id,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)", params![fault.task_id,fault.session_id,fault.generation,fault.seq,fault.desired_json,fault.observed_json,fault.intent,fault.attempt,fault.backend_ref,fault.kind,fault.reason,fault.state,fault.recovery_task_id,fault.created_at])?;
        Ok(c.last_insert_rowid())
    }

    pub fn link_fault_recovery(&self, fault_id: i64, recovery_task_id: &str) -> anyhow::Result<()> {
        let c = self.inner.lock().unwrap();
        let tx = c.unchecked_transaction()?;
        tx.execute(
            "UPDATE faults SET recovery_task_id=?,state='recovery_created' WHERE id=?",
            params![recovery_task_id, fault_id],
        )?;
        tx.execute(
            "UPDATE tasks SET kind='recovery',failure_of=? WHERE task_id=?",
            params![fault_id.to_string(), recovery_task_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_task(
        &self,
        task_id: &str,
        from_ws: &str,
        to_ws: &str,
        transfer_send_to: &str,
        attempt: u32,
        payload: &str,
    ) -> anyhow::Result<bool> {
        let c = self.inner.lock().unwrap();
        let n = c.execute(
            "INSERT OR IGNORE INTO tasks(task_id, from_ws, to_ws, transfer_send_to, attempt, payload) VALUES(?,?,?,?,?,?)",
            params![task_id, from_ws, to_ws, transfer_send_to, attempt, payload],
        )?;
        Ok(n == 1)
    }

    pub fn set_state(&self, task_id: &str, state: TaskState) -> anyhow::Result<()> {
        self.inner.lock().unwrap().execute(
            "UPDATE tasks SET state=? WHERE task_id=?",
            params![state.as_str(), task_id],
        )?;
        Ok(())
    }

    pub fn set_terminal(&self, task_id: &str, terminal: &str) -> anyhow::Result<()> {
        self.inner.lock().unwrap().execute(
            "UPDATE tasks SET terminal=? WHERE task_id=?",
            params![terminal, task_id],
        )?;
        Ok(())
    }

    /// R4 hop substate write. Empty string clears (terminal rows carry no
    /// substate; queries treat '' as plain Running).
    pub fn set_hop(&self, task_id: &str, hop: &str) -> anyhow::Result<()> {
        self.inner.lock().unwrap().execute(
            "UPDATE tasks SET hop_state=? WHERE task_id=?",
            params![hop, task_id],
        )?;
        Ok(())
    }

    /// Record a terminal ledger row: terminal state + out head + reason.
    pub fn set_ledger(
        &self,
        task_id: &str,
        ledger_state: &str,
        out_head: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        self.inner.lock().unwrap().execute(
            "UPDATE tasks SET ledger_state=?, out_head=?, reason=? WHERE task_id=?",
            params![ledger_state, out_head, reason, task_id],
        )?;
        Ok(())
    }

    pub fn get(&self, task_id: &str) -> anyhow::Result<Option<TaskRow>> {
        let c = self.inner.lock().unwrap();
        get_with_conn(&c, task_id)
    }

    /// Lineage tree rooted at `task_id` (downstream spawns via transfer_send_to).
    pub fn family(&self, task_id: &str) -> anyhow::Result<Vec<TaskRow>> {
        let c = self.inner.lock().unwrap();
        let mut st = c.prepare(
            "WITH RECURSIVE fam(id) AS (
               SELECT ? UNION SELECT task_id FROM tasks, fam WHERE transfer_send_to = fam.id
             ) SELECT task_id, from_ws, to_ws, transfer_send_to, attempt, state, terminal, payload, out_head, reason, created_at, hop_state, ledger_state, kind, failure_of
               FROM tasks WHERE task_id IN fam ORDER BY created_at ASC, rowid ASC",
        )?;
        let mapped = st.query_map(params![task_id], row)?;
        let rows = collect_rows(mapped)?;
        Ok(rows)
    }

    /// Paginated history query. The filter is shared by the TUI and IPC, so
    /// `total` and `rows` always use exactly the same predicates.
    pub fn list_page(&self, filter: &TaskFilter) -> anyhow::Result<TaskPage> {
        let c = self.inner.lock().unwrap();
        let (where_sql, args) = task_where(filter);
        let count_sql = format!("SELECT COUNT(*) FROM tasks{where_sql}");
        let total: i64 = c.query_row(&count_sql, params_from_iter(args.clone()), |r| r.get(0))?;

        let limit = filter.limit.clamp(1, 500) as i64;
        let offset = filter.offset.min(i64::MAX as usize) as i64;
        let sql = format!(
            "SELECT task_id, from_ws, to_ws, transfer_send_to, attempt, state, terminal, payload, out_head, reason, created_at, hop_state, ledger_state, kind, failure_of \
             FROM tasks{where_sql} ORDER BY created_at DESC, rowid DESC LIMIT ? OFFSET ?"
        );
        let mut row_args = args;
        row_args.push(SqlValue::Integer(limit));
        row_args.push(SqlValue::Integer(offset));
        let mut st = c.prepare(&sql)?;
        let rows = collect_rows(st.query_map(params_from_iter(row_args), row)?)?;
        Ok(TaskPage { rows, total })
    }

    /// Legacy scheduler query. It keeps its old shape for lifecycle callers;
    /// IPC and the TUI use `list_page` for filtered pagination.
    pub fn list(&self, state: Option<&str>, limit: usize) -> anyhow::Result<Vec<TaskRow>> {
        self.list_page(&TaskFilter {
            state: state.map(str::to_owned),
            limit,
            ..Default::default()
        })
        .map(|page| page.rows)
    }

    pub fn detail(&self, task_id: &str) -> anyhow::Result<Option<TaskDetail>> {
        let c = self.inner.lock().unwrap();
        let Some(task) = get_with_conn(&c, task_id)? else {
            return Ok(None);
        };
        let parent = if task.transfer_send_to.is_empty() {
            None
        } else {
            get_with_conn(&c, &task.transfer_send_to)?
        };
        let mut st = c.prepare(
            "WITH RECURSIVE fam(id) AS (
               SELECT ? UNION SELECT task_id FROM tasks, fam WHERE transfer_send_to = fam.id
             ) SELECT task_id, from_ws, to_ws, transfer_send_to, attempt, state, terminal, payload, out_head, reason, created_at, hop_state, ledger_state, kind, failure_of
               FROM tasks WHERE task_id IN fam ORDER BY created_at ASC, rowid ASC",
        )?;
        let children = collect_rows(st.query_map(params![task_id], row)?)?
            .into_iter()
            .filter(|row| row.task_id != task_id)
            .collect();
        Ok(Some(TaskDetail {
            task: task.into(),
            parent,
            children,
        }))
    }

    pub fn graph(&self) -> anyhow::Result<GraphData> {
        let c = self.inner.lock().unwrap();
        let mut by_ws_stmt = c.prepare(
            "SELECT to_ws, state, COUNT(*) FROM tasks GROUP BY to_ws, state ORDER BY to_ws ASC, state ASC",
        )?;
        let by_ws = by_ws_stmt
            .query_map([], |r| {
                Ok(GraphCount {
                    ws: r.get(0)?,
                    state: r.get(1)?,
                    n: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut edge_stmt = c.prepare(
            "SELECT from_ws, to_ws, COUNT(*) FROM tasks WHERE from_ws <> to_ws \
             GROUP BY from_ws, to_ws ORDER BY from_ws ASC, to_ws ASC",
        )?;
        let edges = edge_stmt
            .query_map([], |r| {
                Ok(GraphEdge {
                    from: r.get(0)?,
                    to: r.get(1)?,
                    n: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(GraphData { by_ws, edges })
    }

    pub fn counts(&self) -> anyhow::Result<std::collections::BTreeMap<String, i64>> {
        let c = self.inner.lock().unwrap();
        let mut st = c.prepare("SELECT state, COUNT(*) FROM tasks GROUP BY state")?;
        let mut map = std::collections::BTreeMap::new();
        for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (k, v) = r?;
            map.insert(k, v);
        }
        Ok(map)
    }

    /// Ledger tail: terminal rows (done/failed/cancelled) newest first.
    pub fn ledger(&self, limit: usize) -> anyhow::Result<Vec<LedgerEvent>> {
        let c = self.inner.lock().unwrap();
        let mut st = c.prepare(
            "SELECT task_id, transfer_send_to, from_ws, to_ws, ledger_state, out_head, reason FROM tasks
             WHERE ledger_state IN ('done','failed','cancelled','spawn_failed','adopted') ORDER BY created_at DESC LIMIT ?",
        )?;
        let mut out = vec![];
        for r in st.query_map(params![limit as i64], |r| {
            Ok(LedgerEvent {
                task_id: r.get(0)?,
                transfer_send_to: r.get(1)?,
                from_ws: r.get(2)?,
                to_ws: r.get(3)?,
                state: r.get(4)?,
                out_head: r.get(5)?,
                reason: r.get(6)?,
            })
        })? {
            out.push(r?);
        }
        Ok(out)
    }

    /// Ledger file tail at `<root>/.onlyne/ledger.jsonl` (append-only mirror).
    pub fn ledger_path(root: &Path) -> std::path::PathBuf {
        root.join(".onlyne/ledger.jsonl")
    }
}

fn session_row(r: &rusqlite::Row) -> rusqlite::Result<SessionRecord> {
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
        updated_at: r.get(12)?,
    })
}

const FAULT_COLUMNS: &str = "id,task_id,session_id,generation,seq,desired_json,observed_json,intent,attempt,backend_ref,kind,reason,state,recovery_task_id,created_at";

fn fault_row(r: &rusqlite::Row) -> rusqlite::Result<FaultRecord> {
    Ok(FaultRecord {
        id: r.get(0)?,
        task_id: r.get(1)?,
        session_id: r.get(2)?,
        generation: r.get(3)?,
        seq: r.get(4)?,
        desired_json: r.get(5)?,
        observed_json: r.get(6)?,
        intent: r.get(7)?,
        attempt: r.get(8)?,
        backend_ref: r.get(9)?,
        kind: r.get(10)?,
        reason: r.get(11)?,
        state: r.get(12)?,
        recovery_task_id: r.get(13)?,
        created_at: r.get(14)?,
    })
}

fn intent_row(r: &rusqlite::Row) -> rusqlite::Result<IntentRecord> {
    Ok(IntentRecord {
        op_id: r.get(0)?,
        task_id: r.get(1)?,
        kind: r.get(2)?,
        payload_json: r.get(3)?,
        attempt: r.get(4)?,
        next_attempt_at: r.get(5)?,
        state: r.get(6)?,
        receipt_json: r.get(7)?,
        last_error: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

fn get_with_conn(c: &Connection, task_id: &str) -> anyhow::Result<Option<TaskRow>> {
    let mut st = c.prepare(
        "SELECT task_id, from_ws, to_ws, transfer_send_to, attempt, state, terminal, payload, out_head, reason, created_at, hop_state, ledger_state, kind, failure_of \
         FROM tasks WHERE task_id=?",
    )?;
    let mut rows = st.query(params![task_id])?;
    let Some(r) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(row(r)?))
}

fn collect_rows<I>(rows: I) -> anyhow::Result<Vec<TaskRow>>
where
    I: IntoIterator<Item = rusqlite::Result<TaskRow>>,
{
    rows.into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn task_where(filter: &TaskFilter) -> (String, Vec<SqlValue>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut args = Vec::new();
    match filter.state.as_deref().unwrap_or("all") {
        "" | "all" => {}
        "active" => clauses.push("state IN ('pending','running')".into()),
        state => {
            clauses.push("state=?".into());
            args.push(SqlValue::Text(state.into()));
        }
    }
    if let Some(to_ws) = filter.to_ws.as_deref().filter(|s| !s.is_empty()) {
        clauses.push("to_ws=?".into());
        args.push(SqlValue::Text(to_ws.into()));
    }
    if let Some(from_ws) = filter.from_ws.as_deref().filter(|s| !s.is_empty()) {
        clauses.push("from_ws=?".into());
        args.push(SqlValue::Text(from_ws.into()));
    }
    if let Some(text) = filter.text.as_deref().filter(|s| !s.is_empty()) {
        clauses.push("(payload LIKE ? OR out_head LIKE ? OR reason LIKE ?)".into());
        let pattern = SqlValue::Text(format!("%{text}%"));
        args.extend([pattern.clone(), pattern.clone(), pattern]);
    }
    if let Some(since) = filter.since {
        clauses.push("created_at >= ?".into());
        args.push(SqlValue::Integer(since));
    }
    if filter.retry_only {
        clauses.push("attempt > 1".into());
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, args)
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        task_id: r.get(0)?,
        from_ws: r.get(1)?,
        to_ws: r.get(2)?,
        transfer_send_to: r.get(3)?,
        attempt: r.get(4)?,
        state: TaskState::from_str(&r.get::<_, String>(5)?),
        terminal: r.get(6)?,
        payload: r.get(7).unwrap_or_default(),
        out_head: r.get(8).unwrap_or_default(),
        reason: r.get(9).unwrap_or_default(),
        created_at: r.get(10).unwrap_or_default(),
        hop_state: r.get(11).unwrap_or_default(),
        ledger_state: r.get(12).unwrap_or_default(),
        kind: r.get(13).unwrap_or("normal".to_string()),
        failure_of: r.get(14).ok().flatten(),
    })
}

/// Append one ledger line to `<root>/.onlyne/ledger.jsonl` (best effort).
pub fn append_ledger_line(root: &Path, ev: &LedgerEvent) {
    use std::io::Write;
    let path = Db::ledger_path(root);
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let line = serde_json::json!({
            "ts": chrono::Utc::now().timestamp(),
            "task_id": ev.task_id,
            "transfer_send_to": ev.transfer_send_to,
            "from": ev.from_ws,
            "to": ev.to_ws,
            "state": ev.state,
            "out_head": ev.out_head,
            "reason": ev.reason,
        });
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The intent helpers the retry pump leans on, each guard exercised on its
    /// own: a claim that must not land twice, a backoff write from the wrong
    /// state, a re-arm of a live row, and a receipt that arrives after
    /// exhaustion. These are the at-least-once invariants, so they are pinned
    /// one level below the pump that uses them.
    #[test]
    fn intent_rows_only_move_from_the_state_their_guard_names() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let payload = serde_json::json!({"workspace": "worker", "wire": "---swarm-ctl\n"});
        db.insert_intent("recycle:a:g1", "a", "recycle", &payload, 100, 100)
            .unwrap();
        // A fresh line is pending, unattempted, due at the caller's slot.
        let fresh = db.get_intent("recycle:a:g1").unwrap().unwrap();
        assert_eq!(
            (fresh.state.as_str(), fresh.attempt, fresh.next_attempt_at),
            ("pending", 0, 100)
        );
        assert_eq!(fresh.task_id, "a");
        assert_eq!(fresh.kind, "recycle");
        assert!(fresh.payload_json.contains("swarm-ctl"), "{fresh:?}");
        // A claim by op is atomic and carries the attempt counter with it.
        let first = db
            .claim_intent_op("recycle:a:g1", 100)
            .unwrap()
            .expect("claim");
        assert_eq!((first.state.as_str(), first.attempt), ("claimed", 1));
        assert!(
            db.claim_intent_op("recycle:a:g1", 100).unwrap().is_none(),
            "a second claimer must get nothing"
        );
        // Claiming the oldest due row skips a claimed line too.
        assert!(db.claim_intent(500).unwrap().is_none());
        // Backoff writes only land from `claimed`.
        assert!(
            !db.retry_intent("recycle:nothing", 200, "gone", 150)
                .unwrap(),
            "an unknown op cannot be scheduled"
        );
        assert!(db
            .retry_intent("recycle:a:g1", 202, "connect refused", 150)
            .unwrap());
        let waiting = db.get_intent("recycle:a:g1").unwrap().unwrap();
        assert_eq!(
            (
                waiting.state.as_str(),
                waiting.attempt,
                waiting.next_attempt_at
            ),
            ("pending", 1, 202)
        );
        assert_eq!(waiting.last_error, "connect refused");
        assert!(db.claim_intent(201).unwrap().is_none(), "not due yet");
        assert!(db.claim_intent(202).unwrap().is_some(), "due at its slot");
        assert!(
            db.claim_intent_op("recycle:a:g1", 202).unwrap().is_none(),
            "already claimed"
        );
        // A receipt outranks everything: neither a retry nor an exhaustion can
        // undo an accepted send.
        assert!(db
            .receipt_intent("recycle:a:g1", &serde_json::json!({"at": 210}), 210)
            .unwrap());
        assert!(
            !db.retry_intent("recycle:a:g1", 300, "late", 220).unwrap(),
            "a late backoff write must not reopen an answered line"
        );
        assert!(
            !db.exhaust_intent("recycle:a:g1", "late", 230).unwrap(),
            "a succeeded row cannot be exhausted"
        );
        let done = db.get_intent("recycle:a:g1").unwrap().unwrap();
        assert_eq!(done.state, "succeeded");
        assert!(done.receipt_json.contains("at"), "{done:?}");
        // Re-arming is for terminal rows only, and it starts a fresh budget.
        assert!(db.reset_intent("recycle:a:g1", 300).unwrap());
        assert!(
            !db.reset_intent("recycle:nothing", 300).unwrap(),
            "unknown op"
        );
        let rearmed = db.get_intent("recycle:a:g1").unwrap().unwrap();
        assert_eq!((rearmed.state.as_str(), rearmed.attempt), ("pending", 0));
        assert!(
            rearmed.receipt_json.is_empty() && rearmed.last_error.is_empty(),
            "{rearmed:?}"
        );
        // From `pending` again a receipt is writable: that is the pump's own
        // path after a re-arm.
        assert!(db
            .receipt_intent("recycle:a:g1", &serde_json::json!({"at": 310}), 310)
            .unwrap());
        assert_eq!(db.list_intents(Some("pending")).unwrap().len(), 0);
        assert_eq!(db.list_intents(Some("succeeded")).unwrap().len(), 1);
    }

    /// The supervisor queue's two operator-facing moves — link a recovery, ack
    /// the fault — are one-shot, and both leave the row readable.
    #[test]
    fn fault_rows_link_once_and_ack_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        db.insert_task("f-1", ".", "worker", "", 1, "payload")
            .unwrap();
        let fault = FaultRecord {
            id: 0,
            task_id: "f-1".into(),
            session_id: "f-1".into(),
            generation: 2,
            seq: 7,
            desired_json: "{}".into(),
            observed_json: "{\"agent\":\"running\"}".into(),
            intent: "reconcile:probe_dead".into(),
            attempt: 1,
            backend_ref: "{\"handle\":\"term-f-1\"}".into(),
            kind: "probe_dead".into(),
            reason: "exited".into(),
            state: "open".into(),
            recovery_task_id: None,
            created_at: 10,
        };
        let id = db.insert_fault(&fault).unwrap();
        let stored = db.get_fault(id).unwrap().expect("the row reads back");
        assert_eq!(
            (
                stored.kind.as_str(),
                stored.state.as_str(),
                stored.generation,
                stored.seq
            ),
            ("probe_dead", "open", 2, 7)
        );
        assert_eq!(stored.observed_json, fault.observed_json);
        assert_eq!(stored.backend_ref, fault.backend_ref);
        assert!(
            db.get_fault(id + 1000).unwrap().is_none(),
            "an unknown id is not an acked one"
        );
        // A task row starts life as ordinary scheduled work.
        let task = db.get("f-1").unwrap().unwrap();
        assert_eq!(task.kind, "normal");
        assert!(task.failure_of.is_none());
        // Linking a recovery moves both ends of the lineage in one write.
        db.insert_task("r-1", "worker", ".", "", 1, "recovery report")
            .unwrap();
        db.link_fault_recovery(id, "r-1").unwrap();
        let linked = db.get_fault(id).unwrap().unwrap();
        assert_eq!(linked.state, "recovery_created");
        assert_eq!(linked.recovery_task_id.as_deref(), Some("r-1"));
        let recovery = db.get("r-1").unwrap().unwrap();
        assert_eq!(recovery.kind, "recovery");
        assert_eq!(
            recovery.failure_of.as_deref(),
            Some(id.to_string().as_str())
        );
        assert_eq!(
            db.get("f-1").unwrap().unwrap().kind,
            "normal",
            "the source task keeps its own kind: the lineage runs one way"
        );
        // Ack is a one-way door, and a second ack says so instead of lying.
        assert!(db.ack_fault(id).unwrap());
        assert_eq!(db.get_fault(id).unwrap().unwrap().state, "acked");
        assert!(
            !db.ack_fault(id).unwrap(),
            "a second ack has nothing left to do"
        );
        assert!(!db.ack_fault(id + 1000).unwrap(), "ack of an unknown id");
        assert_eq!(db.list_faults(Some("f-1")).unwrap().len(), 1);
        assert_eq!(
            db.list_faults(None).unwrap().len(),
            1,
            "the queue stays readable"
        );
    }

    /// `operator_revision` is the only `tasks` column repair writes. It must
    /// read back what the write reported, and answer honestly for a task that
    /// does not exist.
    #[test]
    fn operator_revision_stamps_the_task_row_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        db.insert_task("rev-1", ".", "worker", "", 1, "payload")
            .unwrap();
        assert_eq!(
            db.operator_revision("rev-1").unwrap(),
            None,
            "an untouched row carries no stamp"
        );
        let stamp = db.bump_operator_revision("rev-1", "adopt", 1234).unwrap();
        assert_eq!(stamp.as_deref(), Some("repair adopt at 1234"));
        assert_eq!(
            db.operator_revision("rev-1").unwrap().as_deref(),
            Some("repair adopt at 1234")
        );
        db.bump_operator_revision("rev-1", "close", 2345).unwrap();
        assert_eq!(
            db.operator_revision("rev-1").unwrap().as_deref(),
            Some("repair close at 2345"),
            "the newest repair is the one an operator reads"
        );
        assert!(db
            .bump_operator_revision("rev-nope", "ack", 1)
            .unwrap()
            .is_none());
        assert_eq!(db.operator_revision("rev-nope").unwrap(), None);
        // The widened read path must not disturb the ordinary columns.
        let rows = db.list(None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].task_id, "rev-1");
        assert!(rows[0].failure_of.is_none());
        assert_eq!(rows[0].kind, "normal");
        // The paged history view reads the same widened row, so a TUI page can
        // show lineage without a second query.
        let page = db.list_page(&TaskFilter::default()).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].kind, "normal");
        assert!(page.rows[0].failure_of.is_none());
        let detail = db
            .detail("rev-1")
            .unwrap()
            .expect("the row has a detail view");
        assert_eq!(detail.task.kind, "normal");
        assert!(detail.task.failure_of.is_none());
    }

    #[test]
    fn hop_state_migrates_idempotent_on_old_db() {
        // R4 §6 migration edge: a pre-0.7.0 tasks table without hop_state
        // gains the column on open; reopening is a no-op; old rows read ''.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("swarm.db");
        std::fs::create_dir_all(dir.path().join(".onlyne")).unwrap();
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks(task_id TEXT PRIMARY KEY, from_ws TEXT NOT NULL, to_ws TEXT NOT NULL, transfer_send_to TEXT NOT NULL DEFAULT '', attempt INTEGER NOT NULL DEFAULT 1, state TEXT NOT NULL DEFAULT 'pending', terminal TEXT NOT NULL DEFAULT '', payload TEXT NOT NULL DEFAULT '', out_head TEXT NOT NULL DEFAULT '', reason TEXT NOT NULL DEFAULT '', ledger_state TEXT NOT NULL DEFAULT '', created_at INTEGER NOT NULL DEFAULT 0);",
            ).unwrap();
            conn.execute(
                "INSERT INTO tasks(task_id, from_ws, to_ws) VALUES('old1', '.', 'a')",
                [],
            )
            .unwrap();
        }
        // Open through a root pointing at this dir: Db::open must add the
        // column without touching existing rows.
        let root = dir.path();
        // Db::open resolves <root>/.onlyne/swarm.db; relocate the file.
        std::fs::create_dir_all(root.join(".onlyne")).unwrap();
        std::fs::rename(&db_path, root.join(".onlyne/swarm.db")).unwrap();
        let db = Db::open(root).unwrap();
        let db2 = Db::open(root).unwrap(); // idempotent reopen
        let _ = db2;
        db.insert_task("new1", ".", "a", "", 1, "p").unwrap();
        db.set_hop("new1", "busy").unwrap();
        assert_eq!(db.get("new1").unwrap().unwrap().hop_state, "busy");
    }
    fn insert_id(
        db: &Db,
        id: &str,
        from: &str,
        to: &str,
        parent: &str,
        attempt: u32,
        payload: &str,
        created_at: i64,
    ) {
        db.insert_task(id, from, to, parent, attempt, payload)
            .unwrap();
        db.inner
            .lock()
            .unwrap()
            .execute(
                "UPDATE tasks SET created_at=? WHERE task_id=?",
                params![created_at, id],
            )
            .unwrap();
    }

    #[test]
    fn filtered_page_counts_orders_and_pages() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        insert_id(&db, "a", ".", "scout", "", 1, "alpha", 100);
        db.set_state("a", TaskState::Running).unwrap();
        insert_id(&db, "b", "scout", "model", "a", 2, "needle payload", 200);
        db.set_state("b", TaskState::Done).unwrap();
        db.set_ledger("b", "done", "needle output", "").unwrap();
        insert_id(&db, "c", "model", "scout", "a", 3, "plain", 300);
        db.set_state("c", TaskState::Failed).unwrap();
        db.set_ledger("c", "failed", "", "needle reason").unwrap();

        let active = db
            .list_page(&TaskFilter {
                state: Some("active".into()),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(active.total, 1);
        assert_eq!(active.rows[0].task_id, "a");

        let first = db
            .list_page(&TaskFilter {
                text: Some("needle".into()),
                retry_only: true,
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(first.total, 2);
        assert_eq!(first.rows[0].task_id, "c");
        let second = db
            .list_page(&TaskFilter {
                text: Some("needle".into()),
                retry_only: true,
                limit: 1,
                offset: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(second.rows[0].task_id, "b");

        let scoped = db
            .list_page(&TaskFilter {
                state: Some("all".into()),
                to_ws: Some("scout".into()),
                from_ws: Some("model".into()),
                since: Some(250),
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(scoped.total, 1);
        assert_eq!(scoped.rows[0].task_id, "c");
    }

    #[test]
    fn graph_and_detail_expose_aggregates_and_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        insert_id(&db, "root", ".", "scout", "", 1, "root payload", 100);
        db.set_state("root", TaskState::Running).unwrap();
        insert_id(
            &db,
            "child",
            "scout",
            "model",
            "root",
            2,
            "child payload",
            200,
        );
        db.set_state("child", TaskState::Done).unwrap();
        db.set_ledger("child", "done", "child output", "").unwrap();
        insert_id(
            &db,
            "self",
            "model",
            "model",
            "root",
            1,
            "self payload",
            300,
        );
        db.set_state("self", TaskState::Failed).unwrap();
        db.set_ledger("self", "failed", "", "self reason").unwrap();

        let graph = db.graph().unwrap();
        assert!(graph
            .by_ws
            .iter()
            .any(|row| row.ws == "scout" && row.state == "running" && row.n == 1));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.from == "." && edge.to == "scout" && edge.n == 1));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.from == "scout" && edge.to == "model" && edge.n == 1));
        assert!(!graph.edges.iter().any(|edge| edge.from == edge.to));

        let detail = db.detail("child").unwrap().unwrap();
        assert_eq!(detail.task.payload, "child payload");
        assert_eq!(detail.task.out_head, "child output");
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(
            json.pointer("/task/payload").and_then(|v| v.as_str()),
            Some("child payload")
        );
        assert!(json.pointer("/parent/payload").is_none());
        assert_eq!(detail.parent.unwrap().task_id, "root");
        let root = db.detail("root").unwrap().unwrap();
        assert_eq!(root.children.len(), 2);
        assert!(root.children.iter().any(|row| row.task_id == "child"));
        assert!(root.children.iter().any(|row| row.reason == "self reason"));
    }

    #[test]
    fn insert_is_idempotent_and_family_follows_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        assert!(db.insert_task("a", ".", "x", "", 1, "pa").unwrap());
        assert!(!db.insert_task("a", ".", "x", "", 1, "pa").unwrap());
        db.insert_task("b", "x", "y", "a", 1, "pb").unwrap();
        assert_eq!(db.family("a").unwrap().len(), 2);
        assert_eq!(db.get("a").unwrap().unwrap().payload, "pa");
    }

    #[test]
    fn unsupported_waiting_schema_fails_fast() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".onlyne/swarm.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks(task_id TEXT PRIMARY KEY, reply_to TEXT, pending_replies INTEGER);",
        )
        .unwrap();
        drop(conn);
        let err = match Db::open(dir.path()) {
            Ok(_) => panic!("legacy schema unexpectedly opened"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("unsupported legacy tasks schema"));
    }

    #[test]
    fn protocol_tables_migrate_and_versioned_sessions_are_ordered() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let conn = db.inner.lock().unwrap();
        for table in ["schema_marker", "sessions", "intents", "faults"] {
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    params![table],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
        }
        drop(conn);
        let base = VersionedSession {
            agent_state: "ready".into(),
            delivery_state: "none".into(),
            resource_state: "attached".into(),
            public_lifecycle: "idle".into(),
            recovery_substate: "".into(),
            desired_json: "{}".into(),
            observed_json: "{}".into(),
            generation: 2,
            seq: 3,
            backend_ref: "{}".into(),
            mismatch_count: 0,
            updated_at: 10,
        };
        assert!(db.upsert_session("t", &base).unwrap());
        assert!(!db
            .upsert_session(
                "t",
                &VersionedSession {
                    seq: 2,
                    ..base.clone()
                }
            )
            .unwrap());
        assert!(!db.upsert_session("t", &base).unwrap());
        assert!(db
            .upsert_session("t", &VersionedSession { seq: 4, ..base })
            .unwrap());
    }

    #[test]
    fn intent_claim_receipt_and_duplicate_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        db.insert_intent("op", "task", "send", &serde_json::json!({"x": 1}), 1, 0)
            .unwrap();
        assert!(db
            .insert_intent("op", "task", "send", &serde_json::json!({"x": 2}), 1, 0)
            .is_err());
        let claimed = db.claim_intent(1).unwrap().unwrap();
        assert_eq!(claimed.attempt, 1);
        assert!(db
            .receipt_intent("op", &serde_json::json!({"ok": true}), 2)
            .unwrap());
        assert!(!db.exhaust_intent("op", "late", 3).unwrap());
    }

    #[test]
    fn ledger_tail_lists_terminal_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        db.insert_task("a", ".", "x", "", 1, "p").unwrap();
        db.set_state("a", TaskState::Running).unwrap();
        assert!(db.ledger(10).unwrap().is_empty());
        db.set_state("a", TaskState::Done).unwrap();
        db.set_ledger("a", "done", "out head", "").unwrap();
        let tail = db.ledger(10).unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].task_id, "a");
        assert_eq!(tail[0].out_head, "out head");
    }
}
