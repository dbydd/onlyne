//! Headless session backend: run the role's `session_command` as a child of the
//! client.
//!
//! The other two backends own a *terminal* — zellij a pane, Orca a tab — and the
//! fake backend owns nothing at all, so none of them can serve a coding agent
//! that must be run headlessly and reached over a socket (an e2e case with no
//! window manager, a CI host, a supervisor that only needs the process). This
//! backend is that path, and it is the one place in the tree that spawns a
//! session command directly.
//!
//! Process semantics, which is what makes it different from a naive `Command::spawn`:
//!
//! * **stdin is a pipe the client holds open.** An agent in RPC mode treats EOF
//!   on stdin as "the operator left" and exits, so `Stdio::null()` (immediate
//!   EOF) or a dropped `ChildStdin` would end it the moment it starts. The
//!   child's stdin stays owned by the `Child` this backend keeps, so the write
//!   end lives until [`SessionBackend::close`] takes the child out.
//! * **stdout and stderr go to `<workspace>/.onlyne/logs/session-<task>.log`**
//!   (both streams into one append handle, the shape `onlyne client start` uses
//!   for its own log). The agent's own diagnostics are what an operator reads
//!   when a session misbehaves, so they must not disappear into the client's
//!   stdio.
//! * **the child gets its own process group** (unix), so a signal aimed at the
//!   client's group — the operator's terminal, a supervisor's `kill` — does not
//!   reach the agent behind the drain: only [`SessionBackend::close`] ends a
//!   session, which is the contract a tab or a pane has too.
//! * **close is graceful before it is lethal**: `SIGTERM`, then `SIGKILL` after
//!   the grace window, and `force` skips the grace. The child is reaped either
//!   way, so no zombie outlives its session.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::*;

/// How long `SIGTERM` gets before `SIGKILL`, inside the ten-second exit budget
/// `onlyne-client stop` allows the draining client.
const TERMINATE_GRACE: Duration = Duration::from_secs(5);
/// Poll interval while waiting for a signalled child to leave.
const REAP_POLL: Duration = Duration::from_millis(25);

#[derive(Clone, Default)]
pub struct ExecBackend {
    children: Arc<Mutex<HashMap<String, Child>>>,
}

/// The child map, with a poisoned lock reported as an error rather than a panic.
fn guard(mutex: &Mutex<HashMap<String, Child>>) -> Result<MutexGuard<'_, HashMap<String, Child>>> {
    mutex
        .lock()
        .map_err(|_| anyhow::anyhow!("exec backend child map lock poisoned"))
}

impl ExecBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `pid` still names a process this user can signal.
    ///
    /// Used for a session whose child this process no longer holds — after a
    /// client restart the session row outlives the handle, while the pid in
    /// `backend_ref` still identifies the agent.
    fn pid_alive(pid: u32) -> bool {
        Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn pid_of(session: &SessionRef) -> Result<u32> {
        session
            .backend_ref
            .get("pid")
            .and_then(Value::as_u64)
            .map(|pid| pid as u32)
            .ok_or_else(|| anyhow::anyhow!("exec session ref missing pid"))
    }

    /// `SIGTERM` then, past the grace window, `SIGKILL`.
    fn stop(child: &mut Child, force: bool) -> Result<()> {
        if let Ok(Some(_)) = child.try_wait() {
            return Ok(());
        }
        let pid = child.id();
        if !force {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let deadline = Instant::now() + TERMINATE_GRACE;
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    return Ok(());
                }
                std::thread::sleep(REAP_POLL);
            }
        }
        // The grace window elapsed (or the caller asked for force): the child is
        // killed and reaped, so the session leaves no process behind.
        let _ = child.kill();
        let _ = child.wait();
        Ok(())
    }
}

impl SessionBackend for ExecBackend {
    fn name(&self) -> &'static str {
        "exec"
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
        Ok(true)
    }
    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        let Some(program) = spec.command.first() else {
            return Err(anyhow::anyhow!(
                "exec: role has no session_command; there is nothing to run for task {}",
                spec.task_id
            ));
        };
        let logs = spec.cwd.join(".onlyne").join("logs");
        std::fs::create_dir_all(&logs)
            .map_err(|error| anyhow::anyhow!("create {}: {error}", logs.display()))?;
        let log_path = logs.join(format!("session-{}.log", spec.task_id));
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|error| anyhow::anyhow!("open {}: {error}", log_path.display()))?;
        let errors = log
            .try_clone()
            .map_err(|error| anyhow::anyhow!("clone {}: {error}", log_path.display()))?;
        let mut command = Command::new(program);
        command
            .args(&spec.command[1..])
            .current_dir(&spec.cwd)
            .envs(&spec.env)
            // An open pipe, never `null`: see the module note on EOF.
            .stdin(Stdio::piped())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(errors));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command
            .spawn()
            .map_err(|error| anyhow::anyhow!("spawn {}: {error}", spec.command.join(" ")))?;
        let pid = child.id();
        tracing::info!(task = %spec.task_id, pid, log = %log_path.display(), "exec session started");
        guard(&self.children)?.insert(spec.task_id.clone(), child);
        Ok(SessionRef {
            task_id: spec.task_id.clone(),
            backend: self.name().into(),
            backend_ref: serde_json::json!({"id": spec.task_id, "pid": pid}),
            generation: 1,
        })
    }
    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        let pid = Self::pid_of(session)?;
        if Self::pid_alive(pid) {
            Ok(session.clone())
        } else {
            anyhow::bail!("exec session {} is gone (pid {pid})", session.task_id)
        }
    }
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        let mut children = guard(&self.children)?;
        if let Some(child) = children.get_mut(&session.task_id) {
            // `try_wait` is authoritative while the handle is held: it also reaps
            // a finished child, which is what keeps `alive` from tracking a zombie.
            return match child.try_wait() {
                Ok(Some(status)) => Ok(ResourceProbe {
                    alive: false,
                    attached: false,
                    detail: Some(serde_json::json!({"exit": status.code()})),
                }),
                Ok(None) => Ok(ResourceProbe {
                    alive: true,
                    attached: true,
                    detail: Some(serde_json::json!({"pid": child.id()})),
                }),
                Err(error) => Ok(ResourceProbe {
                    alive: false,
                    attached: false,
                    detail: Some(serde_json::json!({"error": error.to_string()})),
                }),
            };
        }
        drop(children);
        let pid = Self::pid_of(session)?;
        let alive = Self::pid_alive(pid);
        Ok(ResourceProbe {
            alive,
            attached: alive,
            detail: Some(serde_json::json!({"pid": pid, "reattached": true})),
        })
    }
    fn close(&self, session: &SessionRef, reason: CloseReason, force: bool) -> Result<()> {
        let child = guard(&self.children)?.remove(&session.task_id);
        match child {
            Some(mut child) => {
                Self::stop(&mut child, force)?;
                tracing::info!(task = %session.task_id, ?reason, "exec session closed");
                Ok(())
            }
            None => {
                // No handle: the pid from the stored reference is all that is
                // left, and a signal to a pid this process did not spawn is not
                // something to guess at.
                let pid = Self::pid_of(session)?;
                if Self::pid_alive(pid) {
                    anyhow::bail!(
                        "exec session {} has no handle; pid {pid} is still alive",
                        session.task_id
                    )
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    fn spec(cwd: &Path, task: &str, command: Vec<&str>) -> SpawnSpec {
        SpawnSpec {
            cwd: cwd.to_path_buf(),
            task_id: task.into(),
            command: command.into_iter().map(str::to_string).collect(),
            env: BTreeMap::new(),
            focus: None,
            rename: None,
        }
    }

    fn log_text(spec: &SpawnSpec) -> String {
        let path = spec
            .cwd
            .join(".onlyne")
            .join("logs")
            .join(format!("session-{}.log", spec.task_id));
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// The probe blocks in `read` and writes nothing until a line arrives, so a
    /// child still alive with no `got=` line afterwards is a child whose stdin
    /// was not at EOF: `Stdio::null()` or a dropped write end would make `read`
    /// return at once, the script would print `got=` and exit.
    #[test]
    fn a_spawned_child_does_not_see_stdin_eof() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ExecBackend::new();
        let spawned = spec(
            dir.path(),
            "stdin-open",
            vec!["sh", "-c", "read line; printf 'got=%s\\n' \"$line\""],
        );
        let session = backend.spawn(spawned.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let probe = backend.probe(&session).unwrap();
        assert!(
            probe.alive,
            "the child must still be parked in read: {probe:?}"
        );
        assert!(probe.attached);
        assert_eq!(
            log_text(&spawned),
            "",
            "no line arrived, so read cannot have returned"
        );
        backend
            .close(&session, CloseReason::Completed, false)
            .unwrap();
        assert!(!backend.probe(&session).unwrap().alive);
    }

    /// The child's own output is what an operator reads, so both streams land in
    /// the workspace log rather than in the client's stdio.
    #[test]
    fn child_stdio_lands_in_the_workspace_log() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ExecBackend::new();
        let spawned = spec(
            dir.path(),
            "logs",
            vec!["sh", "-c", "echo out; echo err 1>&2"],
        );
        let session = backend.spawn(spawned.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let text = log_text(&spawned);
            if text.contains("out") && text.contains("err") {
                break;
            }
            std::thread::sleep(REAP_POLL);
        }
        let text = log_text(&spawned);
        assert!(text.contains("out"), "stdout must reach the log: {text:?}");
        assert!(text.contains("err"), "stderr must reach the log: {text:?}");
        backend
            .close(&session, CloseReason::Completed, true)
            .unwrap();
    }

    #[test]
    fn probe_follows_the_child_until_close_reaps_it() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ExecBackend::new();
        let session = backend
            .spawn(spec(dir.path(), "long", vec!["sleep", "30"]))
            .unwrap();
        let pid = session.backend_ref["pid"].as_u64().unwrap() as u32;
        assert!(backend.probe(&session).unwrap().alive);
        assert!(backend.attach(&session).is_ok());
        backend
            .close(&session, CloseReason::Cancelled, false)
            .unwrap();
        assert!(!backend.probe(&session).unwrap().alive);
        assert!(
            !ExecBackend::pid_alive(pid),
            "the session must leave no process behind"
        );
        assert!(backend.attach(&session).is_err());
        // Close is idempotent: a second pass has nothing left to signal.
        backend
            .close(&session, CloseReason::Cancelled, false)
            .unwrap();
    }

    #[test]
    fn force_close_kills_a_child_that_ignores_term() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ExecBackend::new();
        let session = backend
            .spawn(spec(
                dir.path(),
                "stubborn",
                vec!["sh", "-c", "trap '' TERM; sleep 30"],
            ))
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        backend
            .close(&session, CloseReason::Shutdown, true)
            .unwrap();
        assert!(!backend.probe(&session).unwrap().alive);
    }

    #[test]
    fn a_role_without_a_session_command_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ExecBackend::new();
        let error = backend
            .spawn(spec(dir.path(), "empty", vec![]))
            .unwrap_err();
        assert!(error.to_string().contains("no session_command"), "{error}");
    }

    #[test]
    fn an_unspawnable_command_is_an_error_rather_than_an_empty_session() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ExecBackend::new();
        let error = backend
            .spawn(spec(
                dir.path(),
                "missing",
                vec!["onlyne-there-is-no-such-binary"],
            ))
            .unwrap_err();
        assert!(error.to_string().contains("spawn"), "{error}");
    }
}
