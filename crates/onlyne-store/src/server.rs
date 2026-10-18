//! The server store: `ServerLedger` and the shared SQL helpers behind it.
//!
//! The module is split into `conn` (connections, schema gate, read pool) and
//! `rows` (row types, mappers, timestamp/text helpers); everything those two
//! hold is re-exported here, so `crate::server`'s surface is unchanged.

mod conn;
mod rows;
#[cfg(test)]
mod tests;

pub(crate) use conn::{ReadPool, SERVER_MARKER, SERVER_SCHEMA_VERSION, open_connection, sql_limit};
pub use rows::{Append, CursorRow, EventRecord, FaultQuery, GhostSweepRow, LedgerRow, RoleRow};
pub use rows::{ServerFaultRow, ServerSessionRow, SessionBindingRow};
pub use rows::{SessionWrite, rfc3339};
pub(crate) use rows::{
    bool_int, current_ledger_state, cursor_row, ensure_transition_allowed, event_row, fault_event,
    fault_row_by_id, ghost_sweep_row, head_preview, insert_ledger_row, ledger_by_msg_id,
    ledger_by_op_id, ledger_row, ledger_state_event, rfc3339_to_unix, role_row, server_fault_row,
    session_binding_row, session_row, string_tag, unix_to_rfc3339,
};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use onlyne_proto::{
    LedgerQuery, LedgerState, Lifecycle, MsgKind, QueryFaultsArgs, QuerySessionsArgs,
};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde_json::Value;

use crate::error::{StoreError, StoreResult};
use crate::liveness::LiveSessions;

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
-- The family-open count's own reading, and the family listing's: rows of one
-- chain are few, so the index is on the family alone and the state pair is
-- read from the rows it finds.
CREATE INDEX IF NOT EXISTS ledger_family_idx ON ledger(family);
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

pub(crate) const LEDGER_COLUMNS: &str = "msg_id,op_id,fingerprint,kind,from_json,to_json,task,parent_task,attempt,state,out_head,reason,enqueued_at,acked_at,body_json,hop,expires_at,requeued,family,hop_budget,origin,deadline,labels_json";
pub(crate) const FAULT_COLUMNS: &str = "id,task_id,role,session_id,generation,seq,desired_json,observed_json,intent,attempt,backend_ref,kind,reason,state,created_at";
pub(crate) const GHOST_SWEEP_COLUMNS: &str =
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
pub(crate) fn project_session_conn(conn: &Connection, write: &SessionWrite) -> StoreResult<bool> {
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
pub(crate) fn sessions_list_sql(where_sql: &str) -> String {
    format!(
        "SELECT {} FROM sessions{where_sql} ORDER BY updated_at DESC,rowid DESC LIMIT ?",
        session_columns()
    )
}

/// The ledger listing read, as SQL, on the same terms as the sessions listing.
pub(crate) fn ledger_list_sql(where_sql: &str) -> String {
    format!(
        "SELECT {LEDGER_COLUMNS} FROM ledger{where_sql} ORDER BY enqueued_at DESC,rowid DESC LIMIT ?"
    )
}

/// The ghost-sweep listing read, as SQL, on the same terms as the two listings
/// above. Named for the same reason: the plan test runs this one text.
pub(crate) fn ghost_sweeps_list_sql() -> String {
    format!(
        "SELECT {GHOST_SWEEP_COLUMNS} FROM ghost_sweeps ORDER BY swept_at DESC,rowid DESC LIMIT ?"
    )
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

pub(crate) fn append_event_conn(conn: &Connection, kind: &str, data: &Value) -> StoreResult<i64> {
    conn.execute(
        "INSERT INTO events(type,data_json,created_at) VALUES(?,?,?)",
        params![kind, serde_json::to_string(data)?, rfc3339(Utc::now())],
    )?;
    Ok(conn.last_insert_rowid())
}

pub(crate) fn repeat_placeholders(len: usize) -> String {
    std::iter::repeat_n("?", len).collect::<Vec<_>>().join(",")
}

pub(crate) fn where_sql(clauses: &[String]) -> String {
    if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    }
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
        self.mark_acked_with_reason(msg_id, at, None)
    }

    /// Settle one row `acked`, carrying the settling word its recipient gave.
    ///
    /// An ordinary completion needs no reason — the row's own body is the
    /// delivery — but a control row is a question, and the answer the client
    /// that acted on it sends back is the reason the row reads `acked` with.
    /// `COALESCE` keeps an already-written word from being overwritten by an
    /// empty one on a repeated ack.
    pub fn mark_acked_with_reason(
        &self,
        msg_id: &str,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> StoreResult<bool> {
        self.transition_msg(
            msg_id,
            LedgerState::Acked,
            Some(rfc3339(at)),
            reason.map(str::to_string),
        )
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

    /// How much work one task family still owes: the rows of that family which
    /// have not settled.
    ///
    /// A count rather than a filtered listing, because the answer *is* the
    /// total: [`ledger_list_sql`] caps every read with `LIMIT`, so counting a
    /// page would report the page and not the family. `queued` is work accepted
    /// but not yet handed out and `in_flight` is work a client holds, which are
    /// the two states in which a family still owes something — `acked`,
    /// `rejected` and `expired` are all settled, and none of them is work.
    ///
    /// A row minted with no `family` at all — a reply, a note, a control frame;
    /// `verbs.rs` starts none of their causality chains — is not counted for any
    /// family, and asking about one returns zero.
    pub fn family_open_count(&self, family: &str) -> StoreResult<u64> {
        let conn = self.read()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ledger WHERE family=? AND state IN ('queued','in_flight')",
            params![family],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
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

    /// Close every open fault the batch filter names, and answer the ids that
    /// this call closed.
    ///
    /// The select and the update are one transaction, so the answer is exact:
    /// a row another writer closed between the two steps is not counted, and
    /// no counted row is left open. An absent `kind` and an absent `before`
    /// are refused here as at the door — the empty filter is never a sweep.
    pub fn ack_faults_batch(
        &self,
        kind: Option<&str>,
        before: Option<String>,
        limit: u32,
    ) -> StoreResult<Vec<i64>> {
        if kind.is_none() && before.is_none() {
            return Err(rusqlite::Error::InvalidParameterName(
                "repair_ack_many needs --kind or --before; refusing every fault".to_string(),
            )
            .into());
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut clauses = vec!["state='open'".to_string()];
        let mut args: Vec<SqlValue> = Vec::new();
        if let Some(kind) = kind {
            clauses.push("kind=?".to_string());
            args.push(SqlValue::Text(kind.to_string()));
        }
        if let Some(before) = before {
            clauses.push("created_at<?".to_string());
            args.push(SqlValue::Text(before));
        }
        let where_clause = where_sql(&clauses);
        args.push(SqlValue::Integer(sql_limit(limit)));
        let ids: Vec<i64> = {
            let sql = format!("SELECT id FROM faults{where_clause} ORDER BY id LIMIT ?");
            let mut stmt = tx.prepare(&sql)?;
            stmt.query_map(params_from_iter(&args), |r| r.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut closed = Vec::new();
        for id in ids {
            let n = tx.execute(
                "UPDATE faults SET state='acked' WHERE id=? AND state='open'",
                params![id],
            )?;
            if n == 1 {
                closed.push(id);
            }
        }
        tx.commit()?;
        Ok(closed)
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
