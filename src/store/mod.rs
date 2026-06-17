use crate::core::*;
use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

pub struct Store {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        migrate_conn(&conn)?;
        Ok(Self {
            path,
            conn: Mutex::new(conn),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn migrate(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        migrate_conn(&conn)
    }
}

fn migrate_conn(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(r#"
            create table if not exists channels (
              channel_id text primary key,
              health text not null,
              updated_at text not null
            );
            create table if not exists conversations (
              channel_id text not null,
              conversation_id text not null,
              title text,
              platform_metadata text not null default '{}',
              updated_at text not null,
              primary key(channel_id, conversation_id)
            );
            create table if not exists messages (
              channel_id text not null,
              conversation_id text not null,
              message_id text primary key,
              direction text not null,
              sender_id text,
              sender_name text,
              text text,
              attachments text not null,
              delivery_state text not null,
              timestamp text not null,
              platform_metadata text not null
            );
            create index if not exists messages_channel_conversation_time on messages(channel_id, conversation_id, timestamp);
            create index if not exists messages_time on messages(timestamp);
        "#)?;
    Ok(())
}

impl Store {
    pub async fn upsert_channel(
        &self,
        id: &ChannelId,
        health: AdapterHealth,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("insert into channels(channel_id, health, updated_at) values(?1, ?2, ?3) on conflict(channel_id) do update set health=excluded.health, updated_at=excluded.updated_at", params![id.0, serde_json::to_string(&health)?, chrono::Utc::now().to_rfc3339()])?;
        Ok(())
    }

    pub async fn list_channels(&self) -> anyhow::Result<Vec<(ChannelId, AdapterHealth)>> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("select channel_id, health from channels order by channel_id")?;
        let rows = stmt.query_map([], |row| {
            let h: String = row.get(1)?;
            Ok((
                ChannelId(row.get(0)?),
                serde_json::from_str(&h).unwrap_or(AdapterHealth::Failed),
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub async fn upsert_conversation(&self, c: &Conversation) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("insert into conversations(channel_id, conversation_id, title, platform_metadata, updated_at) values(?1, ?2, ?3, ?4, ?5) on conflict(channel_id, conversation_id) do update set title=excluded.title, platform_metadata=excluded.platform_metadata, updated_at=excluded.updated_at", params![c.channel_id.0, c.conversation_id.0, c.title, c.platform_metadata.to_string(), chrono::Utc::now().to_rfc3339()])?;
        Ok(())
    }

    pub async fn list_conversations(
        &self,
        channel_id: Option<&ChannelId>,
    ) -> anyhow::Result<Vec<Conversation>> {
        let conn = self.conn.lock().await;
        let sql = if channel_id.is_some() {
            "select channel_id, conversation_id, title, platform_metadata from conversations where channel_id=?1 order by updated_at desc"
        } else {
            "select channel_id, conversation_id, title, platform_metadata from conversations order by updated_at desc"
        };
        let mut stmt = conn.prepare(sql)?;
        let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Conversation> {
            let meta: String = row.get(3)?;
            Ok(Conversation {
                channel_id: ChannelId(row.get(0)?),
                conversation_id: ConversationId(row.get(1)?),
                title: row.get(2)?,
                platform_metadata: serde_json::from_str(&meta).unwrap_or_default(),
            })
        };
        let rows = if let Some(id) = channel_id {
            stmt.query_map(params![id.0], map)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], map)?.collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    pub async fn append_message(&self, m: &MessageEnvelope) -> anyhow::Result<()> {
        let conv = Conversation {
            channel_id: m.channel_id.clone(),
            conversation_id: m.conversation_id.clone(),
            title: None,
            platform_metadata: serde_json::json!({}),
        };
        self.upsert_conversation(&conv).await?;
        let conn = self.conn.lock().await;
        conn.execute("insert or replace into messages(channel_id, conversation_id, message_id, direction, sender_id, sender_name, text, attachments, delivery_state, timestamp, platform_metadata) values(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)", params![m.channel_id.0, m.conversation_id.0, m.message_id.0, serde_json::to_string(&m.direction)?, m.sender_id, m.sender_name, m.text, serde_json::to_string(&m.attachments)?, serde_json::to_string(&m.delivery_state)?, m.timestamp.to_rfc3339(), m.platform_metadata.to_string()])?;
        Ok(())
    }

    pub async fn fetch_history(
        &self,
        channel_id: Option<&ChannelId>,
        conversation_id: Option<&ConversationId>,
        limit: u32,
    ) -> anyhow::Result<Vec<MessageEnvelope>> {
        let conn = self.conn.lock().await;
        let limit = limit.min(500);
        let (sql, args): (&str, Vec<String>) = match (channel_id, conversation_id) {
            (Some(c), Some(v)) => (
                "select * from messages where channel_id=?1 and conversation_id=?2 order by timestamp desc limit ?3",
                vec![c.0.clone(), v.0.clone(), limit.to_string()],
            ),
            (Some(c), None) => (
                "select * from messages where channel_id=?1 order by timestamp desc limit ?2",
                vec![c.0.clone(), limit.to_string()],
            ),
            _ => (
                "select * from messages order by timestamp desc limit ?1",
                vec![limit.to_string()],
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let to_msg = |row: &rusqlite::Row<'_>| -> rusqlite::Result<MessageEnvelope> {
            let direction: String = row.get(3)?;
            let attachments: String = row.get(7)?;
            let state: String = row.get(8)?;
            let ts: String = row.get(9)?;
            let meta: String = row.get(10)?;
            Ok(MessageEnvelope {
                channel_id: ChannelId(row.get(0)?),
                conversation_id: ConversationId(row.get(1)?),
                message_id: MessageId(row.get(2)?),
                direction: serde_json::from_str(&direction).unwrap_or(Direction::Inbound),
                sender_id: row.get(4)?,
                sender_name: row.get(5)?,
                text: row.get(6)?,
                attachments: serde_json::from_str(&attachments).unwrap_or_default(),
                delivery_state: serde_json::from_str(&state).unwrap_or(DeliveryState::Failed),
                timestamp: chrono::DateTime::parse_from_rfc3339(&ts)
                    .map(|d| d.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now()),
                platform_metadata: serde_json::from_str(&meta).unwrap_or_default(),
            })
        };
        let rows = match args.len() {
            1 => stmt
                .query_map(params![args[0].parse::<u32>().unwrap_or(limit)], to_msg)?
                .collect::<Result<Vec<_>, _>>()?,
            2 => stmt
                .query_map(
                    params![args[0], args[1].parse::<u32>().unwrap_or(limit)],
                    to_msg,
                )?
                .collect::<Result<Vec<_>, _>>()?,
            _ => stmt
                .query_map(
                    params![args[0], args[1], args[2].parse::<u32>().unwrap_or(limit)],
                    to_msg,
                )?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    pub async fn find_conversation(
        &self,
        channel: &ChannelId,
        conversation: &ConversationId,
    ) -> anyhow::Result<Option<Conversation>> {
        let conn = self.conn.lock().await;
        let row = conn.query_row("select channel_id, conversation_id, title, platform_metadata from conversations where channel_id=?1 and conversation_id=?2", params![channel.0, conversation.0], |row| {
            let meta: String = row.get(3)?;
            Ok(Conversation { channel_id: ChannelId(row.get(0)?), conversation_id: ConversationId(row.get(1)?), title: row.get(2)?, platform_metadata: serde_json::from_str(&meta).unwrap_or_default() })
        }).optional().context("find conversation")?;
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn history_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("state.db")).unwrap();
        let m = MessageEnvelope {
            channel_id: ChannelId("telegram".into()),
            conversation_id: ConversationId("chat:1".into()),
            message_id: MessageId("m1".into()),
            direction: Direction::Inbound,
            sender_id: None,
            sender_name: None,
            text: Some("hi".into()),
            attachments: vec![],
            delivery_state: DeliveryState::Delivered,
            timestamp: chrono::Utc::now(),
            platform_metadata: serde_json::json!({}),
        };
        s.append_message(&m).await.unwrap();
        assert_eq!(s.fetch_history(None, None, 10).await.unwrap().len(), 1);
        assert_eq!(
            s.fetch_history(
                Some(&ChannelId("telegram".into())),
                Some(&ConversationId("chat:1".into())),
                10
            )
            .await
            .unwrap()[0]
                .text
                .as_deref(),
            Some("hi")
        );
    }
}
