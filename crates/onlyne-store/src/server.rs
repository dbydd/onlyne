use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use onlyne_proto::text::SchemaMismatch;
use onlyne_proto::{
    Envelope, Event, FaultEvent, LedgerQuery, LedgerState, LedgerStateEvent, Lifecycle, MsgKind,
    Outcome, Principal, QueryFaultsArgs, QuerySessionsArgs,
};
use rusqlite::types::{Type, Value as SqlValue};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;

use crate::error::{StoreError, StoreResult};
use crate::liveness::LiveSessions;
use crate::transition_allowed;

/// Server store schema revision. The `hop` column set this to 2: the ledger
/// keeps the hop count of `Causality`, which `onlyne handoff` reads back to
/// extend a chain. The `expires_at` and `requeued` columns were applied in
/// place, so they never moved the marker. Version 3 is the tuple rebuild: the
/// `sessions` row lost its `public_lifecycle` column, which cannot be taken
/// back from an existing file, so an old layout is refused rather than carried.
/// Version 4 adds the `ghost_sweeps` audit table: the server settles a `working`
/// mirror row whose task ledger row already reached a terminal state, and each
/// settlement writes one row there. A marker-3 file carries no such table, so it
/// stops at the door on the same string every other mismatch prints.
/// The ledger's five family-metadata columns — `family`, `hop_budget`,
/// `origin`, `deadline`, `labels_json` — were applied in place beside
/// `expires_at` and `requeued`, so they never moved the marker.
/// Version 5 rekeys the mirror: a session row is addressed by `session_id`
/// rather than by the delivery it happens to serve, carries `last_seen` beside
/// `updated_at`, and the deliveries it serves are recorded in `session_tasks`.
/// A marker-4 file holds rows under the old key and no bindings table, which
/// cannot be read back as this layout, so it stops at the door on the same
/// string every other mismatch prints.
/// Version 6 adds the `hook_cursors` table: an event hook is delivered
/// at-least-once, so each declared hook records the last event `seq` it
/// handled successfully and resumes from there after a restart. A marker-5
/// file holds no such row, so it stops at the door on the same string every
/// other mismatch prints.
/// The client store keeps its own revision.
const SERVER_SCHEMA_VERSION: i64 = 6;
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
  session_id TEXT PRIMARY KEY,
  role TEXT NOT NULL,
  generation INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  agent_state TEXT NOT NULL,
  delivery_state TEXT NOT NULL,
  resource_state TEXT NOT NULL,
  recovery_substate TEXT NOT NULL,
  -- The (generation, seq) gate. The session reducer's isolate-after-N and
  -- terminate-after-N policy needs a persisted counter, so this pair carries
  -- DEFAULT_ISOLATE_AFTER and DEFAULT_TERMINATE_AFTER.
  desired_json TEXT NOT NULL,
  -- The client's published projection whole, lifecycle included: there is no
  -- column beside it to fall back to, and every reader of the mirror parses the
  -- lifecycle out of these bytes.
  observed_json TEXT NOT NULL,
  mismatch_count INTEGER NOT NULL,
  -- The mirror's own freshness: what a reader judges a stale row by. v1's
  -- mirror could be hours old and read as current.
  last_seen TEXT NOT NULL,
  -- When the projection content last moved. A beat that only refreshes
  -- `last_seen` leaves it alone.
  updated_at TEXT NOT NULL
);
-- The role-addressed reads: one role's sessions, which the note rule and the
-- stale scan ask for.
CREATE INDEX IF NOT EXISTS sessions_role_idx ON sessions(role);
-- The order `list_sessions` reads in, ascending so the read walks it backwards:
-- a descending index does not satisfy `updated_at DESC, rowid DESC` — SQLite
-- leaves a `TEMP B-TREE FOR LAST TERM` and spills it to a temporary file on
-- every read, and a reader that polls once a second turns that into megabytes
-- per second of writes nothing asked for.
CREATE INDEX IF NOT EXISTS sessions_updated_idx ON sessions(updated_at);
-- Which delivery a session serves, and which sessions have served a delivery.
-- The mirror row answers for a session; this table is the only place a
-- delivery binding lives, so a session that served one delivery and then
-- another is one row here twice rather than two mirror rows.
CREATE TABLE IF NOT EXISTS session_tasks(
  session_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  bound_at TEXT NOT NULL,
  released_at TEXT,
  PRIMARY KEY (session_id, task_id)
);
-- The reverse read: the session serving a task.
CREATE INDEX IF NOT EXISTS session_tasks_task_idx ON session_tasks(task_id);
-- The open binding of one session: at most one row per session is unreleased,
-- because a session serves one delivery at a time.
CREATE INDEX IF NOT EXISTS session_tasks_open_idx ON session_tasks(session_id, released_at);
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
  -- Nullable because retention pruning clears an acknowledged body after the
  -- cutoff.
  body_json TEXT,
  -- Causality's hop count from the root task. `parent_task` alone gives the
  -- chain's shape, not its depth; `onlyne handoff` reads both back to extend
  -- the chain, so the row stores the counter beside the link.
  hop INTEGER NOT NULL DEFAULT 0,
  -- Persisted expiry deadline of a ttl note, so a restarted server can re-arm
  -- its sweep.
  expires_at TEXT,
  -- Times this row has moved from in_flight back to queued.
  requeued INTEGER NOT NULL DEFAULT 0,
  -- The family's root task id, read off the envelope's causality. `parent_task`
  -- gives the chain's shape, and this names the arc every hop of one run
  -- belongs to, which `onlyne ledger` prints beside the hop.
  family TEXT,
  -- The hops the family may spend, set by whoever started the run.
  hop_budget INTEGER,
  -- The role the family reports home to, carried on every row of the run.
  origin TEXT,
  -- Wall-clock bound for the whole family, in the RFC 3339 shape `expires_at`
  -- uses.
  deadline TEXT,
  -- The causality's free-form labels as JSON text.
  labels_json TEXT
);
CREATE INDEX IF NOT EXISTS ledger_state_enqueued_idx ON ledger(state,enqueued_at);
CREATE INDEX IF NOT EXISTS ledger_task_idx ON ledger(task);
CREATE INDEX IF NOT EXISTS ledger_kind_state_idx ON ledger(kind,state);
-- The ledger listing's own reading order, on the same terms as the sessions
-- index above. A listing filtered by `state` keeps using `ledger_state_enqueued_idx`
-- and reads that backwards.
CREATE INDEX IF NOT EXISTS ledger_enqueued_idx ON ledger(enqueued_at);
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
  -- Encoded from the kernel's unix seconds through this crate's own helper on
  -- every write.
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS faults_task_kind_generation_idx ON faults(task_id,kind,generation);
CREATE INDEX IF NOT EXISTS faults_state_idx ON faults(state);
-- The ghost sweep's own audit trail, one row per `working` mirror row the server
-- settled because the task's ledger row had already reached a terminal state.
-- `seq_before` and `seq_after` are the mirror row's two versions, so an operator
-- reads the exact write the pass made straight off this row.
CREATE TABLE IF NOT EXISTS ghost_sweeps(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL,
  role TEXT NOT NULL,
  session_id TEXT NOT NULL,
  generation INTEGER NOT NULL,
  seq_before INTEGER NOT NULL,
  seq_after INTEGER NOT NULL,
  outcome TEXT NOT NULL,
  evidence TEXT NOT NULL,
  swept_at TEXT NOT NULL
);
-- The listing's own reading order, on the same terms as the sessions index
-- above: ascending, so `swept_at DESC, rowid DESC` walks it backwards.
CREATE INDEX IF NOT EXISTS ghost_sweeps_swept_at_idx ON ghost_sweeps(swept_at);
CREATE TABLE IF NOT EXISTS inbox_cursors(
  role TEXT PRIMARY KEY,
  last_msg_id TEXT,
  last_seq INTEGER NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS hook_cursors(
  hook TEXT PRIMARY KEY,
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

#[derive(Clone, Debug)]
pub struct ServerLedger {
    path: PathBuf,
    retention_days: i64,
    /// The one connection that writes, behind its own lock.
    inner: Arc<Mutex<Connection>>,
    /// The read-only handles every query runs on, so a reader is never queued
    /// behind the writer above.
    readers: ReadPool,
    /// Writes this store made against `sessions` since it opened: one bump per
    /// `project_session`, `rebind_session`, `publish_mirror_outcome`, and
    /// `flush_last_seen`. A heartbeat that landed in memory counts as none.
    session_rows_written: Arc<AtomicU64>,
    /// The beats this process took and has not written down.
    live: Arc<LiveSessions>,
}

impl ServerLedger {
    pub fn open(path: impl AsRef<Path>, retention_days: u32) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = open_connection(
            &path,
            "server",
            SERVER_MARKER,
            SERVER_DDL,
            SERVER_SCHEMA_VERSION,
        )?;
        // The writer above ran the schema gate — the marker check, the DDL, the
        // in-place column adds — so the readers below only ever open a file
        // this process has already accepted.
        let readers = ReadPool::open(&path)?;
        Ok(Self {
            path,
            retention_days: i64::from(retention_days).max(1),
            inner: Arc::new(Mutex::new(conn)),
            readers,
            session_rows_written: Arc::new(AtomicU64::new(0)),
            live: Arc::new(LiveSessions::default()),
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
        let conn = self.read()?;
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

    /// Write one mirror row, and bind the delivery the write carries.
    ///
    /// The row is addressed by session id and the `(generation, seq)` gate is
    /// the row's own, as it was when the row answered for a task. A write that
    /// lands binds its delivery in the same transaction: a session serves one
    /// delivery at a time, so taking this one releases whatever the session was
    /// on before. A write the gate refuses changes nothing, the binding
    /// included — the row it was refused by is the newer word.
    ///
    /// A write that lands carries the session's `last_seen` forward, so the
    /// beats it has already outlived are spent: [`crate::liveness`] is told
    /// what the row now holds and drops the ones that are no longer newer.
    pub fn project_session(&self, write: &SessionWrite) -> StoreResult<bool> {
        let conn = self.conn()?;
        self.note_session_row_write();
        let tx = conn.unchecked_transaction()?;
        let changed = project_session_conn(&tx, write)?;
        tx.commit()?;
        if changed {
            self.live.settle(&write.session_id, write.last_seen);
        }
        Ok(changed)
    }

    /// Move one mirror row to the session the write names, and write it there.
    ///
    /// This is what `repair rebind` means once a row is addressed by its
    /// session: the operator says the delivery is now carried by another
    /// session, so the row moves to that address instead of a second row
    /// appearing beside it. The bindings move with it, since they name the same
    /// session. A row already sitting at the new address is the one being
    /// replaced, so it goes first: the operator's word is the newer fact.
    pub fn rebind_session(&self, from_session_id: &str, write: &SessionWrite) -> StoreResult<bool> {
        let conn = self.conn()?;
        self.note_session_row_write();
        let tx = conn.unchecked_transaction()?;
        if from_session_id != write.session_id {
            tx.execute(
                "DELETE FROM session_tasks WHERE session_id=?",
                params![write.session_id],
            )?;
            tx.execute(
                "DELETE FROM sessions WHERE session_id=?",
                params![write.session_id],
            )?;
            tx.execute(
                "UPDATE session_tasks SET session_id=? WHERE session_id=?",
                params![write.session_id, from_session_id],
            )?;
            tx.execute(
                "UPDATE sessions SET session_id=? WHERE session_id=?",
                params![write.session_id, from_session_id],
            )?;
        }
        let changed = project_session_conn(&tx, write)?;
        tx.commit()?;
        if changed {
            // The row moved, so a beat taken under the old address is no longer
            // this row's. The write's own clock is what the row now holds.
            self.live.settle(from_session_id, write.last_seen);
            self.live.settle(&write.session_id, write.last_seen);
        }
        Ok(changed)
    }

    /// Take one delivery for a session: a `session_tasks` row with `bound_at`.
    ///
    /// Answers whether the binding moved, which a pair already open does not.
    pub fn open_binding(
        &self,
        session_id: &str,
        task_id: &str,
        bound_at: i64,
    ) -> StoreResult<bool> {
        let conn = self.conn()?;
        Ok(open_binding_conn(&conn, session_id, task_id, bound_at)? > 0)
    }

    /// Stop serving one delivery: its `session_tasks` row gets `released_at`.
    ///
    /// Only an open binding is released, so the first release is the one the
    /// row keeps and a second call changes nothing.
    pub fn release_binding(
        &self,
        session_id: &str,
        task_id: &str,
        released_at: i64,
    ) -> StoreResult<bool> {
        let conn = self.conn()?;
        Ok(release_binding_conn(&conn, session_id, task_id, released_at)? == 1)
    }

    /// The delivery a session is on now, when it is on one.
    ///
    /// At most one binding of a session is open, so the order here only decides
    /// which row a hand-written pair of open bindings answers with.
    pub fn open_binding_of(&self, session_id: &str) -> StoreResult<Option<SessionBindingRow>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT session_id,task_id,bound_at,released_at FROM session_tasks WHERE session_id=? AND released_at IS NULL ORDER BY bound_at DESC,task_id DESC LIMIT 1",
                params![session_id],
                session_binding_row,
            )
            .optional()?)
    }

    /// Publish a late mirror verdict when the stored projection bytes still
    /// match the bytes the server compared. The stored tuple remains the
    /// session version while `observed_json` carries the task verdict.
    ///
    /// `last_seen` is left where it stands: this write says the projection
    /// gained a verdict, not that the session was heard from, and the beats
    /// that were newer than the row keep answering for it
    /// ([`ServerLedger::beat_session`]).
    pub fn publish_mirror_outcome(
        &self,
        session_id: &str,
        observed_json: &str,
        expected_observed_json: &str,
        updated_at: i64,
    ) -> StoreResult<bool> {
        let conn = self.conn()?;
        self.note_session_row_write();
        let changed = conn.execute(
            "UPDATE sessions SET observed_json=?,updated_at=? WHERE session_id=? AND observed_json=?",
            params![
                observed_json,
                unix_to_rfc3339(updated_at),
                session_id,
                expected_observed_json
            ],
        )?;
        Ok(changed == 1)
    }

    /// Take one beat for a session whose projection did not change.
    ///
    /// This is the whole of v2's liveness rule, and it is deliberately not a
    /// projection write: `(generation, seq)`, `updated_at`, and the event
    /// stream are all left exactly as they were, and the beat reaches the row
    /// only when the row is at least `flush_after_secs` behind — the interval
    /// the server states beside its call, which is also the reader's worst-case
    /// staleness. Between those flushes the beat lives in memory, where every
    /// read of the row picks it up ([`crate::liveness`]).
    ///
    /// The caller supplies the interval rather than this store: it is a promise
    /// about what a reader of `last_seen` is owed, and the server is the layer
    /// that knows the cluster's presence window. The interval is floored at one
    /// second, so a spec that asks for a window below it gets one flush per
    /// second rather than one per beat — the v1 write rate this slice removes.
    ///
    /// Answers whether the beat reached the table.
    pub fn beat_session(
        &self,
        session_id: &str,
        at: i64,
        flush_after_secs: i64,
    ) -> StoreResult<bool> {
        // The memory entry moves first. A reader arriving between the two steps
        // below must be handed this beat, never the value it replaced.
        self.live.note(session_id, at);
        let Some(stored) = self.stored_session_row(session_id)? else {
            // Nothing to refresh: a beat for a session with no row is not a row
            // this process can make fresher. The entry stays for the row that
            // may yet be written, and a row that never appears is read by
            // nobody.
            return Ok(false);
        };
        if at.saturating_sub(stored.last_seen) < flush_after_secs.max(1) {
            return Ok(false);
        }
        let flushed = self.flush_last_seen(session_id, at)?;
        if flushed {
            self.live.settle(session_id, at);
        }
        Ok(flushed)
    }

    /// Write one mirror row's `last_seen`, and nothing else.
    ///
    /// The row is the durable half of a beat: what a restarted server, and any
    /// reader of the file rather than of the cluster, has to judge freshness
    /// by. A mirror whose `last_seen` froze at the last content change is the
    /// v1 defect that column exists to fix, and it is why the row carries it at
    /// all — [`ServerLedger::beat_session`] is the only caller, and it decides
    /// when the row is worth reaching.
    fn flush_last_seen(&self, session_id: &str, last_seen: i64) -> StoreResult<bool> {
        let conn = self.conn()?;
        self.note_session_row_write();
        let changed = conn.execute(
            "UPDATE sessions SET last_seen=? WHERE session_id=?",
            params![unix_to_rfc3339(last_seen), session_id],
        )?;
        Ok(changed == 1)
    }

    /// One mirror row, addressed by its session id: the live `last_seen` this
    /// process holds when it has a newer one, the persisted value otherwise.
    pub fn get_session_row(&self, session_id: &str) -> StoreResult<Option<ServerSessionRow>> {
        Ok(self
            .stored_session_row(session_id)?
            .map(|row| self.with_live_seen(row)))
    }

    /// One mirror row exactly as the file holds it.
    ///
    /// The liveness layer needs the persisted value on its own — the interval
    /// is measured from it — and a caller that wants what a reader is handed
    /// wants [`ServerLedger::get_session_row`] instead.
    fn stored_session_row(&self, session_id: &str) -> StoreResult<Option<ServerSessionRow>> {
        let conn = self.read()?;
        let row = conn
            .query_row(
                &format!(
                    "SELECT {} FROM sessions WHERE session_id=?",
                    session_columns()
                ),
                params![session_id],
                session_row,
            )
            .optional()?;
        Ok(row)
    }

    /// The freshest `last_seen` this process can answer for one stored row.
    ///
    /// Every read of the table passes through here. A reader must never be
    /// handed a value the server has already outlived — that is v1's frozen
    /// mirror, and it is the failure this column was added to end — so the
    /// answer is the newer of the row's value and the beat held in memory.
    fn with_live_seen(&self, mut row: ServerSessionRow) -> ServerSessionRow {
        row.last_seen = self.live.freshest(&row.session_id, row.last_seen);
        row
    }

    /// The mirror row serving one delivery, read through its binding.
    ///
    /// A reader asks "the session serving this task"; the binding is where that
    /// fact lives, and the row's own `task_id` is derived from it either way.
    pub fn session_row_for_task(&self, task_id: &str) -> StoreResult<Option<ServerSessionRow>> {
        let conn = self.read()?;
        let row = conn
            .query_row(
                &format!(
                    "SELECT {} FROM sessions WHERE session_id={SESSION_ID_FOR_TASK}",
                    session_columns()
                ),
                params![task_id],
                session_row,
            )
            .optional()?;
        Ok(row.map(|row| self.with_live_seen(row)))
    }

    pub fn list_sessions(&self, filter: QuerySessionsArgs) -> StoreResult<Vec<ServerSessionRow>> {
        let conn = self.read()?;
        let mut clauses = Vec::new();
        let mut args = Vec::new();
        if let Some(task_id) = filter.task_id {
            // The delivery filter asks which session served the task, which is
            // the bindings table's answer. Both spellings of a binding count: a
            // delivery that ended still names the session that carried it, and
            // a reader of a settled delivery still asks for it.
            clauses.push(
                "session_id IN (SELECT session_id FROM session_tasks WHERE task_id=?)".to_string(),
            );
            args.push(SqlValue::Text(task_id));
        }
        if let Some(role) = filter.role {
            clauses.push("role=?".to_string());
            args.push(SqlValue::Text(role));
        }
        if let Some(lifecycle) = filter.lifecycle {
            // The mirror holds one copy of the published projection, in
            // `observed_json`, and the lifecycle is a key inside it. The row's
            // own dimensions stay columns; this one stays derived.
            //
            // Bytes that do not parse, or that never carried the key, read back
            // as `created` on the row's own read path, which decodes the mirror
            // and falls back to the columns. The filter has to agree: JSON1
            // answers NULL for a key it cannot find and fails outright on bytes
            // that are not JSON, and a scan that dropped such a row would leave
            // a stale session without ever earning its fault row.
            clauses.push(
                "IFNULL(CASE WHEN json_valid(observed_json) THEN json_extract(observed_json,'$.lifecycle') END,?)=?".to_string(),
            );
            args.push(SqlValue::Text(string_tag(&Lifecycle::Created)?));
            args.push(SqlValue::Text(string_tag(&lifecycle)?));
        }
        let where_sql = where_sql(&clauses);
        let limit = sql_limit(filter.limit);
        args.push(SqlValue::Integer(limit));
        let sql = sessions_list_sql(&where_sql);
        let rows = conn
            .prepare(&sql)?
            .query_map(params_from_iter(args), session_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|row| self.with_live_seen(row))
            .collect())
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

    /// How many deliveries are queued for one role's inbox, counted exactly.
    ///
    /// [`ServerLedger::queued_for`] takes a limit, so a caller that counted its
    /// rows would report the cap as the depth: an operator reading "512" when
    /// the truth is 900 reads a saturated role as a full one. The rows are the
    /// ones `pull` would hand this role, so `note` rows are left out exactly as
    /// `relay::pull` leaves them out (`crates/onlyne-server/src/relay.rs`).
    pub fn queued_count_for(&self, role: &str) -> StoreResult<u32> {
        let conn = self.read()?;
        let count = conn
            .query_row(
                "SELECT COUNT(*) FROM ledger WHERE state=? AND json_extract(to_json,'$.role.role')=? AND kind<>?",
                params![LedgerState::Queued.as_str(), role, MsgKind::Note.as_str()],
                |row| row.get::<_, u32>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(count)
    }

    pub fn in_flight_for(&self, role: &str) -> StoreResult<Vec<LedgerRow>> {
        self.ledger_for_role(role, LedgerState::InFlight, 500)
    }

    /// `msg_id` plus parsed deadline for every queued or in-flight row whose
    /// `expires_at` is set, oldest deadline first.
    pub fn pending_expiries(&self) -> StoreResult<Vec<(String, DateTime<Utc>)>> {
        let conn = self.read()?;
        let rows = conn
            .prepare(
                "SELECT msg_id, expires_at FROM ledger WHERE state IN ('queued','in_flight') AND expires_at IS NOT NULL ORDER BY expires_at,rowid",
            )?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(msg_id, expires_at)| {
                DateTime::parse_from_rfc3339(&expires_at)
                    .ok()
                    .map(|deadline| (msg_id, deadline.with_timezone(&Utc)))
            })
            .collect())
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
            "UPDATE ledger SET state='queued',requeued=requeued+1 WHERE msg_id=?",
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

    /// Move one row to `rejected` and publish its `ledger_state` event in the
    /// same transaction. The automatic requeue gate uses this when the row has
    /// used its requeue budget.
    pub fn fail_one(&self, msg_id: &str, reason: &str) -> StoreResult<LedgerRow> {
        let conn = self.conn()?;
        let current = ledger_by_msg_id(&conn, msg_id)?;
        ensure_transition_allowed(current.state, LedgerState::Rejected)?;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE ledger SET state='rejected',reason=? WHERE msg_id=?",
            params![reason, msg_id],
        )?;
        let updated = ledger_by_msg_id(&tx, msg_id)?;
        let event = ledger_state_event(&updated, LedgerState::Rejected, Some(reason.to_string()))?;
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
        let conn = self.read()?;
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
        let sql = ledger_list_sql(&where_sql(&clauses));
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
        let conn = self.read()?;
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
        let conn = self.read()?;
        events_since_conn(&conn, seq, limit)
    }

    pub fn event_head(&self) -> StoreResult<i64> {
        let conn = self.read()?;
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
        let conn = self.read()?;
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

    /// Persist one ghost-sweep audit row and return its id.
    pub fn record_ghost_sweep(&self, sweep: &GhostSweepRow) -> StoreResult<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO ghost_sweeps(task_id,role,session_id,generation,seq_before,seq_after,outcome,evidence,swept_at) VALUES(?,?,?,?,?,?,?,?,?)",
            params![
                sweep.task_id,
                sweep.role,
                sweep.session_id,
                sweep.generation,
                sweep.seq_before,
                sweep.seq_after,
                string_tag(&sweep.outcome)?,
                sweep.evidence,
                unix_to_rfc3339(sweep.swept_at)
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// The recorded sweeps, newest first.
    ///
    /// The order is the sessions listing's order: `swept_at DESC, rowid DESC`
    /// against an ascending index, which SQLite walks backwards. `rowid` breaks
    /// the ties inside one second, and it breaks them in the order the pass
    /// wrote them.
    pub fn list_ghost_sweeps(&self, limit: u32) -> StoreResult<Vec<GhostSweepRow>> {
        let conn = self.read()?;
        let rows = conn
            .prepare(&ghost_sweeps_list_sql())?
            .query_map(params![sql_limit(limit)], ghost_sweep_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn cursor_for(&self, role: &str) -> StoreResult<Option<CursorRow>> {
        let conn = self.read()?;
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

    /// The last event `seq` one hook handled successfully, `None` for a hook
    /// this cluster has never run.
    ///
    /// This is the resume point of the at-least-once rule: a hook that
    /// restarted picks up after the last event it handled, so nothing between
    /// the cursor and the head is skipped (`docs/v2-CONTRACT.md` §"Slice 7").
    pub fn hook_cursor(&self, hook: &str) -> StoreResult<Option<i64>> {
        let conn = self.read()?;
        Ok(conn
            .query_row(
                "SELECT last_seq FROM hook_cursors WHERE hook=?",
                params![hook],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Record the last event `seq` one hook handled successfully. A `seq` at or
    /// below the recorded one is left alone: a worker that resumed from an
    /// older row after a failure must not walk the cursor backwards.
    pub fn set_hook_cursor(&self, hook: &str, seq: i64) -> StoreResult<bool> {
        let conn = self.conn()?;
        let updated_at = rfc3339(Utc::now());
        Ok(conn.execute(
            "INSERT INTO hook_cursors(hook,last_seq,updated_at) VALUES(?,?,?)
             ON CONFLICT(hook) DO UPDATE SET last_seq=excluded.last_seq,updated_at=excluded.updated_at
             WHERE excluded.last_seq > hook_cursors.last_seq",
            params![hook, seq, updated_at],
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
        let conn = self.read()?;
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

    /// One connection from the read-only pool.
    ///
    /// Every statement that only reads goes here rather than through
    /// [`ServerLedger::conn`]: the writer's lock is held for the whole of a
    /// write, including the part where SQLite waits on the file lock, and a
    /// board refresh that shares it waits out the delivery path.
    fn read(&self) -> StoreResult<MutexGuard<'_, Connection>> {
        self.readers.get()
    }

    /// Record one statement this store executed against `sessions`.
    fn note_session_row_write(&self) {
        self.session_rows_written.fetch_add(1, Ordering::Relaxed);
    }

    /// Statements this store has executed against the `sessions` table.
    ///
    /// The projection writes, the mirror-outcome publishes, the liveness
    /// flushes, and the address moves a rebind makes all count here. It is the
    /// observation a liveness claim is made against: "a beat that changed
    /// nothing wrote no row" is answered by this number rather than by reading
    /// the code, and a test that reads it does not have to be a party to the
    /// write path.
    pub fn session_row_writes(&self) -> u64 {
        self.session_rows_written.load(Ordering::Relaxed)
    }
}

/// How many read-only connections the query paths share.
///
/// WAL lets readers run beside the writer, so the pool is about *width*: a
/// board, a TUI, and an operator's `onlyne sessions` are three reads that may
/// be in flight at once, and four leaves room for the server's own scans
/// without any of them waiting on another. Each of these is one file handle;
/// the pool never grows, because every read behind it is a point lookup or an
/// indexed listing.
const READ_POOL_SIZE: usize = 4;

/// The read-only handles a query runs on.
///
/// These are opened `SQLITE_OPEN_READ_ONLY`, so a statement that tried to write
/// here would fail instead of quietly becoming a second writer: the one writer
/// is the connection above, and this pool is the answer to "the reader should
/// not queue behind it".
#[derive(Clone, Debug)]
struct ReadPool {
    path: PathBuf,
    conns: Arc<Vec<Mutex<Connection>>>,
    next: Arc<AtomicUsize>,
}

impl ReadPool {
    fn open(path: &Path) -> StoreResult<Self> {
        let mut conns = Vec::with_capacity(READ_POOL_SIZE);
        for _ in 0..READ_POOL_SIZE {
            conns.push(Mutex::new(open_reader(path)?));
        }
        Ok(Self {
            path: path.to_path_buf(),
            conns: Arc::new(conns),
            next: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// One connection, round-robin, so two reads in flight land on two.
    fn get(&self) -> StoreResult<MutexGuard<'_, Connection>> {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.conns.len();
        self.conns[index].lock().map_err(|_| {
            StoreError::Sqlite(format!(
                "read connection mutex poisoned for {}",
                self.path.display()
            ))
        })
    }
}

/// The DDL revision travels beside the marker: the two databases evolve apart,
/// so the server store states its own number instead of sharing one constant
/// that a change to either DDL would invalidate for both.
pub(crate) fn open_connection(
    path: &Path,
    which: &'static str,
    marker: &str,
    ddl: &str,
    schema_version: i64,
) -> StoreResult<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::Sqlite(e.to_string()))?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    ensure_schema(&conn, which, marker, ddl, schema_version)?;
    Ok(conn)
}

fn ensure_schema(
    conn: &Connection,
    which: &'static str,
    marker: &str,
    ddl: &str,
    schema_version: i64,
) -> StoreResult<()> {
    let tables = user_tables(conn)?;
    let legacy: Vec<String> = tables
        .iter()
        .filter(|name| is_legacy_table(name))
        .cloned()
        .collect();
    if !legacy.is_empty() {
        return Err(StoreError::unsupported_schema(
            which,
            SchemaMismatch::LegacyTables { names: legacy },
        ));
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
        Some((version, protocol)) if version == schema_version && protocol == PROTOCOL_VERSION => {}
        Some((version, protocol)) => {
            // The two revisions are reported apart: a file from an older build
            // and a file from a build that speaks another protocol are different
            // problems, and an operator who is told only "unsupported" has to
            // guess which one they have.
            return Err(if version != schema_version {
                StoreError::unsupported_schema(
                    which,
                    SchemaMismatch::Version {
                        found: version,
                        expected: schema_version,
                    },
                )
            } else {
                StoreError::unsupported_schema(
                    which,
                    SchemaMismatch::Protocol {
                        found: protocol,
                        expected: PROTOCOL_VERSION,
                    },
                )
            });
        }
        None => {
            let marker_count: i64 =
                conn.query_row("SELECT COUNT(*) FROM schema_marker", [], |r| r.get(0))?;
            if marker_count > 0 || !tables.is_empty() {
                return Err(StoreError::unsupported_schema(
                    which,
                    SchemaMismatch::NotEmpty {
                        tables: tables.len(),
                    },
                ));
            }
            conn.execute(
                "INSERT INTO schema_marker(name,version,protocol_version) VALUES(?,?,?)",
                params![marker, schema_version, PROTOCOL_VERSION],
            )?;
        }
    }
    conn.execute_batch(ddl)?;
    ensure_ledger_expires_at(conn)?;
    ensure_ledger_requeued(conn)?;
    ensure_ledger_causality_columns(conn)?;
    Ok(())
}

/// Open one read-only handle on a server database that already exists.
///
/// No pragmas and no DDL: the writer set `journal_mode=WAL` on the file and ran
/// the schema gate before any of these open, and a read-only connection cannot
/// change either. The busy timeout is the writer's, so a reader that meets a
/// checkpoint waits it out instead of failing a board refresh.
fn open_reader(path: &Path) -> StoreResult<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    Ok(conn)
}

/// Add `ledger.expires_at` to a file whose ledger lacks it.
///
/// The column landed in place, so an existing database keeps its rows and a
/// file at the current marker keeps opening.
fn ensure_ledger_expires_at(conn: &Connection) -> StoreResult<()> {
    let columns = conn
        .prepare("PRAGMA table_info(ledger)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns.is_empty() || columns.iter().any(|name| name == "expires_at") {
        return Ok(());
    }
    conn.execute("ALTER TABLE ledger ADD COLUMN expires_at TEXT", [])?;
    Ok(())
}

/// Add `ledger.requeued` to a file whose ledger lacks it.
///
/// The column landed in place, so an existing database keeps its rows and a
/// file at the current marker keeps opening.
fn ensure_ledger_requeued(conn: &Connection) -> StoreResult<()> {
    let columns = conn
        .prepare("PRAGMA table_info(ledger)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns.is_empty() || columns.iter().any(|name| name == "requeued") {
        return Ok(());
    }
    conn.execute(
        "ALTER TABLE ledger ADD COLUMN requeued INTEGER NOT NULL DEFAULT 0",
        [],
    )?;
    Ok(())
}

/// Add the five ledger columns the envelope's causality feeds to a file whose
/// ledger lacks them: `family`, `hop_budget`, `origin`, `deadline`,
/// `labels_json`.
///
/// The columns landed in place, so an existing database keeps its rows and a
/// file at the current marker keeps opening.
fn ensure_ledger_causality_columns(conn: &Connection) -> StoreResult<()> {
    let columns = conn
        .prepare("PRAGMA table_info(ledger)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns.is_empty() {
        return Ok(());
    }
    for (name, kind) in [
        ("family", "TEXT"),
        ("hop_budget", "INTEGER"),
        ("origin", "TEXT"),
        ("deadline", "TEXT"),
        ("labels_json", "TEXT"),
    ] {
        if columns.iter().any(|column| column == name) {
            continue;
        }
        conn.execute(&format!("ALTER TABLE ledger ADD COLUMN {name} {kind}"), [])?;
    }
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
        "INSERT INTO ledger(msg_id,op_id,fingerprint,kind,from_json,to_json,task,parent_task,attempt,hop,state,out_head,reason,enqueued_at,acked_at,body_json,expires_at,requeued,family,hop_budget,origin,deadline,labels_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
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

fn ledger_by_op_id(conn: &Connection, op_id: &str) -> StoreResult<Option<LedgerRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {LEDGER_COLUMNS} FROM ledger WHERE op_id=?"),
            params![op_id],
            ledger_row,
        )
        .optional()?)
}

const LEDGER_COLUMNS: &str = "msg_id,op_id,fingerprint,kind,from_json,to_json,task,parent_task,attempt,state,out_head,reason,enqueued_at,acked_at,body_json,hop,expires_at,requeued,family,hop_budget,origin,deadline,labels_json";
const FAULT_COLUMNS: &str = "id,task_id,role,session_id,generation,seq,desired_json,observed_json,intent,attempt,backend_ref,kind,reason,state,created_at";
const GHOST_SWEEP_COLUMNS: &str =
    "id,task_id,role,session_id,generation,seq_before,seq_after,outcome,evidence,swept_at";

/// The delivery one session is serving right now, as a subquery over its
/// bindings: an unbound session answers with nothing, which is the state a
/// claim reports as a session with no delivery.
///
/// No ordering: at most one binding of a session is open, which is what
/// `open_binding_conn` maintains, and an ordered subquery would have SQLite
/// spill a sorter into a temporary file on every listing read.
pub(crate) const OPEN_BINDING_TASK: &str =
    "(SELECT st.task_id FROM session_tasks st WHERE st.session_id=sessions.session_id
   AND st.released_at IS NULL LIMIT 1)";

/// One mirror row's columns, as every read of the table selects them.
///
/// `task_id` is not a column of `sessions`: it is the delivery the row is
/// labelled with — the one its session is on now, selected in the second
/// position `session_row` reads.
pub(crate) fn session_columns() -> String {
    format!(
        "session_id,{OPEN_BINDING_TASK},\
         role,generation,seq,agent_state,delivery_state,resource_state,recovery_substate,\
         desired_json,observed_json,mismatch_count,last_seen,updated_at"
    )
}

/// The session serving one task, as a subquery over the bindings.
///
/// The open binding is the session on that delivery now; the fallback to the
/// last binding taken keeps a settled delivery readable, exactly as the row it
/// used to be keyed by stayed readable.
pub(crate) const SESSION_ID_FOR_TASK: &str = "(SELECT session_id FROM session_tasks WHERE task_id=?
   ORDER BY (released_at IS NULL) DESC, bound_at DESC, session_id DESC LIMIT 1)";

/// Apply one mirror write, and bind its delivery when the write lands.
///
/// Shared by the plain write and the operator's rebind, which differ only in
/// where the row is addressed before the write.
fn project_session_conn(conn: &Connection, write: &SessionWrite) -> StoreResult<bool> {
    let changed = conn.execute(
        "INSERT INTO sessions(session_id,role,generation,seq,agent_state,delivery_state,resource_state,recovery_substate,desired_json,observed_json,mismatch_count,last_seen,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(session_id) DO UPDATE SET role=excluded.role,generation=excluded.generation,seq=excluded.seq,agent_state=excluded.agent_state,delivery_state=excluded.delivery_state,resource_state=excluded.resource_state,recovery_substate=excluded.recovery_substate,desired_json=excluded.desired_json,observed_json=excluded.observed_json,mismatch_count=excluded.mismatch_count,last_seen=excluded.last_seen,updated_at=excluded.updated_at
         WHERE excluded.generation > sessions.generation OR (excluded.generation = sessions.generation AND excluded.seq > sessions.seq)",
        params![
            write.session_id,
            write.role,
            write.generation,
            write.seq,
            write.agent_state,
            write.delivery_state,
            write.resource_state,
            write.recovery_substate,
            write.desired_json,
            write.observed_json,
            write.mismatch_count,
            unix_to_rfc3339(write.last_seen),
            unix_to_rfc3339(write.updated_at)
        ],
    )?;
    if changed == 1 {
        if let Some(task_id) = write.task_id.as_deref() {
            open_binding_conn(conn, &write.session_id, task_id, write.last_seen)?;
        }
    }
    Ok(changed == 1)
}

/// Take one delivery for a session, releasing whatever it was on before.
///
/// A session serves one delivery at a time, so this is the one place that rule
/// is enforced: the session's other open binding is released at the clock this
/// take carries. A pair already open stays as it stands — its `bound_at` is
/// when this session took that delivery, not when it was last written about —
/// and a pair whose binding was released is taken again under this clock.
pub(crate) fn open_binding_conn(
    conn: &Connection,
    session_id: &str,
    task_id: &str,
    bound_at: i64,
) -> StoreResult<usize> {
    let at = unix_to_rfc3339(bound_at);
    conn.execute(
        "UPDATE session_tasks SET released_at=? WHERE session_id=? AND released_at IS NULL AND task_id<>?",
        params![at, session_id, task_id],
    )?;
    Ok(conn.execute(
        "INSERT INTO session_tasks(session_id,task_id,bound_at,released_at) VALUES(?,?,?,NULL)
         ON CONFLICT(session_id,task_id) DO UPDATE SET bound_at=excluded.bound_at,released_at=NULL
         WHERE session_tasks.released_at IS NOT NULL",
        params![session_id, task_id, at],
    )?)
}

/// Stop serving one delivery. Only an open binding is released, so the first
/// release is the one the row keeps.
pub(crate) fn release_binding_conn(
    conn: &Connection,
    session_id: &str,
    task_id: &str,
    released_at: i64,
) -> StoreResult<usize> {
    Ok(conn.execute(
        "UPDATE session_tasks SET released_at=? WHERE session_id=? AND task_id=? AND released_at IS NULL",
        params![unix_to_rfc3339(released_at), session_id, task_id],
    )?)
}

/// The sessions listing read, as SQL. Named so the caller and the plan test run
/// one text: the order's tie key is the primary key because an explicit `rowid`
/// cannot be an index column, and an order SQLite cannot satisfy from an index
/// makes it spill a sorter to a temporary file on every read.
fn sessions_list_sql(where_sql: &str) -> String {
    format!(
        "SELECT {} FROM sessions{where_sql} ORDER BY updated_at DESC,rowid DESC LIMIT ?",
        session_columns()
    )
}

/// The ledger listing read, as SQL, on the same terms as the sessions listing.
fn ledger_list_sql(where_sql: &str) -> String {
    format!(
        "SELECT {LEDGER_COLUMNS} FROM ledger{where_sql} ORDER BY enqueued_at DESC,rowid DESC LIMIT ?"
    )
}

/// The ghost-sweep listing read, as SQL, on the same terms as the two listings
/// above. Named for the same reason: the plan test runs this one text.
fn ghost_sweeps_list_sql() -> String {
    format!(
        "SELECT {GHOST_SWEEP_COLUMNS} FROM ghost_sweeps ORDER BY swept_at DESC,rowid DESC LIMIT ?"
    )
}

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

/// One [`session_columns`] row. `task_id` is the derived binding, so it is the
/// second column rather than one of the table's own.
fn session_row(r: &Row<'_>) -> rusqlite::Result<ServerSessionRow> {
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

fn session_binding_row(r: &Row<'_>) -> rusqlite::Result<SessionBindingRow> {
    Ok(SessionBindingRow {
        session_id: r.get(0)?,
        task_id: r.get(1)?,
        bound_at: rfc3339_to_unix(&r.get::<_, String>(2)?),
        released_at: r
            .get::<_, Option<String>>(3)?
            .map(|text| rfc3339_to_unix(&text)),
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

fn ghost_sweep_row(r: &Row<'_>) -> rusqlite::Result<GhostSweepRow> {
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

fn parse_outcome(value: &str) -> Option<Outcome> {
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
