//! One role's message history: the aggregate record and each branch's own.
//!
//! `docs/v2.4-REPLY-THREADS.md` §6 is the design; this module is its storage
//! half. The shapes and the reasons:
//!
//! * **`main.jsonl` is the only aggregate record.** Every channel appends to
//!   it, in arrival order, so a role's conversations read as one timeline
//!   rather than a directory to walk. It is the seam a reader starts at and the
//!   only file that always exists.
//! * **Each branch is its own file**, `branches/<branch-id>.jsonl`. A
//!   discussion that forked out carries its own thread, so reading one is
//!   reading one file instead of filtering the aggregate by a predicate.
//! * **The two are linked, not copied.** A branch file's first entry names the
//!   message it forks from (`fork_of`, present only there), and every entry
//!   carries its branch id, so a reader on either side can find the other: the
//!   aggregate names the branch files, and a branch names its origin.
//! * **Nothing here is authoritative.** These are the gateway's own record of
//!   what passed through; the platform holds the truth and the server ledger
//!   holds the delivery. A write that fails is warned about and costs nothing.
//!
//! A branch record keeps the message's `parent` (the platform id it answers)
//! alongside the ids the role and the chat use, so a reply chain can be walked
//! without consulting the platform.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The aggregate record's name inside a role's history directory.
pub const MAIN_FILE: &str = "main.jsonl";
/// The subdirectory holding one file per branch.
pub const BRANCH_DIR: &str = "branches";

/// One message as the role's history records it.
///
/// The fields are the union of what both writers know: the platform's own ids
/// (`channel`, `conversation`, `platform_message`), the role's own
/// (`chat_key`, `branch`), and the chain (`parent`). A field that does not
/// apply is absent rather than null, so a line reads as the record it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HistoryEntry {
    /// `in` or `out`.
    pub direction: String,
    /// RFC 3339 UTC, when the role recorded it.
    pub at: String,
    /// The chat key, which conversation this was on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_key: Option<String>,
    /// The platform backend's id, `telegram` / `feishu` / `qqbot` / ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The platform's conversation id inside that backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// The branch this message belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The platform message id this one answers; absent for a root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The message the fork started from, present only on a branch file's
    /// first entry: the link from a branch back to the aggregate it forked out
    /// of. Its `chat_key` and `platform_message` name the row in `main.jsonl`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_of: Option<Fork>,
    /// The platform's own id for this message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_message: Option<String>,
    /// Who sent it, on the platform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The text, both directions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Where a branch forked from, in the terms the aggregate record uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fork {
    pub chat_key: String,
    pub platform_message: String,
}

impl HistoryEntry {
    /// One inbound message, already branched by the caller.
    pub fn inbound(chat_key: &str, channel: &str, platform_message: &str) -> Self {
        Self {
            direction: "in".into(),
            at: crate::chatlog::now(),
            chat_key: Some(chat_key.into()),
            channel: Some(channel.into()),
            platform_message: Some(platform_message.into()),
            ..Self::default()
        }
    }

    /// One outbound reply, naming the message it answers.
    pub fn outbound(chat_key: &str, channel: &str, parent: Option<&str>) -> Self {
        Self {
            direction: "out".into(),
            at: crate::chatlog::now(),
            chat_key: Some(chat_key.into()),
            channel: Some(channel.into()),
            parent: parent.map(str::to_owned),
            ..Self::default()
        }
    }

    /// Attach the branch this message belongs to.
    pub fn with_branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }

    /// Mark this entry as the first of a forked branch.
    pub fn with_fork(mut self, chat_key: &str, platform_message: &str) -> Self {
        self.fork_of = Some(Fork {
            chat_key: chat_key.into(),
            platform_message: platform_message.into(),
        });
        self
    }

    /// Attach the text this entry carries, for either direction.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }
}

/// Where a role's history lives: the aggregate and the per-branch files.
#[derive(Debug, Clone)]
pub struct History {
    root: PathBuf,
}

impl History {
    /// The history directory for one role workspace.
    ///
    /// It lives under the workspace's own `.onlyne/`, beside the runtime files
    /// the gateway already keeps, and is created on first write rather than at
    /// construction: a role that never receives a message has no history.
    pub fn new(workspace: &Path) -> Self {
        Self {
            root: workspace.join(".onlyne").join("history"),
        }
    }

    /// The aggregate record's path.
    pub fn main_path(&self) -> PathBuf {
        self.root.join(MAIN_FILE)
    }

    /// One branch's file path, named by the branch id alone.
    pub fn branch_path(&self, branch: &str) -> PathBuf {
        self.root.join(BRANCH_DIR).join(format!("{branch}.jsonl"))
    }

    /// Append one entry to the aggregate record.
    pub fn append_main(&self, entry: &HistoryEntry) -> Result<()> {
        append_line(&self.main_path(), entry).context("writing the aggregate record")
    }

    /// Append one entry to its branch's own file.
    pub fn append_branch(&self, entry: &HistoryEntry) -> Result<()> {
        let Some(branch) = entry.branch.as_deref() else {
            anyhow::bail!("a branch append names no branch");
        };
        append_line(&self.branch_path(branch), entry).context("writing a branch record")
    }

    /// The last `window` entries of the aggregate, oldest first.
    ///
    /// Read backwards, because the tail is what every reader wants and `lines`
    /// is double-ended: the common case parses `window` lines rather than the
    /// whole record.
    pub fn recent(&self, window: usize) -> Vec<HistoryEntry> {
        recent(&self.main_path(), window)
    }

    /// Every entry of one branch, oldest first.
    pub fn branch(&self, branch: &str) -> Vec<HistoryEntry> {
        let path = self.branch_path(branch);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

/// The last `window` entries of a JSONL file, oldest first.
fn recent(path: &Path, window: usize) -> Vec<HistoryEntry> {
    if window == 0 {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines: Vec<&str> = text.lines().rev().take(window).collect();
    lines.reverse();
    lines
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Append one line, creating the file and its directory on first use.
fn append_line(path: &Path, entry: &HistoryEntry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut bytes = serde_json::to_vec(entry)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(())
}

/// Read one branch file's first entry's fork origin, if it names one.
///
/// The other half of the two-way link: a reader holding a branch id learns
/// which aggregate row it forked from without scanning the aggregate.
pub fn fork_of(path: &Path) -> Option<Fork> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|line| serde_json::from_str::<HistoryEntry>(line).ok())
        .and_then(|entry| entry.fork_of)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history() -> (tempfile::TempDir, History) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        (dir, History::new(&root))
    }

    #[test]
    fn one_role_keeps_every_channel_in_one_record() {
        let (_keep, history) = history();
        history
            .append_main(&HistoryEntry::inbound("k1", "telegram", "tg-1").with_text("tg said"))
            .unwrap();
        history
            .append_main(&HistoryEntry::inbound("k2", "feishu", "fs-1").with_text("fs said"))
            .unwrap();
        let rows = history.recent(10);
        assert_eq!(rows.len(), 2, "both channels land in main.jsonl, in order");
        assert_eq!(rows[0].channel.as_deref(), Some("telegram"));
        assert_eq!(rows[1].channel.as_deref(), Some("feishu"));
        assert_eq!(rows[0].text.as_deref(), Some("tg said"));
    }

    #[test]
    fn a_branch_is_its_own_file_and_its_first_row_names_the_fork() {
        let (_keep, history) = history();
        let forked = HistoryEntry::inbound("k1", "qqbot", "qq-9").with_branch("b1");
        let forked = forked.with_fork("k1", "qq-3");
        history.append_main(&forked).unwrap();
        history.append_branch(&forked).unwrap();

        let path = history.branch_path("b1");
        assert!(path.is_file());
        assert_eq!(
            fork_of(&path),
            Some(Fork {
                chat_key: "k1".into(),
                platform_message: "qq-3".into(),
            })
        );
        // Both sides carry the link: the aggregate row names its branch, the
        // branch file names where it came from.
        let rows = history.recent(10);
        assert_eq!(rows[0].branch.as_deref(), Some("b1"));
        assert_eq!(history.branch("b1").len(), 1);
    }

    #[test]
    fn a_branch_append_without_a_branch_is_refused() {
        let (_keep, history) = history();
        let nameless = HistoryEntry::outbound("k1", "telegram", None);
        let error = history.append_branch(&nameless).expect_err("no branch id");
        assert!(error.to_string().contains("names no branch"), "{error}");
    }

    #[test]
    fn the_tail_is_what_recent_returns() {
        let (_keep, history) = history();
        for i in 0..9 {
            history
                .append_main(&HistoryEntry::inbound("k", "tg", &format!("m{i}")).with_text("x"))
                .unwrap();
        }
        let tail = history.recent(4);
        assert_eq!(tail.len(), 4);
        assert_eq!(tail.last().unwrap().platform_message.as_deref(), Some("m8"));
        assert_eq!(
            tail.first().unwrap().platform_message.as_deref(),
            Some("m5")
        );
    }

    #[test]
    fn a_missing_record_reads_as_empty_rather_than_an_error() {
        let (_keep, history) = history();
        assert!(history.recent(10).is_empty());
        assert!(history.branch("never").is_empty());
        assert!(fork_of(&history.branch_path("never")).is_none());
    }
}
