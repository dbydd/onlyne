//! Server database connections: the schema gate, the in-place column adds,
//! and the read-only connection pool.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use onlyne_proto::text::SchemaMismatch;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::error::{StoreError, StoreResult};

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
pub(crate) const SERVER_SCHEMA_VERSION: i64 = 6;
pub(crate) const PROTOCOL_VERSION: i64 = 1;
pub(crate) const SERVER_MARKER: &str = "onlyne-server";
pub(crate) const DEFAULT_LIMIT: i64 = 100;

pub(crate) const SCHEMA_MARKER_DDL: &str = "CREATE TABLE IF NOT EXISTS schema_marker(name TEXT PRIMARY KEY, version INTEGER NOT NULL, protocol_version INTEGER NOT NULL);";

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
pub(crate) struct ReadPool {
    path: std::path::PathBuf,
    conns: Arc<Vec<Mutex<Connection>>>,
    next: Arc<AtomicUsize>,
}

impl ReadPool {
    pub(crate) fn open(path: &Path) -> StoreResult<Self> {
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
    pub(crate) fn get(&self) -> StoreResult<MutexGuard<'_, Connection>> {
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

pub(crate) fn user_tables(conn: &Connection) -> StoreResult<Vec<String>> {
    let rows = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// Rejection-path markers only: these names identify pre-v1 tables and the `swarm` prefix that the schema gate refuses (plan §10 line 370).
pub(crate) fn is_legacy_table(name: &str) -> bool {
    matches!(
        name,
        "io_cursors" | "loopback_idempotency" | "pending_replies"
    ) || name.starts_with("swarm")
}

pub(crate) fn sql_limit(limit: u32) -> i64 {
    if limit == 0 {
        DEFAULT_LIMIT
    } else {
        i64::from(limit.min(500))
    }
}
