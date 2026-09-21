//! The session layer: the backend's own type and its [`SessionBackend`]
//! implementation.

use super::cli::{DEAD_STATUS, is_gone, spawn_command};
use super::policy::{TabMemo, absolute, host_worktree_env};
use super::resource::{TabKeys, ref_str};
use crate::backend::*;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct OrcaBackend {
    pub(super) runner: Arc<dyn Runner>,
    pub(super) command: String,
    pub(super) policy: WorktreePolicy,
    /// `ORCA_WORKTREE_ID` as the daemon inherited it: the worktree the
    /// supervisor's own tab lives in.
    pub(super) host_worktree: Option<String>,
    /// Spawn-time facts keyed by `pane_key`.
    pub(super) tabs: Mutex<BTreeMap<String, TabMemo>>,
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
