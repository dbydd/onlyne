use super::*;
use chrono::{SecondsFormat, Utc};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// Terminal states that end a pane. Some Orca builds omit `status` entirely,
/// which is why [`OrcaBackend::probe`] also reads `exitCause`.
const DEAD_STATUS: [&str; 3] = ["exited", "closed", "dead"];

/// Orca's codes for a handle that no longer names the current PTY
/// incarnation. The pane keeps its `pane_key`, so a stale handle is
/// recoverable through [`OrcaBackend::remint`].
const STALE_HANDLE_CODES: [&str; 2] = ["terminal_handle_stale", "terminal_gone"];

/// Whether the CLI refused because the stored handle went stale.
fn is_stale(error: &anyhow::Error) -> bool {
    failure_code(error).is_some_and(|code| STALE_HANDLE_CODES.contains(&code))
}

/// Whether the CLI named a resource that no longer exists, which makes a
/// close a no-op instead of an error.
fn is_gone(error: &anyhow::Error) -> bool {
    is_stale(error) || failure_code(error) == Some("terminal_not_found")
}

/// Whether the CLI cannot resolve a worktree selector: the answer for a
/// selector Orca dropped since the tab was created.
fn is_selector_not_found(error: &anyhow::Error) -> bool {
    failure_code(error) == Some("selector_not_found")
}

/// The stable per-tab keys Orca repeats in `terminal create` responses and
/// `terminal list` rows. `pane_key` is `tabId:leafId`: it survives a runtime
/// restart, while the `term_<uuid>` handle is minted per PTY incarnation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TabKeys {
    handle: Option<String>,
    pane_key: Option<String>,
    tab_id: Option<String>,
    leaf_id: Option<String>,
    worktree_id: Option<String>,
    pty_id: Option<String>,
}

impl TabKeys {
    /// Read one CLI row. `create`/`show` nest the row under `terminal`,
    /// `list` under `terminals`, so every accepted position is tried; a key
    /// the build does not send stays `None`.
    fn read(row: &Value) -> Self {
        let text = |key: &str| {
            ["", "/terminal", "/result/terminal"]
                .iter()
                .find_map(|prefix| row.pointer(&format!("{prefix}/{key}")))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let tab_id = text("tabId");
        let leaf_id = text("leafId");
        let pane_key = text("paneKey").or_else(|| match (&tab_id, &leaf_id) {
            (Some(tab), Some(leaf)) => Some(format!("{tab}:{leaf}")),
            _ => None,
        });
        Self {
            handle: text("handle"),
            pane_key,
            tab_id,
            leaf_id,
            worktree_id: text("worktreeId"),
            pty_id: text("ptyId"),
        }
    }

    /// Read what a [`SessionRef`] already persisted.
    fn from_ref(session: &SessionRef) -> Self {
        Self {
            handle: ref_str(session, "handle"),
            pane_key: ref_str(session, "pane_key"),
            tab_id: ref_str(session, "tab_id"),
            leaf_id: ref_str(session, "leaf_id"),
            worktree_id: ref_str(session, "worktree_id"),
            pty_id: ref_str(session, "pty_id"),
        }
    }

    /// The keys worth persisting. Absent keys are omitted rather than null,
    /// so a ref written before one of them existed still reads.
    fn to_ref(&self) -> Value {
        let mut map = serde_json::Map::new();
        for (key, value) in [
            ("handle", &self.handle),
            ("pane_key", &self.pane_key),
            ("tab_id", &self.tab_id),
            ("leaf_id", &self.leaf_id),
            ("worktree_id", &self.worktree_id),
            ("pty_id", &self.pty_id),
        ] {
            if let Some(value) = value {
                map.insert(key.to_string(), Value::String(value.clone()));
            }
        }
        Value::Object(map)
    }
}

/// Read one non-empty string field from a session's `backend_ref`.
fn ref_str(session: &SessionRef, key: &str) -> Option<String> {
    session
        .backend_ref
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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
fn absolute(path: &Path) -> PathBuf {
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
struct TabMemo {
    root: PathBuf,
    role: String,
    session_id: String,
    title: String,
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
fn host_worktree_env() -> Option<String> {
    std::env::var("ORCA_WORKTREE_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub struct OrcaBackend {
    runner: Arc<dyn Runner>,
    command: String,
    policy: WorktreePolicy,
    /// `ORCA_WORKTREE_ID` as the daemon inherited it: the worktree the
    /// supervisor's own tab lives in.
    host_worktree: Option<String>,
    /// Spawn-time facts keyed by `pane_key`.
    tabs: Mutex<BTreeMap<String, TabMemo>>,
}

impl OrcaBackend {
    /// Orca backend with the client default policy: tabs land flat in the
    /// worktree the supervisor's tab lives in.
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self::with_policy(runner, WorktreePolicy::Host)
    }

    /// Orca backend with an explicit worktree policy, reading the host
    /// worktree from the process environment.
    pub fn with_policy(runner: Arc<dyn Runner>, policy: WorktreePolicy) -> Self {
        Self::with_host_worktree(runner, policy, host_worktree_env())
    }

    /// Orca backend that addresses `host_worktree` instead of the environment's
    /// `ORCA_WORKTREE_ID`. Tests inject it; production reads the inherited
    /// value through [`OrcaBackend::with_policy`].
    pub fn with_host_worktree(
        runner: Arc<dyn Runner>,
        policy: WorktreePolicy,
        host_worktree: Option<String>,
    ) -> Self {
        Self {
            runner,
            command: std::env::var("ORCA_CLI_COMMAND").unwrap_or_else(|_| "orca".into()),
            policy,
            host_worktree,
            tabs: Mutex::new(BTreeMap::new()),
        }
    }

    fn json(&self, args: Vec<String>) -> Result<Value> {
        run_json(
            self.runner.as_ref(),
            &self.command,
            &args,
            None,
            &BTreeMap::new(),
        )
    }

    fn ref_handle(session: &SessionRef) -> Result<String> {
        ref_str(session, "handle")
            .ok_or_else(|| anyhow::anyhow!("orca session ref missing string handle"))
    }

    /// The `--worktree` selector for a spawn, or `None` when the tab follows
    /// Orca's active worktree.
    ///
    /// `Host` passes the raw `ORCA_WORKTREE_ID` value — `<worktree-id>::<abs
    /// path>`, the spelling `orca`'s own CLI resolves a tab's worktree with —
    /// and degrades to `Inherit` when the daemon started outside an Orca tab.
    fn selector_for(&self) -> Option<String> {
        match &self.policy {
            WorktreePolicy::Host => self.host_worktree.clone(),
            WorktreePolicy::Inherit => None,
            WorktreePolicy::Selector(selector) => Some(selector.clone()),
        }
    }

    /// `terminal show` for one handle.
    fn show(&self, handle: &str) -> Result<Value> {
        self.json(vec![
            "terminal".into(),
            "show".into(),
            "--terminal".into(),
            handle.into(),
            "--json".into(),
        ])
    }

    /// Terminal rows of one worktree, or of every worktree when no selector
    /// is known.
    fn list(&self, selector: Option<&str>) -> Result<Vec<Value>> {
        let mut args = vec!["terminal".into(), "list".into()];
        if let Some(selector) = selector {
            args.push("--worktree".into());
            args.push(selector.into());
        }
        args.push("--json".into());
        let value = self.json(args)?;
        Ok(value
            .pointer("/terminals")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// `terminal list` rows for a remint. A selector this daemon can no longer
    /// resolve must not hide the pane, so the host-wide listing is the
    /// fallback.
    fn rows(&self, selector: Option<&str>) -> Result<Vec<Value>> {
        match self.list(selector) {
            Ok(rows) => Ok(rows),
            Err(error) if selector.is_some() && is_selector_not_found(&error) => self.list(None),
            Err(error) => Err(error),
        }
    }

    /// Re-resolve a session's handle from its stable `pane_key`.
    ///
    /// Orca's CLI has no paneKey→handle command, so the stable key round-trips
    /// through `terminal list`: rows carry the pane key Orca persisted and the
    /// handle of the pane's current PTY. Every other field of the ref is kept,
    /// including the selector, and the observed keys are merged in.
    fn remint(&self, session: &SessionRef) -> Result<SessionRef> {
        let pane_key = ref_str(session, "pane_key").ok_or_else(|| {
            anyhow::anyhow!("orca session ref has no pane_key; the handle cannot be re-resolved")
        })?;
        let selector = ref_str(session, "selector");
        let row = self
            .rows(selector.as_deref())?
            .into_iter()
            .find(|row| TabKeys::read(row).pane_key.as_deref() == Some(pane_key.as_str()))
            .ok_or_else(|| {
                CommandFailure::new(
                    format!("orca terminal list (pane {pane_key})"),
                    1,
                    Some("terminal_not_found".into()),
                    Some(format!("pane {pane_key} is not listed in any worktree")),
                )
            })?;
        let keys = TabKeys::read(&row);
        if keys.handle.is_none() {
            anyhow::bail!("orca terminal list row for pane {pane_key} has no handle");
        }
        let mut backend_ref = session.backend_ref.clone();
        if let (Some(map), Value::Object(observed)) = (backend_ref.as_object_mut(), keys.to_ref()) {
            map.extend(observed);
        }
        Ok(SessionRef {
            backend_ref,
            ..session.clone()
        })
    }

    /// `terminal close` for one handle.
    fn close_handle(&self, handle: &str) -> Result<()> {
        self.json(vec![
            "terminal".into(),
            "close".into(),
            "--terminal".into(),
            handle.into(),
            "--json".into(),
        ])
        .map(|_| ())
    }

    /// Where the tab a `terminal create` just made is, when its response named
    /// no handle.
    ///
    /// The coordinates the response did carry are an exact hook: `terminal list`
    /// repeats `paneKey`, or the `tabId:leafId` pair it is built from, beside the
    /// handle of the pane's current PTY. A response with no coordinates leaves
    /// the create-time title, which Orca's login shell replaces within seconds,
    /// so the newest row carrying that exact title wins. Position alone never
    /// picks a tab: closing one this backend cannot identify is worse than
    /// leaving it for the operator.
    fn find_created(&self, keys: &TabKeys, title: &str) -> Option<String> {
        let rows = self.rows(self.selector_for().as_deref()).ok()?;
        if let Some(wanted) = keys.pane_key.as_deref() {
            let named = rows
                .iter()
                .find(|row| TabKeys::read(row).pane_key.as_deref() == Some(wanted));
            if let Some(row) = named {
                return TabKeys::read(row).handle;
            }
        }
        rows.iter()
            .filter(|row| row.get("title").and_then(Value::as_str) == Some(title))
            .max_by_key(|row| row.get("lastOutputAt").and_then(Value::as_i64).unwrap_or(0))
            .and_then(|row| TabKeys::read(row).handle)
    }

    /// The error for a `terminal create` whose response named no handle, with
    /// the tab it made rolled back first.
    ///
    /// The tab exists by the time this runs, so it is the one way a failed spawn
    /// can leak a resource: the handle is recovered from the coordinates the
    /// response did carry and the tab is closed. A tab that cannot be located is
    /// named by those coordinates in the error instead, so an operator can
    /// finish the job, and no other tab is touched.
    fn roll_back_create(&self, keys: &TabKeys, title: &str, value: &Value) -> anyhow::Error {
        let coordinates = format!(
            "title {title:?}, tab_id {:?}, leaf_id {:?}, pane_key {:?}",
            keys.tab_id, keys.leaf_id, keys.pane_key
        );
        match self.find_created(keys, title) {
            Some(handle) => match self.close_handle(&handle) {
                Ok(()) => anyhow::anyhow!(
                    "orca terminal create returned no handle; the tab it made ({coordinates}, \
                     handle {handle}) was closed: {value}"
                ),
                Err(error) => anyhow::anyhow!(
                    "orca terminal create returned no handle and the tab it made ({coordinates}, \
                     handle {handle}) was not closed: {error}; close it by hand. create \
                     answered: {value}"
                ),
            },
            None => anyhow::anyhow!(
                "orca terminal create returned no handle and the tab it made ({coordinates}) \
                 could not be located to close; close it by hand. create answered: {value}"
            ),
        }
    }

    /// The freshest `terminal show` payload for a session, re-resolving the
    /// stored handle once when Orca answers that it went stale.
    ///
    /// A stale handle is expected after an Orca runtime restart: the pane keeps
    /// its `pane_key` while the PTY incarnation mints a new `term_<uuid>`. The
    /// returned [`SessionRef`] carries the handle the payload belongs to.
    fn current(&self, session: &SessionRef) -> Result<(SessionRef, Value)> {
        if let Some(handle) = ref_str(session, "handle") {
            match self.show(&handle) {
                Ok(value) => return Ok((session.clone(), value)),
                Err(error) if !is_stale(&error) => return Err(error),
                Err(_) => {}
            }
        }
        let refreshed = self.remint(session)?;
        let handle = ref_str(&refreshed, "handle")
            .ok_or_else(|| anyhow::anyhow!("orca remint produced no handle"))?;
        let value = self.show(&handle)?;
        self.note(&refreshed, "spawned");
        Ok((refreshed, value))
    }

    /// Record one tab state in the plugin-facing mapping cache.
    ///
    /// The file is a display cache under the role workspace, so nothing here
    /// may fail a spawn, a remint, or a close: a write error only warns.
    fn note(&self, session: &SessionRef, state: &str) {
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Build the command executed inside the Orca terminal.
///
/// Tab ownership and working directory are independent: `--worktree` decides
/// which tab list the tab joins, while this command's `cd` decides what the
/// agent sees. So the tab lives flat among the supervisor's tabs (the
/// supervisor's worktree) while the process runs in the role workspace, which
/// Orca is never told about. Environment entries travel through `env` so the
/// terminal process receives the same spawn contract as zellij and other
/// backends.
fn spawn_command(spec: &SpawnSpec) -> Result<String> {
    if spec.command.is_empty() {
        anyhow::bail!("orca spawn requires a command");
    }
    let mut command = format!("cd {} &&", shell_quote(&spec.cwd.to_string_lossy()));
    if !spec.env.is_empty() {
        command.push_str(" env");
        for (key, value) in &spec.env {
            command.push(' ');
            command.push_str(&shell_quote(&format!("{key}={value}")));
        }
    }
    for arg in &spec.command {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    Ok(command)
}

impl SessionBackend for OrcaBackend {
    fn name(&self) -> &'static str {
        "orca"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            spawn: true,
            attach: true,
            probe: true,
            close: true,
            focus: true,
            rename: true,
        }
    }
    fn available(&self) -> Result<bool> {
        Ok(self
            .json(vec!["terminal".into(), "list".into(), "--json".into()])
            .is_ok())
    }
    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        let title = spec
            .rename
            .clone()
            .unwrap_or_else(|| format!("onlyne:{}", spec.task_id));
        let selector = self.selector_for();
        let shell = spawn_command(&spec)?;
        let mut args = vec!["terminal".into(), "create".into()];
        if let Some(selector) = &selector {
            args.push("--worktree".into());
            args.push(selector.clone());
        }
        args.push("--title".into());
        args.push(title.clone());
        args.push("--command".into());
        args.push(shell);
        if spec.focus.unwrap_or(false) {
            args.push("--focus".into());
        }
        args.push("--json".into());
        let value = self.json(args)?;
        let keys = TabKeys::read(&value);
        if keys.handle.is_none() {
            // `create` made a tab but named no handle, so the session cannot be
            // addressed: left alone it would leak a tab that no session ref and
            // no tab map line records. Roll it back before giving up.
            return Err(self.roll_back_create(&keys, &title, &value));
        }
        let mut backend_ref = keys.to_ref();
        if let Some(selector) = &selector {
            backend_ref["selector"] = Value::String(selector.clone());
        }
        let session = SessionRef {
            task_id: spec.task_id.clone(),
            backend: self.name().into(),
            backend_ref,
            generation: 1,
        };
        if let Some(pane_key) = keys.pane_key.clone() {
            self.tabs.lock().insert(
                pane_key,
                TabMemo {
                    root: absolute(&spec.cwd),
                    role: spec.env.get("ONLYNE_ROLE").cloned().unwrap_or_default(),
                    session_id: spec
                        .env
                        .get("ONLYNE_SESSION_ID")
                        .cloned()
                        .unwrap_or_default(),
                    title,
                },
            );
        }
        self.note(&session, "spawned");
        Ok(session)
    }
    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        self.current(session).map(|(refreshed, _)| refreshed)
    }
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        let (session, value) = match self.current(session) {
            Ok(found) => found,
            // A stale handle whose pane_key is in no listing names a resource
            // that is gone, which is a verdict; anything else is a probe that
            // could not answer.
            Err(error) if is_gone(&error) => {
                return Ok(ResourceProbe {
                    alive: false,
                    attached: false,
                    detail: Some(serde_json::json!({"error": error.to_string()})),
                });
            }
            Err(error) => return Err(error),
        };
        let row = value.pointer("/terminal").unwrap_or(&value);
        let status = row.get("status").and_then(Value::as_str);
        let exit_cause = row.pointer("/exitCause/kind").and_then(Value::as_str);
        let connected = row.get("connected").and_then(Value::as_bool);
        let writable = row.get("writable").and_then(Value::as_bool);
        // Liveness: an explicit exit cause or a closed status ends the pane,
        // because `connected`/`writable` keep reading true for a tab the
        // operator closed. Without either, the pane counts as alive only while
        // its PTY is connected and writable; a payload with no status at all
        // is the shape some builds answer for a healthy tab.
        let ended = exit_cause.is_some() || status.is_some_and(|s| DEAD_STATUS.contains(&s));
        let alive = !ended
            && match status {
                Some(_) => true,
                None => connected.unwrap_or(false) && writable.unwrap_or(false),
            };
        Ok(ResourceProbe {
            alive,
            attached: connected.unwrap_or(alive),
            detail: Some(serde_json::json!({
                "handle": ref_str(&session, "handle"),
                "pane_key": ref_str(&session, "pane_key"),
                "status": status,
                "exit_cause": exit_cause,
                "last_output_at": row.get("lastOutputAt"),
            })),
        })
    }
    fn close(&self, session: &SessionRef, reason: CloseReason, force: bool) -> Result<()> {
        // Orca has one close path, so the reason and the force flag are
        // recorded for the log and the tab map instead of mapped onto flags.
        tracing::debug!(task = %session.task_id, ?reason, force, "closing orca terminal");
        let current = match self.current(session) {
            Ok((current, _)) => current,
            // Nothing addressable is left: closing again is a no-op, and the
            // tombstone still records the end of the mapping.
            Err(error) if is_gone(&error) => {
                self.note(session, "closed");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.close_handle(&Self::ref_handle(&current)?)?;
        self.note(&current, "closed");
        if let Some(pane_key) = ref_str(&current, "pane_key") {
            self.tabs.lock().remove(&pane_key);
        }
        Ok(())
    }
    fn rename(&self, session: &SessionRef, title: &str) -> Result<()> {
        self.json(vec![
            "terminal".into(),
            "rename".into(),
            "--terminal".into(),
            Self::ref_handle(session)?,
            "--title".into(),
            title.into(),
            "--json".into(),
        ])
        .map(|_| ())
    }
    fn focus(&self, session: &SessionRef) -> Result<()> {
        self.json(vec![
            "terminal".into(),
            "switch".into(),
            "--terminal".into(),
            Self::ref_handle(session)?,
            "--json".into(),
        ])
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scripted Orca CLI: every entry answers the first call whose argv
    /// contains its fragment and is then consumed, so a retry needs its own
    /// entry and an unscripted call panics.
    #[derive(Default)]
    struct OrcaCli {
        calls: Mutex<Vec<String>>,
        script: Mutex<Vec<(String, i32, String)>>,
    }

    impl OrcaCli {
        fn reply(self, fragment: &str, status: i32, body: String) -> Self {
            self.script
                .lock()
                .push((fragment.to_string(), status, body));
            self
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().clone()
        }
        fn called(&self, fragment: &str) -> usize {
            self.calls()
                .iter()
                .filter(|call| call.contains(fragment))
                .count()
        }
    }

    impl Runner for OrcaCli {
        fn run(
            &self,
            program: &str,
            args: &[String],
            _cwd: Option<&Path>,
            _env: &BTreeMap<String, String>,
        ) -> Result<CommandOutput> {
            let call = format!("{program} {}", args.join(" "));
            self.calls.lock().push(call.clone());
            let mut script = self.script.lock();
            let index = script
                .iter()
                .position(|(fragment, ..)| call.contains(fragment.as_str()))
                .unwrap_or_else(|| panic!("unscripted orca call: {call}"));
            let (_, status, body) = script.remove(index);
            Ok(CommandOutput {
                status,
                stdout: body.into_bytes(),
                stderr: Vec::new(),
            })
        }
    }

    /// A successful response envelope; `run_json` unwraps `result`.
    fn envelope(result: Value) -> String {
        serde_json::json!({"ok": true, "result": result}).to_string()
    }

    /// The failure shape 1.4.198 uses: exit 1, empty stderr, error body on
    /// stdout.
    fn refusal(code: &str) -> String {
        serde_json::json!({
            "ok": false,
            "error": {"code": code, "message": format!("{code} refused")}
        })
        .to_string()
    }

    /// A `terminal create` row, as the CLI nests it under `terminal`.
    fn created_row() -> Value {
        serde_json::json!({
            "terminal": {
                "handle": "term_one",
                "paneKey": "tab-1:leaf-2",
                "tabId": "tab-1",
                "leafId": "leaf-2",
                "ptyId": "inst::/tmp/ws@@ab",
                "worktreeId": "inst::/tmp/ws"
            }
        })
    }

    /// A `terminal list` row for the same pane on a newer PTY incarnation.
    fn relisted_row() -> Value {
        serde_json::json!({
            "handle": "term_two",
            "paneKey": "tab-1:leaf-2",
            "tabId": "tab-1",
            "leafId": "leaf-2",
            "ptyId": "inst2::/tmp/ws@@cd",
            "worktreeId": "inst::/tmp/ws",
            "connected": true,
            "writable": true,
            "lastOutputAt": 9
        })
    }

    fn session(reference: Value) -> SessionRef {
        SessionRef {
            task_id: "task-1".into(),
            backend: "orca".into(),
            backend_ref: reference,
            generation: 1,
        }
    }

    fn spawn_spec(cwd: &Path) -> SpawnSpec {
        let mut env = BTreeMap::new();
        env.insert("ONLYNE_SESSION_ID".into(), "session-1".into());
        env.insert("ONLYNE_TASK_ID".into(), "task-1".into());
        env.insert("ONLYNE_ROLE".into(), "planner".into());
        SpawnSpec {
            cwd: cwd.to_path_buf(),
            task_id: "task-1".into(),
            command: vec!["pi".into()],
            env,
            focus: None,
            rename: None,
        }
    }

    fn mapping_lines(root: &Path) -> Vec<Value> {
        std::fs::read_to_string(root.join(".onlyne/cache/orca-tabs.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn spawn_command_enters_workspace_exports_env_and_quotes_args() {
        let mut env = BTreeMap::new();
        env.insert("ONLYNE_TASK".into(), "task one".into());
        env.insert("QUOTED".into(), "a'b".into());
        let command = spawn_command(&SpawnSpec {
            cwd: "/tmp/work space".into(),
            task_id: "task-1".into(),
            command: vec!["pi".into(), "--model".into(), "gpt 5".into()],
            env,
            focus: None,
            rename: None,
        })
        .unwrap();
        assert_eq!(
            command,
            "cd '/tmp/work space' && env 'ONLYNE_TASK=task one' 'QUOTED=a'\\''b' 'pi' '--model' 'gpt 5'"
        );
    }

    #[test]
    fn spawn_command_rejects_an_empty_command() {
        let error = spawn_command(&SpawnSpec {
            cwd: "/tmp/work".into(),
            task_id: "task-1".into(),
            command: vec![],
            env: BTreeMap::new(),
            focus: None,
            rename: None,
        })
        .unwrap_err();
        assert!(error.to_string().contains("requires a command"));
    }

    /// The worktree id Orca exports to a tab, in its measured 1.4.198 shape:
    /// `<worktree-id>::<abs workspace path>`.
    const HOST_WORKTREE: &str = "2ea2fe23-829c-4a8f-bcac-4129eb78a164::/tmp/host-ws";

    #[test]
    fn the_config_value_selects_the_policy() {
        assert_eq!(WorktreePolicy::from_config(""), WorktreePolicy::Host);
        assert_eq!(WorktreePolicy::from_config(" host "), WorktreePolicy::Host);
        // The spelling `auto` used to mean the default, so it still does.
        assert_eq!(WorktreePolicy::from_config("auto"), WorktreePolicy::Host);
        assert_eq!(
            WorktreePolicy::from_config("inherit"),
            WorktreePolicy::Inherit
        );
        assert_eq!(
            WorktreePolicy::from_config("id:folder:abc"),
            WorktreePolicy::Selector("id:folder:abc".into())
        );
    }

    #[test]
    fn spawn_lands_in_the_host_worktree_as_a_flat_tab() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
        let backend = OrcaBackend::with_host_worktree(
            cli.clone(),
            WorktreePolicy::Host,
            Some(HOST_WORKTREE.into()),
        );
        let spawned = backend.spawn(spawn_spec(&root)).unwrap();

        assert_eq!(spawned.backend_ref["selector"], HOST_WORKTREE);
        assert_eq!(spawned.backend_ref["handle"], "term_one");
        assert_eq!(spawned.backend_ref["pane_key"], "tab-1:leaf-2");
        assert_eq!(spawned.backend_ref["worktree_id"], "inst::/tmp/ws");
        assert_eq!(spawned.generation, 1);
        // One call, nothing else: no registration and no selector probe, even
        // though the role workspace is a directory Orca has never seen.
        let shell = spawn_command(&spawn_spec(&root)).unwrap();
        assert_eq!(
            cli.calls(),
            [format!(
                "orca terminal create --worktree {HOST_WORKTREE} --title onlyne:task-1 \
                 --command {shell} --json"
            )]
        );
        assert_eq!(mapping_lines(&root)[0]["worktree_selector"], HOST_WORKTREE);
    }

    #[test]
    fn spawn_outside_an_orca_tab_lets_orca_pick_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
        let backend = OrcaBackend::with_host_worktree(cli.clone(), WorktreePolicy::Host, None);
        let spawned = backend.spawn(spawn_spec(&root)).unwrap();

        assert!(!cli.calls()[0].contains("--worktree"));
        assert!(spawned.backend_ref.get("selector").is_none());
        assert_eq!(mapping_lines(&root)[0]["worktree_selector"], "");
    }

    #[test]
    fn an_explicit_selector_overrides_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
        let backend = OrcaBackend::with_host_worktree(
            cli.clone(),
            WorktreePolicy::Selector("id:folder:abc".into()),
            Some(HOST_WORKTREE.into()),
        );
        let spawned = backend.spawn(spawn_spec(&root)).unwrap();

        assert_eq!(spawned.backend_ref["selector"], "id:folder:abc");
        assert_eq!(
            cli.called("terminal create --worktree id:folder:abc"),
            1,
            "{:?}",
            cli.calls()
        );
    }

    #[test]
    fn inherit_lets_orca_pick_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
        let backend = OrcaBackend::with_host_worktree(
            cli.clone(),
            WorktreePolicy::Inherit,
            Some(HOST_WORKTREE.into()),
        );
        let spawned = backend.spawn(spawn_spec(&root)).unwrap();

        assert!(!cli.calls()[0].contains("--worktree"));
        assert!(spawned.backend_ref.get("selector").is_none());
        assert_eq!(mapping_lines(&root)[0]["worktree_selector"], "");
    }

    #[test]
    fn a_handleless_create_closes_the_tab_it_made() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // `create` made a tab but named no handle. The pane key it did carry is
        // the hook the rollback addresses it by, through `terminal list`.
        let cli = Arc::new(
            OrcaCli::default()
                .reply(
                    "terminal create",
                    0,
                    envelope(serde_json::json!({
                        "terminal": {"tabId": "tab-1", "leafId": "leaf-2"}
                    })),
                )
                .reply(
                    "terminal list",
                    0,
                    envelope(serde_json::json!({"terminals": [relisted_row()]})),
                )
                .reply(
                    "terminal close",
                    0,
                    envelope(serde_json::json!({"closed": true})),
                ),
        );
        let backend = OrcaBackend::with_host_worktree(
            cli.clone(),
            WorktreePolicy::Host,
            Some(HOST_WORKTREE.into()),
        );

        let error = backend.spawn(spawn_spec(&root)).unwrap_err();

        assert_eq!(cli.called("terminal close --terminal term_two"), 1);
        assert!(error.to_string().contains("term_two"), "{error}");
        assert!(error.to_string().contains("tab-1:leaf-2"), "{error}");
        // The spawn never handed a session out, so the tab map has no line and
        // the rollback is the only record of the tab.
        assert!(!root.join(".onlyne/cache/orca-tabs.jsonl").exists());
    }

    #[test]
    fn a_handleless_create_falls_back_to_its_title_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // No coordinates at all, so the create-time title is the only hook left.
        // Two rows carry it, and the newest one is the tab `create` just made.
        let row = |handle: &str, last: i64| {
            serde_json::json!({
                "handle": handle,
                "paneKey": format!("tab-{handle}:leaf-1"),
                "title": "onlyne:task-1",
                "lastOutputAt": last
            })
        };
        let cli = Arc::new(
            OrcaCli::default()
                .reply(
                    "terminal create",
                    0,
                    envelope(serde_json::json!({"terminal": {}})),
                )
                .reply(
                    "terminal list",
                    0,
                    envelope(serde_json::json!({
                        "terminals": [row("term_old", 5), row("term_new", 9)]
                    })),
                )
                .reply(
                    "terminal close",
                    0,
                    envelope(serde_json::json!({"closed": true})),
                ),
        );
        let backend = OrcaBackend::with_host_worktree(
            cli.clone(),
            WorktreePolicy::Host,
            Some(HOST_WORKTREE.into()),
        );

        let error = backend.spawn(spawn_spec(&root)).unwrap_err();

        assert_eq!(cli.called("terminal close --terminal term_new"), 1);
        assert_eq!(cli.called("terminal close --terminal term_old"), 0);
        assert!(error.to_string().contains("term_new"), "{error}");
    }

    #[test]
    fn a_create_with_no_coordinates_is_reported_never_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // The response named neither a handle nor a pane, and the listing holds
        // only the operator's own tab: nothing identifies the tab `create` made,
        // so the rollback closes nothing and says so. No close call is scripted
        // and the double panics on an unscripted call, so a rollback that
        // guessed would fail this test by touching a tab it does not own.
        let cli = Arc::new(
            OrcaCli::default()
                .reply(
                    "terminal create",
                    0,
                    envelope(serde_json::json!({"terminal": {}})),
                )
                .reply(
                    "terminal list",
                    0,
                    envelope(serde_json::json!({
                        "terminals": [{
                            "handle": "term_operator",
                            "paneKey": "tab-9:leaf-9",
                            "tabId": "tab-9",
                            "leafId": "leaf-9",
                            "title": "dbydd@workstation: ~/work",
                            "lastOutputAt": 11
                        }]
                    })),
                ),
        );
        let backend = OrcaBackend::with_host_worktree(
            cli.clone(),
            WorktreePolicy::Host,
            Some(HOST_WORKTREE.into()),
        );

        let error = backend.spawn(spawn_spec(&root)).unwrap_err();

        assert_eq!(cli.called("terminal close"), 0);
        assert!(error.to_string().contains("close it by hand"), "{error}");
        // The coordinates that let an operator finish the job are in the error.
        assert!(error.to_string().contains("onlyne:task-1"), "{error}");
    }

    #[test]
    fn spawn_writes_the_tab_map_under_the_canonical_workspace() {
        // The map is a workspace file and the workspace may be reached through
        // a symlink (`/tmp` on macOS), so the line has to land under the
        // canonical root the supervisor script reads.
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let link = std::env::temp_dir().join(format!("onlyne-orca-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
        let backend = OrcaBackend::with_host_worktree(
            cli.clone(),
            WorktreePolicy::Host,
            Some(HOST_WORKTREE.into()),
        );
        backend.spawn(spawn_spec(&link)).unwrap();
        std::fs::remove_file(&link).unwrap();

        assert!(root.join(".onlyne/cache/orca-tabs.jsonl").exists());
    }

    #[test]
    fn spawn_records_the_plugin_mapping_line() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
        let backend =
            OrcaBackend::with_host_worktree(cli, WorktreePolicy::Host, Some(HOST_WORKTREE.into()));
        backend.spawn(spawn_spec(&root)).unwrap();

        let text = std::fs::read_to_string(root.join(".onlyne/cache/orca-tabs.jsonl")).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1, "{text}");
        // The field order is the contract the supervisor script folds on.
        assert!(lines[0].starts_with(r#"{"pane_key":"tab-1:leaf-2","handle":"term_one""#));
        let line = &mapping_lines(&root)[0];
        assert_eq!(line.as_object().unwrap().len(), 9);
        assert_eq!(line["task_id"], "task-1");
        assert_eq!(line["session_id"], "session-1");
        assert_eq!(line["role"], "planner");
        assert_eq!(line["worktree_selector"], HOST_WORKTREE);
        assert_eq!(line["title"], "onlyne:task-1");
        assert_eq!(line["state"], "spawned");
        assert!(line["updated_at"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn an_unwritable_mapping_cache_does_not_fail_the_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // A file where the `.onlyne` directory belongs makes the append fail.
        std::fs::write(root.join(".onlyne"), b"not a directory").unwrap();
        let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
        let backend =
            OrcaBackend::with_host_worktree(cli, WorktreePolicy::Host, Some(HOST_WORKTREE.into()));

        let spawned = backend.spawn(spawn_spec(&root)).unwrap();
        assert_eq!(spawned.backend_ref["handle"], "term_one");
    }

    #[test]
    fn probe_reads_liveness_from_status_and_exit_cause() {
        let cases = [
            (
                "a tab with no status and a live pty",
                serde_json::json!({"connected": true, "writable": true, "lastOutputAt": 7}),
                true,
            ),
            (
                "a tab the operator closed",
                serde_json::json!({
                    "connected": true,
                    "writable": true,
                    "exitCause": {"kind": "operator_close"}
                }),
                false,
            ),
            (
                "an exited status",
                serde_json::json!({"status": "exited", "connected": false, "writable": false}),
                false,
            ),
            (
                "a pty that is neither connected nor writable",
                serde_json::json!({"connected": false, "writable": false}),
                false,
            ),
        ];
        for (name, row, alive) in cases {
            let cli = Arc::new(OrcaCli::default().reply(
                "terminal show",
                0,
                envelope(serde_json::json!({"terminal": row})),
            ));
            let backend = OrcaBackend::with_policy(cli, WorktreePolicy::Inherit);
            let probe = backend
                .probe(&session(
                    serde_json::json!({"handle": "term_live", "pane_key": "tab-1:leaf-2"}),
                ))
                .unwrap();
            assert_eq!(probe.alive, alive, "{name}");
        }
    }

    #[test]
    fn availability_follows_the_listing_answer() {
        let live = Arc::new(OrcaCli::default().reply(
            "terminal list",
            0,
            envelope(serde_json::json!({"terminals": []})),
        ));
        assert!(
            OrcaBackend::with_policy(live, WorktreePolicy::Inherit)
                .available()
                .unwrap()
        );

        // A CLI that answers on stdout with a refusal is not usable, even
        // though the exit code alone used to read as ready.
        let refusing =
            Arc::new(OrcaCli::default().reply("terminal list", 1, refusal("runtime_unavailable")));
        assert!(
            !OrcaBackend::with_policy(refusing, WorktreePolicy::Inherit)
                .available()
                .unwrap()
        );
    }

    #[test]
    fn probe_detail_carries_the_fields_the_reaper_reads() {
        let cli = Arc::new(OrcaCli::default().reply(
            "terminal show",
            0,
            envelope(serde_json::json!({"terminal": {
                "status": "running",
                "connected": true,
                "writable": true,
                "lastOutputAt": 7
            }})),
        ));
        let backend = OrcaBackend::with_policy(cli, WorktreePolicy::Inherit);
        let probe = backend
            .probe(&session(serde_json::json!({"handle": "term_live"})))
            .unwrap();
        assert!(probe.alive);
        let detail = probe.detail.unwrap();
        assert_eq!(detail["handle"], "term_live");
        assert_eq!(detail["status"], "running");
        assert!(detail["exit_cause"].is_null());
        assert_eq!(detail["last_output_at"], 7);
    }

    #[test]
    fn a_stale_handle_is_reminted_through_the_listing() {
        let cli = Arc::new(
            OrcaCli::default()
                .reply(
                    "terminal show --terminal term_old",
                    1,
                    refusal("terminal_handle_stale"),
                )
                .reply(
                    "terminal list",
                    0,
                    envelope(serde_json::json!({"terminals": [relisted_row()]})),
                )
                .reply(
                    "terminal show --terminal term_two",
                    0,
                    envelope(serde_json::json!({"terminal": {
                        "handle": "term_two",
                        "connected": true,
                        "writable": true,
                        "lastOutputAt": 9
                    }})),
                ),
        );
        let backend = OrcaBackend::with_policy(cli.clone(), WorktreePolicy::Inherit);
        let stale = session(serde_json::json!({
            "handle": "term_old",
            "pane_key": "tab-1:leaf-2",
            "pty_id": "inst::/tmp/ws@@ab",
            "selector": "path:/tmp"
        }));
        let probe = backend.probe(&stale).unwrap();

        assert!(probe.alive);
        assert_eq!(probe.detail.unwrap()["handle"], "term_two");
        assert_eq!(
            cli.calls(),
            vec![
                "orca terminal show --terminal term_old --json".to_string(),
                "orca terminal list --worktree path:/tmp --json".to_string(),
                "orca terminal show --terminal term_two --json".to_string(),
            ]
        );
    }

    #[test]
    fn attach_rewrites_a_stale_ref_and_leaves_a_live_one_alone() {
        let cli = Arc::new(
            OrcaCli::default()
                .reply(
                    "terminal show --terminal term_old",
                    1,
                    refusal("terminal_handle_stale"),
                )
                .reply(
                    "terminal list",
                    0,
                    envelope(serde_json::json!({"terminals": [relisted_row()]})),
                )
                .reply(
                    "terminal show --terminal term_two",
                    0,
                    envelope(serde_json::json!({"terminal": {"handle": "term_two"}})),
                )
                .reply(
                    "terminal show --terminal term_live",
                    0,
                    envelope(serde_json::json!({"terminal": {"handle": "term_live"}})),
                ),
        );
        let backend = OrcaBackend::with_policy(cli.clone(), WorktreePolicy::Inherit);
        let stale = session(serde_json::json!({
            "handle": "term_old",
            "pane_key": "tab-1:leaf-2",
            "pty_id": "inst::/tmp/ws@@ab",
            "selector": "path:/tmp"
        }));
        let refreshed = backend.attach(&stale).unwrap();

        assert_eq!(refreshed.backend_ref["handle"], "term_two");
        assert_eq!(refreshed.backend_ref["pty_id"], "inst2::/tmp/ws@@cd");
        assert_eq!(refreshed.backend_ref["selector"], "path:/tmp");
        assert_eq!(refreshed.task_id, "task-1");
        assert_eq!(refreshed.generation, 1);

        let live = session(serde_json::json!({"handle": "term_live", "pane_key": "tab-1:leaf-2"}));
        assert_eq!(backend.attach(&live).unwrap(), live);
        assert_eq!(cli.calls().len(), 4);
    }

    #[test]
    fn a_pane_missing_from_every_listing_is_dead() {
        let cli = Arc::new(
            OrcaCli::default()
                .reply("terminal show", 1, refusal("terminal_handle_stale"))
                .reply(
                    "terminal list",
                    0,
                    envelope(serde_json::json!({"terminals": []})),
                ),
        );
        let backend = OrcaBackend::with_policy(cli.clone(), WorktreePolicy::Inherit);
        let probe = backend
            .probe(&session(serde_json::json!({
                "handle": "term_old",
                "pane_key": "tab-1:leaf-2"
            })))
            .unwrap();

        assert!(!probe.alive);
        assert!(!probe.attached);
        assert!(
            probe.detail.unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("terminal_not_found")
        );
        // The unfiltered listing is the fallback when no selector is known.
        assert_eq!(cli.calls()[1], "orca terminal list --json".to_string(),);
    }

    #[test]
    fn a_stale_ref_without_a_pane_key_cannot_be_probed() {
        let cli = Arc::new(OrcaCli::default().reply(
            "terminal show",
            1,
            refusal("terminal_handle_stale"),
        ));
        let backend = OrcaBackend::with_policy(cli, WorktreePolicy::Inherit);
        let error = backend
            .probe(&session(serde_json::json!({"handle": "term_old"})))
            .unwrap_err();
        assert!(error.to_string().contains("pane_key"));
    }

    #[test]
    fn close_remints_a_stale_handle_and_records_the_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let selector = HOST_WORKTREE;
        let cli = Arc::new(
            OrcaCli::default()
                .reply("terminal create", 0, envelope(created_row()))
                .reply(
                    "terminal show --terminal term_one",
                    1,
                    refusal("terminal_handle_stale"),
                )
                .reply(
                    "terminal list",
                    0,
                    envelope(serde_json::json!({"terminals": [relisted_row()]})),
                )
                .reply(
                    "terminal show --terminal term_two",
                    0,
                    envelope(serde_json::json!({"terminal": {"handle": "term_two"}})),
                )
                .reply(
                    "terminal close --terminal term_two",
                    0,
                    envelope(serde_json::json!({"closed": true})),
                ),
        );
        let backend = OrcaBackend::with_host_worktree(
            cli.clone(),
            WorktreePolicy::Host,
            Some(HOST_WORKTREE.into()),
        );
        let spawned = backend.spawn(spawn_spec(&root)).unwrap();
        backend
            .close(&spawned, CloseReason::Completed, false)
            .unwrap();

        assert_eq!(cli.called("terminal close --terminal term_two"), 1);
        assert_eq!(cli.called("terminal close --terminal term_one"), 0);
        // The remint records the new handle, then the close records the end.
        let lines = mapping_lines(&root);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0]["handle"], "term_one");
        assert_eq!(lines[0]["state"], "spawned");
        assert_eq!(lines[1]["handle"], "term_two");
        assert_eq!(lines[1]["state"], "spawned");
        let tombstone = lines.last().unwrap();
        assert_eq!(tombstone["state"], "closed");
        assert_eq!(tombstone["handle"], "term_two");
        assert_eq!(tombstone["pane_key"], "tab-1:leaf-2");
        assert_eq!(tombstone["worktree_selector"], selector);
        assert_eq!(tombstone["role"], "planner");
        assert_eq!(tombstone["session_id"], "session-1");
        assert_eq!(tombstone["title"], "onlyne:task-1");
    }

    #[test]
    fn closing_a_pane_that_is_already_gone_is_a_no_op() {
        let cli = Arc::new(
            OrcaCli::default()
                .reply("terminal show", 1, refusal("terminal_handle_stale"))
                .reply(
                    "terminal list",
                    0,
                    envelope(serde_json::json!({"terminals": []})),
                ),
        );
        let backend = OrcaBackend::with_policy(cli.clone(), WorktreePolicy::Inherit);
        backend
            .close(
                &session(serde_json::json!({"handle": "term_old", "pane_key": "tab-1:leaf-2"})),
                CloseReason::Operator,
                true,
            )
            .unwrap();

        assert_eq!(cli.called("terminal close"), 0);
    }
}
