//! Host-specific policy that stands alone: which worktree a tab lands in, and
//! the tab map the plugin-facing cache records.

use super::resource::{TabKeys, ref_str};
use super::session::OrcaBackend;
use crate::backend::*;
use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Where `terminal create` puts the new tab.
///
/// Session identity belongs to the adapter protocol: the pi plugin owns the
/// task, and Orca is only the supervisor's management port. So a session tab
/// lands in the worktree the supervisor's own tab lives in, flat beside every
/// other tab the supervisor has, and the role workspace never appears in Orca.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreePolicy {
    /// Address the worktree the supervisor's tab exported as
    /// `ORCA_WORKTREE_ID`. A daemon started outside an Orca tab has no such
    /// value and behaves like [`WorktreePolicy::Inherit`]. The client default.
    Host,
    /// Pass no selector: the tab lands in Orca's active worktree.
    Inherit,
    /// Use one Orca worktree selector verbatim (`id:<…>`, `path:<abs>`,
    /// `name:<…>`, `branch:<…>`).
    Selector(String),
}

impl WorktreePolicy {
    /// Read the `[orca] worktree` config value: `host` (also the empty value
    /// and the retired `auto` spelling), `inherit`, or a literal Orca
    /// selector.
    pub fn from_config(value: &str) -> Self {
        match value.trim() {
            "" | "auto" | "host" => Self::Host,
            "inherit" => Self::Inherit,
            selector => Self::Selector(selector.to_string()),
        }
    }
}

/// The directory a worktree selector names, when it names one: `path:<abs>`
/// or the `<worktree-id>::<abs>` shape `ORCA_WORKTREE_ID` carries.
fn selector_path(selector: &str) -> Option<PathBuf> {
    let path = selector
        .strip_prefix("path:")
        .or_else(|| selector.split_once("::").map(|(_, path)| path))?;
    let path = PathBuf::from(path);
    path.is_absolute().then_some(path)
}

/// The cwd as an absolute, symlink-free path, so a workspace reached through a
/// symlink (`/tmp` on macOS) still files its tab map under one stable root.
pub(super) fn absolute(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// What the plugin-facing map needs about one tab, remembered for the
/// daemon's lifetime: `attach` and `close` see only a [`SessionRef`], so the
/// spawn-time facts live here and degrade to empty strings after a restart.
#[derive(Debug, Clone)]
pub(super) struct TabMemo {
    pub(super) root: PathBuf,
    pub(super) role: String,
    pub(super) session_id: String,
    pub(super) title: String,
}

/// One line of `.onlyne/cache/orca-tabs.jsonl`: the tab⇄session mapping a
/// supervisor script can tail, and the display hook beside `client sessions`
/// (the board itself does not read it).
///
/// The file is append-only and never read back by this backend, because
/// `backend_ref` in `client.db` stays the authoritative record. Field order is
/// part of the contract the readers rely on.
#[derive(Debug, Clone, Serialize)]
struct TabLine<'a> {
    pane_key: &'a str,
    handle: &'a str,
    task_id: &'a str,
    session_id: &'a str,
    role: &'a str,
    worktree_selector: &'a str,
    title: &'a str,
    state: &'a str,
    updated_at: String,
}

/// Append one mapping line under `<root>/.onlyne/cache/`.
fn append_tab_line(root: &Path, line: &TabLine<'_>) -> std::io::Result<()> {
    let dir = root.join(".onlyne/cache");
    std::fs::create_dir_all(&dir)?;
    let mut text =
        serde_json::to_string(line).map_err(|error| std::io::Error::other(error.to_string()))?;
    text.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("orca-tabs.jsonl"))?
        .write_all(text.as_bytes())
}

/// `ORCA_WORKTREE_ID` as the daemon inherited it, `None` when the shell Orca
/// exported it from is not a tab.
pub(super) fn host_worktree_env() -> Option<String> {
    std::env::var("ORCA_WORKTREE_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

impl OrcaBackend {
    /// The `--worktree` selector for a spawn, or `None` when the tab follows
    /// Orca's active worktree.
    ///
    /// `Host` passes the raw `ORCA_WORKTREE_ID` value — `<worktree-id>::<abs
    /// path>`, the spelling `orca`'s own CLI resolves a tab's worktree with —
    /// and degrades to `Inherit` when the daemon started outside an Orca tab.
    pub(super) fn selector_for(&self) -> Option<String> {
        match &self.policy {
            WorktreePolicy::Host => self.host_worktree.clone(),
            WorktreePolicy::Inherit => None,
            WorktreePolicy::Selector(selector) => Some(selector.clone()),
        }
    }

    /// Record one tab state in the plugin-facing mapping cache.
    ///
    /// The file is a display cache under the role workspace, so nothing here
    /// may fail a spawn, a remint, or a close: a write error only warns.
    pub(super) fn note(&self, session: &SessionRef, state: &str) {
        let keys = TabKeys::from_ref(session);
        let (Some(pane_key), Some(handle)) = (keys.pane_key.as_deref(), keys.handle.as_deref())
        else {
            tracing::warn!(
                task = %session.task_id,
                "orca terminal has no pane key or handle; the tab map stays unwritten"
            );
            return;
        };
        let memo = self.tabs.lock().get(pane_key).cloned();
        let selector = ref_str(session, "selector");
        let root = memo
            .as_ref()
            .map(|memo| memo.root.clone())
            .or_else(|| selector.as_deref().and_then(selector_path));
        let Some(root) = root else {
            tracing::warn!(
                task = %session.task_id,
                pane_key,
                "no workspace root for the orca tab map; the line is skipped"
            );
            return;
        };
        let line = TabLine {
            pane_key,
            handle,
            task_id: &session.task_id,
            session_id: memo
                .as_ref()
                .map(|memo| memo.session_id.as_str())
                .unwrap_or_default(),
            role: memo
                .as_ref()
                .map(|memo| memo.role.as_str())
                .unwrap_or_default(),
            worktree_selector: selector.as_deref().unwrap_or_default(),
            title: memo
                .as_ref()
                .map(|memo| memo.title.as_str())
                .unwrap_or_default(),
            state,
            updated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        };
        if let Err(error) = append_tab_line(&root, &line) {
            tracing::warn!(
                error = %error,
                task = %session.task_id,
                "orca tab map line was not written"
            );
        }
    }
}
