//! Gateway-local correlation table (§10.3).
//!
//! One row per external message the gateway has seen: the platform channel, the
//! conversation, the platform message id, and the scene. The table lives in the
//! gateway's own SQLite database, so cross-process traffic carries only
//! `Principal::Gateway` and the opaque handle stored in `causality.reply_to`.

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use std::{fmt, path::Path};

/// One correlation row, with the four columns named by the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRef {
    pub channel: String,
    pub conversation: String,
    pub external_id: String,
    pub scene: Option<String>,
}

impl GatewayRef {
    pub fn new(
        channel: impl Into<String>,
        conversation: impl Into<String>,
        external_id: impl Into<String>,
        scene: Option<String>,
    ) -> Self {
        Self {
            channel: channel.into(),
            conversation: conversation.into(),
            external_id: external_id.into(),
            scene,
        }
    }

    fn scene_column(&self) -> &str {
        self.scene.as_deref().unwrap_or("")
    }
}

/// A correlation table failure.
#[derive(Debug)]
pub enum RefError {
    Sql(String),
    Io(String),
    EmptyHandle,
    Open(String),
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(detail) => write!(f, "gateway ref store: {detail}"),
            Self::Io(detail) => write!(f, "gateway ref store io: {detail}"),
            Self::EmptyHandle => write!(f, "gateway ref handle must not be empty"),
            Self::Open(path) => write!(f, "cannot open gateway ref database {path}"),
        }
    }
}

impl std::error::Error for RefError {}

/// The gateway's local `gateway_ref` table.
///
/// Newest row first: a reply into a conversation threads to that
/// conversation's most recently recorded message.
pub struct GatewayRefStore {
    conn: Connection,
}

impl GatewayRefStore {
    /// Open the store at `path`, creating the file and its parent directory.
    pub fn open(path: &Path) -> Result<Self, RefError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| RefError::Io(err.to_string()))?;
        }
        let conn =
            Connection::open(path).map_err(|_| RefError::Open(path.display().to_string()))?;
        Self::prepare(conn)
    }

    /// Open a private in-memory store, used by tests and dry runs.
    pub fn open_in_memory() -> Result<Self, RefError> {
        let conn = Connection::open_in_memory().map_err(|err| RefError::Sql(err.to_string()))?;
        Self::prepare(conn)
    }

    fn prepare(conn: Connection) -> Result<Self, RefError> {
        conn.execute_batch(
            "pragma journal_mode = wal;
             pragma busy_timeout = 5000;
             create table if not exists gateway_ref (
               channel text not null,
               conversation text not null,
               external_id text not null,
               scene text not null default '',
               handle text not null,
               updated_at text not null,
               primary key (channel, conversation, external_id, scene)
             );
             create index if not exists gateway_ref_handle on gateway_ref(handle);",
        )
        .map_err(|err| RefError::Sql(err.to_string()))?;
        Ok(Self { conn })
    }

    /// Store one row against the handle the plugin encoded for it.
    ///
    /// A repeated row keeps the handle already on record, so the value carried
    /// on the wire stays identical across restarts and re-encodings.
    pub fn record(&mut self, handle: &str, row: &GatewayRef) -> Result<String, RefError> {
        if handle.trim().is_empty() {
            return Err(RefError::EmptyHandle);
        }
        if let Some(existing) = self.find(row)? {
            return Ok(existing);
        }
        self.conn
            .execute(
                "insert into gateway_ref (channel, conversation, external_id, scene, handle, updated_at)
                 values (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    row.channel,
                    row.conversation,
                    row.external_id,
                    row.scene_column(),
                    handle,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|err| RefError::Sql(err.to_string()))?;
        Ok(handle.to_string())
    }

    /// Read the row behind one handle.
    pub fn resolve(&self, handle: &str) -> Result<Option<GatewayRef>, RefError> {
        self.conn
            .query_row(
                "select channel, conversation, external_id, scene from gateway_ref
                 where handle = ?1 order by updated_at desc limit 1",
                params![handle],
                |row| {
                    let scene: String = row.get(3)?;
                    Ok(GatewayRef {
                        channel: row.get(0)?,
                        conversation: row.get(1)?,
                        external_id: row.get(2)?,
                        scene: (!scene.is_empty()).then_some(scene),
                    })
                },
            )
            .optional()
            .map_err(|err| RefError::Sql(err.to_string()))
    }

    /// The newest row recorded for one conversation.
    ///
    /// A reply carries no handle when the sender addresses the conversation
    /// alone, so the newest row for that conversation supplies the platform
    /// message id the reply threads to.
    pub fn newest_for_conversation(
        &self,
        channel: &str,
        conversation: &str,
    ) -> Result<Option<GatewayRef>, RefError> {
        self.conn
            .query_row(
                "select channel, conversation, external_id, scene from gateway_ref
                 where channel = ?1 and conversation = ?2
                 order by updated_at desc, rowid desc limit 1",
                params![channel, conversation],
                |row| {
                    let scene: String = row.get(3)?;
                    Ok(GatewayRef {
                        channel: row.get(0)?,
                        conversation: row.get(1)?,
                        external_id: row.get(2)?,
                        scene: (!scene.is_empty()).then_some(scene),
                    })
                },
            )
            .optional()
            .map_err(|err| RefError::Sql(err.to_string()))
    }

    /// Handles on record for one row identity.
    pub fn find(&self, row: &GatewayRef) -> Result<Option<String>, RefError> {
        self.conn
            .query_row(
                "select handle from gateway_ref
                 where channel = ?1 and conversation = ?2 and external_id = ?3 and scene = ?4
                 order by updated_at desc limit 1",
                params![
                    row.channel,
                    row.conversation,
                    row.external_id,
                    row.scene_column()
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| RefError::Sql(err.to_string()))
    }

    /// Number of rows on record.
    pub fn row_count(&self) -> Result<u64, RefError> {
        self.conn
            .query_row("select count(*) from gateway_ref", [], |row| row.get(0))
            .map_err(|err| RefError::Sql(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(conversation: &str, external_id: &str) -> GatewayRef {
        GatewayRef::new(
            "telegram",
            conversation,
            external_id,
            Some("private".into()),
        )
    }

    #[test]
    fn record_then_resolve_returns_the_same_row() {
        let mut store = GatewayRefStore::open_in_memory().unwrap();
        let handle = store.record("tg1.handle", &row("42", "99")).unwrap();
        assert_eq!(handle, "tg1.handle");
        let found = store.resolve("tg1.handle").unwrap().unwrap();
        assert_eq!(found, row("42", "99"));
    }

    #[test]
    fn repeated_row_keeps_the_first_handle() {
        let mut store = GatewayRefStore::open_in_memory().unwrap();
        store.record("tg1.first", &row("42", "99")).unwrap();
        let second = store.record("tg1.second", &row("42", "99")).unwrap();
        assert_eq!(second, "tg1.first");
        assert_eq!(store.row_count().unwrap(), 1);
    }

    #[test]
    fn scene_separates_two_rows_on_one_message_id() {
        let mut store = GatewayRefStore::open_in_memory().unwrap();
        store.record("qq.group", &row("7", "100")).unwrap();
        let mut reply = row("7", "100");
        reply.scene = Some("reply".into());
        store.record("qq.reply", &reply).unwrap();
        assert_eq!(store.row_count().unwrap(), 2);
        assert_eq!(store.find(&reply).unwrap().as_deref(), Some("qq.reply"));
    }

    #[test]
    fn missing_scene_matches_the_empty_column() {
        let mut store = GatewayRefStore::open_in_memory().unwrap();
        let mut bare = row("7", "100");
        bare.scene = None;
        store.record("qq.bare", &bare).unwrap();
        assert_eq!(store.find(&bare).unwrap().as_deref(), Some("qq.bare"));
        assert!(store.resolve("qq.absent").unwrap().is_none());
    }

    #[test]
    fn newest_row_wins_for_one_conversation() {
        let mut store = GatewayRefStore::open_in_memory().unwrap();
        store.record("tg1.older", &row("42", "99")).unwrap();
        store.record("tg1.newer", &row("42", "100")).unwrap();
        let newest = store
            .newest_for_conversation("telegram", "42")
            .unwrap()
            .expect("rows exist for this conversation");
        assert_eq!(newest.external_id, "100");
        assert!(
            store
                .newest_for_conversation("telegram", "absent")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn empty_handle_is_refused() {
        let mut store = GatewayRefStore::open_in_memory().unwrap();
        let err = store.record("  ", &row("42", "99")).unwrap_err();
        assert!(matches!(err, RefError::EmptyHandle));
    }

    #[test]
    fn open_creates_the_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gw").join("tg1.db");
        let store = GatewayRefStore::open(&path).unwrap();
        assert_eq!(store.row_count().unwrap(), 0);
        assert!(path.exists());
    }
}
