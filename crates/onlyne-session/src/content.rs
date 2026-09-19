//! Durable cursors for the session content a backend journals.
//!
//! The event JSON remains in the task journal exactly once.  The companion
//! index stores only enough metadata to recover the role-wide sequence and read
//! those original bytes back, whoever the eventual reader is.

use anyhow::{Context, Result, anyhow};
use onlyne_layout::RoleWorkspace;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One content record after the journal append that gave it a role-wide cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct ContentRecord {
    pub seq: u64,
    pub task_id: String,
    pub session_id: Option<String>,
    pub at: String,
    /// The exact JSON value appended to the task journal.
    pub record: Value,
}

/// Client-owned edge of a backend's content stream.
///
/// The backend calls `publish` only after the journal object and its durable
/// cursor index entry have both been appended. An implementation must do bounded
/// bookkeeping only and must never block the journalling turn.
pub trait ContentSink: Send + Sync {
    fn publish(&self, record: ContentRecord);
}

#[derive(Clone, Default)]
pub(crate) struct ContentWriter {
    inner: Arc<Mutex<ContentWriterState>>,
}

#[derive(Default)]
struct ContentWriterState {
    sink: Option<Arc<dyn ContentSink>>,
    heads: BTreeMap<PathBuf, u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentIndexEntry {
    seq: u64,
    task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    at: String,
    journal: String,
    offset: u64,
    len: u64,
}

impl ContentWriter {
    pub(crate) fn set_sink(&self, sink: Arc<dyn ContentSink>) {
        self.inner.lock().sink = Some(sink);
    }

    /// Append one unchanged JSON object, index its original bytes, then notify
    /// the client. The lock is the role-wide ordering point across task turns.
    pub(crate) fn append(
        &self,
        workspace: &Path,
        task_id: &str,
        session_id: Option<&str>,
        journal: &Path,
        record: &Value,
        at: &str,
    ) -> std::io::Result<()> {
        let mut state = self.inner.lock();
        let head = match state.heads.get(workspace).copied() {
            Some(head) => head,
            None => indexed_head(workspace)?,
        };
        let seq = head
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("content sequence exhausted"))?;
        // Reserve in memory before either append. A failed record is never
        // published, and the next successful record must not reuse its number in
        // this client run.
        state.heads.insert(workspace.to_path_buf(), seq);
        let (offset, len) = append_record(journal, record)?;
        let journal_name = journal
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("journal has no UTF-8 file name: {}", journal.display()),
                )
            })?;
        let entry = ContentIndexEntry {
            seq,
            task_id: task_id.to_string(),
            session_id: session_id.map(str::to_string),
            at: at.to_string(),
            journal: journal_name.to_string(),
            offset,
            len,
        };
        append_index(workspace, &entry)?;
        if let Some(sink) = state.sink.as_ref() {
            sink.publish(ContentRecord {
                seq,
                task_id: task_id.to_string(),
                session_id: session_id.map(str::to_string),
                at: at.to_string(),
                record: record.clone(),
            });
        }
        Ok(())
    }
}

fn indexed_head(workspace: &Path) -> std::io::Result<u64> {
    let path = RoleWorkspace::resolve(workspace).content_index_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .try_fold(0, |head, line| {
            let entry: ContentIndexEntry = serde_json::from_str(line)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            Ok(head.max(entry.seq))
        })
}

fn append_record(path: &Path, record: &Value) -> std::io::Result<(u64, u64)> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let offset = file.metadata()?.len();
    file.write_all(&body)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok((offset, body.len() as u64))
}

fn append_index(workspace: &Path, entry: &ContentIndexEntry) -> std::io::Result<()> {
    let path = RoleWorkspace::resolve(workspace).content_index_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
    line.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&line)?;
    file.flush()
}

/// Read the original journal objects named by the durable cursor index.
///
/// Entries are returned in role sequence order. A corrupt index or an indexed
/// range that no longer names exactly one JSON object is an error rather than a
/// silent stream gap.
pub fn read_content_records(workspace: &Path) -> Result<Vec<ContentRecord>> {
    let index_path = RoleWorkspace::resolve(workspace).content_index_path();
    let text = match std::fs::read_to_string(&index_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", index_path.display())),
    };
    let logs = index_path
        .parent()
        .ok_or_else(|| anyhow!("content index has no parent: {}", index_path.display()))?;
    let mut records = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: ContentIndexEntry = serde_json::from_str(line).with_context(|| {
            format!(
                "decode content index {} line {}",
                index_path.display(),
                line_number + 1
            )
        })?;
        let path = safe_journal_path(logs, &entry.journal)?;
        let record = read_indexed_record(&path, entry.offset, entry.len)
            .with_context(|| format!("read content seq {} from {}", entry.seq, path.display()))?;
        records.push(ContentRecord {
            seq: entry.seq,
            task_id: entry.task_id,
            session_id: entry.session_id,
            at: entry.at,
            record,
        });
    }
    records.sort_by_key(|record| record.seq);
    for pair in records.windows(2) {
        if pair[0].seq == pair[1].seq {
            return Err(anyhow!("duplicate content sequence {}", pair[0].seq));
        }
    }
    Ok(records)
}

fn safe_journal_path(logs: &Path, journal: &str) -> Result<PathBuf> {
    let name = Path::new(journal);
    if name.components().count() != 1
        || name.file_name().and_then(|part| part.to_str()) != Some(journal)
    {
        return Err(anyhow!("invalid content journal name {journal:?}"));
    }
    Ok(logs.join(name))
}

fn read_indexed_record(path: &Path, offset: u64, len: u64) -> Result<Value> {
    let len: usize = len
        .try_into()
        .map_err(|_| anyhow!("indexed content length {len} does not fit memory"))?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)?;
    let mut newline = [0_u8; 1];
    file.read_exact(&mut newline)?;
    if newline[0] != b'\n' {
        return Err(anyhow!("indexed content is not one complete JSONL record"));
    }
    serde_json::from_slice(&bytes).context("decode indexed journal record")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[derive(Default)]
    struct Sink {
        records: Mutex<Vec<ContentRecord>>,
    }

    impl ContentSink for Sink {
        fn publish(&self, record: ContentRecord) {
            self.records.lock().push(record);
        }
    }

    #[test]
    fn role_cursor_indexes_original_records_across_tasks() {
        let dir = tempdir().unwrap();
        let sink = Arc::new(Sink::default());
        let writer = ContentWriter::default();
        writer.set_sink(sink.clone());
        let logs = dir.path().join(".onlyne/logs");
        let first = serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"text": "one"}
        });
        let second = serde_json::json!({"onlyne": {"kind": "turn", "at": "2026-09-19T00:00:01Z"}});
        writer
            .append(
                dir.path(),
                "task-a",
                Some("session-a"),
                &logs.join("session-task-a.events.jsonl"),
                &first,
                "2026-09-19T00:00:00Z",
            )
            .unwrap();
        writer
            .append(
                dir.path(),
                "task-b",
                Some("session-b"),
                &logs.join("session-task-b.events.jsonl"),
                &second,
                "2026-09-19T00:00:01Z",
            )
            .unwrap();

        let restarted_sink = Arc::new(Sink::default());
        let restarted = ContentWriter::default();
        restarted.set_sink(restarted_sink.clone());
        let third = serde_json::json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": {"text": "three"}
        });
        restarted
            .append(
                dir.path(),
                "task-a",
                Some("session-a"),
                &logs.join("session-task-a.events.jsonl"),
                &third,
                "2026-09-19T00:00:02Z",
            )
            .unwrap();

        let replay = read_content_records(dir.path()).unwrap();
        assert_eq!(
            replay.iter().map(|record| record.seq).collect::<Vec<_>>(),
            [1, 2, 3],
            "a new writer resumes the role cursor from its durable index"
        );
        assert_eq!(replay[0].record, first);
        assert_eq!(replay[1].record, second);
        assert_eq!(replay[2].record, third);
        assert_eq!(sink.records.lock().as_slice(), &replay[..2]);
        assert_eq!(restarted_sink.records.lock().as_slice(), &replay[2..]);
        let task_a = std::fs::read_to_string(logs.join("session-task-a.events.jsonl")).unwrap();
        let journal: Value = serde_json::from_str(task_a.lines().next().unwrap()).unwrap();
        assert_eq!(
            journal, first,
            "the index must not decorate the journal object"
        );
    }
}
