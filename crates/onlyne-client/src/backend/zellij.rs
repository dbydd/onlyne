//! The zellij backend: one zellij session per task.
//!
//! Session names are derived from the task id rather than remembered, so
//! `spawn`, `attach`, `probe` and `close` agree on the name with no state
//! carried between them — and the derivation is what fits the name inside
//! zellij's socket path budget.

use super::*;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Prefix every onlyne session name carries, so `zellij list-sessions` reads as
/// the onlyne sessions among the user's.
const SESSION_PREFIX: &str = "onlyne-";

/// How many task-id characters the name keeps after the prefix.
///
/// A uuid v4 is 32 hex digits plus four dashes, so twelve dash-free characters
/// are twelve hex digits: 48 bits of name entropy. Two tasks in one workspace
/// would have to share their first twelve id characters to collide, which is
/// negligible at workspace scale, and the truncation is what buys a 19-byte
/// name. Length is the point: zellij refuses a session whose IPC socket path
/// reaches 104 bytes on macOS (`sun_path`), the full `onlyne-<uuid>` name is 43
/// bytes, and a socket directory can spend most of the rest — 79 bytes on this
/// machine — so the untruncated name failed every spawn with zellij's report of
/// a negative character budget.
const SESSION_ID_CHARS: usize = 12;

/// Zellij's client-server contract directory, appended to the socket directory
/// (`zellij-utils/src/consts.rs`).
const CONTRACT_DIR: &str = "contract_version_1";

/// The longest session IPC socket path zellij accepts: `check_ipc_pipe_length`
/// refuses a path that reaches it (`zellij-client/src/lib.rs`), and
/// `ZELLIJ_SOCK_MAX_LENGTH` carries `sun_path`'s 104 bytes on macOS/BSD and 108
/// elsewhere.
#[cfg(target_os = "macos")]
const SOCK_PATH_LIMIT: usize = 104;
#[cfg(not(target_os = "macos"))]
const SOCK_PATH_LIMIT: usize = 108;

/// The session name for one task: the prefix plus the first
/// [`SESSION_ID_CHARS`] characters of the task id with its dashes dropped.
///
/// Pure by design. Nothing stores the name: `spawn` builds it, and `attach`,
/// `probe` and `close` rebuild it from the `SessionRef::task_id` they already
/// hold, so the name cannot disagree between the call that made a session and
/// the call that ends it.
fn short_session_name(task_id: &str) -> String {
    let id: String = task_id
        .chars()
        .filter(|c| *c != '-')
        .take(SESSION_ID_CHARS)
        .collect();
    format!("{SESSION_PREFIX}{id}")
}

/// The checked session name for one task: [`short_session_name`], refused when
/// not even that fits zellij's socket path budget.
fn session_name(task_id: &str) -> Result<String> {
    let name = short_session_name(task_id);
    check_socket_budget(&socket_dir(), &name)?;
    Ok(name)
}

/// Refuse a session name whose socket path overruns the budget zellij enforces,
/// naming the override that fixes it.
///
/// Shortening the name cannot help at this point — the socket directory itself
/// is what is over budget — so the only cure is a shorter directory, which
/// zellij reads from `ZELLIJ_SOCKET_DIR`. Left to zellij the operator instead
/// gets its report of a negative character budget, naming neither the cause nor
/// the cure.
fn check_socket_budget(dir: &Path, name: &str) -> Result<()> {
    let socket = dir.join(name);
    let path_len = socket.as_os_str().len();
    if path_len >= SOCK_PATH_LIMIT {
        anyhow::bail!(
            "zellij session {name} needs a {path_len}-byte socket path ({}), over the \
             {SOCK_PATH_LIMIT}-byte unix socket limit; set ZELLIJ_SOCKET_DIR to a shorter directory",
            socket.display()
        );
    }
    Ok(())
}

/// Zellij's session socket directory, rebuilt the way zellij computes it
/// (`zellij-utils/src/consts.rs`): `ZELLIJ_SOCKET_DIR` when set, else the
/// project runtime directory on platforms that have one, else a per-uid
/// directory under the temp dir — with the client-server contract directory
/// appended in every case.
fn socket_dir() -> PathBuf {
    let base = std::env::var("ZELLIJ_SOCKET_DIR").map_or_else(
        |_| {
            runtime_dir()
                .unwrap_or_else(|| std::env::temp_dir().join(format!("zellij-{}", temp_dir_uid())))
        },
        PathBuf::from,
    );
    base.join(CONTRACT_DIR)
}

/// The project runtime directory zellij prefers where a platform defines one.
/// `ProjectDirs::runtime_dir` is `Some` only on Linux, as
/// `$XDG_RUNTIME_DIR/zellij`.
#[cfg(target_os = "linux")]
fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join("zellij"))
}

#[cfg(not(target_os = "linux"))]
fn runtime_dir() -> Option<PathBuf> {
    None
}

/// The uid zellij stamps into its temp socket directory.
///
/// std exposes no `getuid`, and on macOS the temp dir is per-user, so its owner
/// is that uid there. Where the temp dir is shared the number can be a digit or
/// two off, which moves the budget check by the same amount; that check exists
/// for a socket directory far past the limit, where a digit cannot decide the
/// answer.
#[cfg(unix)]
fn temp_dir_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;

    std::fs::metadata(std::env::temp_dir()).map_or(0, |meta| meta.uid())
}

#[cfg(not(unix))]
fn temp_dir_uid() -> u32 {
    0
}

enum SessionListing {
    Missing,
    Exited,
    Live,
}

fn classify_session_listing(listing: &str, name: &str) -> SessionListing {
    let Some(line) = listing.lines().find(|line| listing_line_names(line, name)) else {
        return SessionListing::Missing;
    };
    if line.contains("EXITED") {
        SessionListing::Exited
    } else {
        SessionListing::Live
    }
}

fn listing_line_names(line: &str, name: &str) -> bool {
    let trimmed = line.trim();
    trimmed == name
        || trimmed.starts_with(&format!("{name} "))
        || trimmed.starts_with(&format!("{name}\t"))
        || trimmed.split_whitespace().any(|tok| tok == name)
}

fn parse_pane_token(pane: &str) -> Option<(u32, bool)> {
    let pane = pane.trim();
    if let Some(rest) = pane.strip_prefix("terminal_") {
        rest.parse().ok().map(|id| (id, false))
    } else if let Some(rest) = pane.strip_prefix("plugin_") {
        rest.parse().ok().map(|id| (id, true))
    } else {
        pane.parse().ok().map(|id| (id, false))
    }
}

fn collect_panes<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_panes(item, out);
            }
        }
        Value::Object(map) => {
            if map.contains_key("id") {
                out.push(value);
            }
            for nested in map.values() {
                collect_panes(nested, out);
            }
        }
        _ => {}
    }
}

fn probe_pane(rows: &Value, pane_ref: &str) -> ResourceProbe {
    let Some((want_id, want_plugin)) = parse_pane_token(pane_ref) else {
        return ResourceProbe {
            alive: false,
            attached: false,
            detail: Some(serde_json::json!({"reason": "pane_missing"})),
        };
    };
    let mut panes = Vec::new();
    collect_panes(rows, &mut panes);
    let Some(row) = panes.iter().copied().find(|row| {
        let id = row
            .get("id")
            .and_then(Value::as_u64)
            .map(|id| id as u32)
            .or_else(|| {
                row.get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| id.parse().ok())
            });
        let plugin = row
            .get("is_plugin")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        id == Some(want_id) && plugin == want_plugin
    }) else {
        return ResourceProbe {
            alive: false,
            attached: false,
            detail: Some(serde_json::json!({"reason": "pane_missing"})),
        };
    };
    let exited = row.get("exited").and_then(Value::as_bool).unwrap_or(false);
    let held = row.get("is_held").and_then(Value::as_bool).unwrap_or(false);
    if exited || held {
        let exit = row.get("exit_status").cloned().unwrap_or(Value::Null);
        return ResourceProbe {
            alive: false,
            attached: false,
            detail: Some(serde_json::json!({"exit": exit, "exited": true})),
        };
    }
    ResourceProbe {
        alive: true,
        attached: true,
        detail: None,
    }
}

pub struct ZellijBackend {
    runner: Arc<dyn Runner>,
    command: String,
}
impl ZellijBackend {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            command: std::env::var("ZELLIJ_COMMAND").unwrap_or_else(|_| "zellij".into()),
        }
    }

    /// Make sure the session exists before an action is addressed to it, and
    /// report whether this call created it.
    ///
    /// `zellij run` is an action sent to a *live* session: with none, zellij
    /// answers `There is no active session!`. A task's first spawn therefore has
    /// to bring the session up first. `attach --create-background` makes one
    /// detached without a TTY (the interactive `--create` requires one), and it
    /// is not idempotent — on a session that already exists it exits 1 with
    /// `Session already exists` — so the listing decides rather than the exit
    /// code, which would break the day zellij rewords its message.
    fn ensure_session(&self, name: &str) -> Result<bool> {
        if self.session_listed(name)? {
            return Ok(false);
        }
        run_checked(
            self.runner.as_ref(),
            &self.command,
            &["attach".into(), "--create-background".into(), name.into()],
            None,
            &BTreeMap::new(),
        )
        .map_err(|error| anyhow::anyhow!("zellij attach --create-background {name}: {error}"))?;
        Ok(true)
    }

    /// Whether `list-sessions --short` names a session, which is the one place
    /// "this session exists" is decided: `attach` uses it to answer whether the
    /// resource is still there, and `spawn` uses it to decide on creating one.
    fn session_listed(&self, name: &str) -> Result<bool> {
        let out = self.runner.run(
            &self.command,
            &["list-sessions".into(), "--short".into()],
            None,
            &BTreeMap::new(),
        )?;
        Ok(out.status == 0
            && String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == name))
    }

    /// `list-sessions --no-formatting` keeps the EXITED marker `--short` strips.
    fn session_listing(&self, name: &str) -> Result<SessionListing> {
        let out = self.runner.run(
            &self.command,
            &["list-sessions".into(), "--no-formatting".into()],
            None,
            &BTreeMap::new(),
        )?;
        if out.status != 0 {
            return Ok(SessionListing::Missing);
        }
        Ok(classify_session_listing(
            &String::from_utf8_lossy(&out.stdout),
            name,
        ))
    }

    fn list_panes(&self, name: &str) -> Result<Value> {
        let out = self.runner.run(
            &self.command,
            &[
                "--session".into(),
                name.into(),
                "action".into(),
                "list-panes".into(),
                "--json".into(),
                "--state".into(),
                "--command".into(),
            ],
            None,
            &BTreeMap::new(),
        )?;
        if out.status != 0 {
            anyhow::bail!(
                "zellij action list-panes {name} failed (status {})",
                out.status
            );
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|error| anyhow::anyhow!("zellij list-panes json: {error}"))
    }

    /// `kill-session` for an already-derived name.
    fn kill_session(&self, name: &str) -> Result<()> {
        run_checked(
            self.runner.as_ref(),
            &self.command,
            &["kill-session".into(), name.into()],
            None,
            &BTreeMap::new(),
        )
        .map(|_| ())
    }

    /// Undo a session this call created when the spawn then failed, so a spawn
    /// that cannot produce a usable session ref leaves nothing running behind
    /// it.
    ///
    /// Only a session created by *this* call is reclaimed: one that was already
    /// listed may have a live pane a previous session ref addresses, and a
    /// failed run is no reason to take that away. A failed cleanup is logged and
    /// not propagated, because the run's own error is what the caller must see.
    fn reclaim_created(&self, created: bool, name: &str) {
        if !created {
            return;
        }
        if let Err(error) = self.kill_session(name) {
            tracing::warn!(
                session = %name,
                %error,
                "zellij could not reclaim the session of a failed spawn"
            );
        }
    }
}
impl SessionBackend for ZellijBackend {
    fn name(&self) -> &'static str {
        "zellij"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            spawn: true,
            attach: true,
            probe: true,
            close: true,
            focus: false,
            rename: false,
        }
    }
    fn available(&self) -> Result<bool> {
        Ok(self
            .runner
            .run(
                &self.command,
                &["list-sessions".into(), "--short".into()],
                None,
                &BTreeMap::new(),
            )
            .map(|o| o.status == 0)
            .unwrap_or(false))
    }

    /// Bring the session up if this is the task's first spawn, then run the
    /// command as a pane in it.
    ///
    /// The ref records the pane the command runs in, so `probe`/`close` address
    /// the session by the name they derive and a caller can read the pane id.
    /// A spawn that cannot finish takes the session it created with it.
    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        let session = session_name(&spec.task_id)?;
        let created = self.ensure_session(&session)?;
        let mut args = vec![
            "--session".into(),
            session.clone(),
            "run".into(),
            "--cwd".into(),
            spec.cwd.to_string_lossy().into_owned(),
            "--no-focus".into(),
            "--".into(),
        ];
        args.extend(spec.command);
        let pane = match run_checked(self.runner.as_ref(), &self.command, &args, None, &spec.env) {
            Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            Err(error) => {
                self.reclaim_created(created, &session);
                return Err(error);
            }
        };
        if pane.is_empty() {
            self.reclaim_created(created, &session);
            return Err(anyhow::anyhow!("zellij run returned no pane id"));
        }
        Ok(SessionRef {
            task_id: spec.task_id,
            backend: self.name().into(),
            backend_ref: serde_json::json!({"session": session, "pane": pane}),
            generation: 1,
        })
    }
    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        let name = session_name(&session.task_id)?;
        if !self.session_listed(&name)? {
            return Err(anyhow::anyhow!("zellij session not found: {name}"));
        }
        Ok(session.clone())
    }
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        let name = session_name(&session.task_id)?;
        match self.session_listing(&name)? {
            SessionListing::Missing => Ok(ResourceProbe {
                alive: false,
                attached: false,
                detail: Some(serde_json::json!({"reason": "session_missing"})),
            }),
            SessionListing::Exited => Ok(ResourceProbe {
                alive: false,
                attached: false,
                detail: Some(serde_json::json!({"reason": "session_exited"})),
            }),
            SessionListing::Live => {
                let pane = session
                    .backend_ref
                    .get("pane")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match self.list_panes(&name) {
                    Ok(rows) => Ok(probe_pane(&rows, pane)),
                    Err(_) => Ok(ResourceProbe {
                        alive: false,
                        attached: false,
                        detail: Some(serde_json::json!({"reason": "pane_missing"})),
                    }),
                }
            }
        }
    }
    fn close(&self, session: &SessionRef, _reason: CloseReason, _force: bool) -> Result<()> {
        self.kill_session(&session_name(&session.task_id)?)
    }
}

#[cfg(test)]
mod tests;
