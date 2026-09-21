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
//!   (both streams into one append handle, the shape an operator gives
//!   `onlyne-client run` for its own log). The agent's own diagnostics are what
//!   an operator reads when a session misbehaves, so they must not disappear
//!   into the client's stdio.
//! * **the child gets its own process group** (unix), so a signal aimed at the
//!   client's group — the operator's terminal, a supervisor's `kill` — does not
//!   reach the agent behind the drain: only [`SessionBackend::close`] ends a
//!   session, which is the contract a tab or a pane has too.
//! * **close is graceful before it is lethal**: `SIGTERM`, then `SIGKILL` after
//!   the grace window, and `force` skips the grace. The child is reaped either
//!   way, so no zombie outlives its session. The group signal is `kill(2)` on
//!   the recorded pgid only (`backend_ref.pgid`, equal to the leader pid after
//!   `process_group(0)`). Close never matches cmdline text and refuses pid 0/`-1`.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::*;
use onlyne_layout::RoleWorkspace;

/// How long `SIGTERM` gets before `SIGKILL`, inside the shutdown budget the
/// client allows its own teardown.
const TERMINATE_GRACE: Duration = Duration::from_secs(5);
/// Poll interval while waiting for a signalled child to leave.
const REAP_POLL: Duration = Duration::from_millis(25);
/// Last N lines of the session log copied into `ResourceProbe.detail` when
/// the held child is observed to have exited. Byte cap is applied first so a
/// huge log cannot land in the projection.
const OUTPUT_TAIL_LINES: usize = 200;
/// Whole-line window used when reading [`OUTPUT_TAIL_LINES`].
const OUTPUT_TAIL_BYTES: usize = 16 * 1024;

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
        #[cfg(unix)]
        {
            pid_alive_unix(pid)
        }
        #[cfg(windows)]
        {
            pid_alive_windows(pid)
        }
    }

    fn pid_of(session: &SessionRef) -> Result<u32> {
        session
            .backend_ref
            .get("pid")
            .and_then(Value::as_u64)
            .map(|pid| pid as u32)
            .ok_or_else(|| anyhow::anyhow!("exec session ref missing pid"))
    }

    /// `SIGTERM` then, past the grace window, `SIGKILL` — aimed at the group.
    ///
    /// Every session leads its own process group (`process_group(0)` at spawn),
    /// and the work an agent starts runs in that group's children: the driver
    /// script, the training process rewriting the measured surface. Signalling
    /// the leader alone stops the agent and leaves its children running under no
    /// owner, which is the one thing a recycle must not produce. A negative pid
    /// is `/bin/kill`'s spelling for a process group, and the leader's pid is the
    /// group id. When the group send finds no group, the leader pid gets the
    /// signal on its own, which is what reaps a child that outlived its work.
    fn stop(child: &mut Child, force: bool) -> Result<()> {
        if let Ok(Some(_)) = child.try_wait() {
            return Ok(());
        }
        let pid = child.id();
        if !force {
            if !signal_group(pid, "TERM") {
                signal_pid(pid, "TERM");
            }
            let deadline = Instant::now() + TERMINATE_GRACE;
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    return Ok(());
                }
                std::thread::sleep(REAP_POLL);
            }
        }
        // The grace window elapsed (or the caller asked for force): the group is
        // killed, the leader is reaped, and the session leaves no process behind.
        signal_group(pid, "KILL");
        let _ = child.kill();
        let _ = child.wait();
        Ok(())
    }
}

/// Signal one session's whole process group, answering whether it worked.
///
/// Uses `libc::kill(-pgid, sig)` so the target is the numeric group recorded
/// at spawn — never a `kill(1)` argv, and never pid 0/`-1` (those mean "this
/// process group" / "every process we can signal" and would sweep a CI runner
/// whose cmdline happens to contain `onlyne`).
#[cfg(unix)]
fn signal_group(pid: u32, signal: &str) -> bool {
    let Some(pgid) = unix_pid(pid) else {
        return false;
    };
    let Some(sig) = unix_sig(signal) else {
        return false;
    };
    send_signal(-pgid, sig)
}

/// `CTRL_BREAK` is the group signal `CREATE_NEW_PROCESS_GROUP` accepts.
/// Without a console the call fails and [`ExecBackend::stop`] falls through to
/// `child.kill()`. `KILL` is never a console event: return false so the
/// existing TerminateProcess path runs.
#[cfg(windows)]
fn signal_group(pid: u32, signal: &str) -> bool {
    if signal != "TERM" {
        return false;
    }
    generate_ctrl_break(pid)
}

/// Signal one process by pid.
#[cfg(unix)]
fn signal_pid(pid: u32, signal: &str) {
    let Some(pid) = unix_pid(pid) else {
        return;
    };
    let Some(sig) = unix_sig(signal) else {
        return;
    };
    let _ = send_signal(pid, sig);
}

/// A pid/pgid that is safe to pass to `kill(2)`. 0 means "caller's group" and
/// -1 / 1 would broadcast or punch init; none of those are a session child.
#[cfg(unix)]
fn unix_pid(pid: u32) -> Option<i32> {
    i32::try_from(pid).ok().filter(|&pid| pid > 1)
}

#[cfg(unix)]
fn unix_sig(signal: &str) -> Option<i32> {
    match signal {
        "TERM" => Some(libc::SIGTERM),
        "KILL" => Some(libc::SIGKILL),
        _ => None,
    }
}

#[cfg(unix)]
fn send_signal(pid: i32, sig: i32) -> bool {
    if pid == 0 || pid == -1 {
        return false;
    }
    unsafe { libc::kill(pid, sig) == 0 }
}

#[cfg(unix)]
fn pid_alive_unix(pid: u32) -> bool {
    let Some(pid) = unix_pid(pid) else {
        return false;
    };
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(windows)]
fn signal_pid(pid: u32, signal: &str) {
    if signal == "TERM" {
        let _ = generate_ctrl_break(pid);
    }
}

#[cfg(windows)]
fn generate_ctrl_break(pid: u32) -> bool {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    // dwProcessGroupId 0 broadcasts to every process sharing this console.
    if pid == 0 {
        return false;
    }
    unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) != 0 }
}

#[cfg(windows)]
fn pid_alive_windows(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, INVALID_HANDLE_VALUE, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        let err = GetLastError();
        CloseHandle(handle);
        if ok == 0 {
            // ERROR_ALREADY_WAITING and any other query failure: the pid is gone.
            let _ = err;
            return false;
        }
        // A collected process still has a queryable handle; its code is not STILL_ACTIVE.
        code == STILL_ACTIVE as u32
    }
}

/// Last lines of the session log, or `None` when the file is missing or
/// unreadable. A truncated byte window is snapped to a whole line so a
/// mid-line cut never becomes the first "line" of the tail.
fn read_output_tail(path: &str) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let start = data.len().saturating_sub(OUTPUT_TAIL_BYTES);
    let slice = if start == 0 {
        data.as_slice()
    } else {
        match data[start..].iter().position(|&b| b == b'\n') {
            Some(offset) => &data[start + offset + 1..],
            None => &data[start..],
        }
    };
    let text = String::from_utf8_lossy(slice);
    let lines: Vec<&str> = text.lines().collect();
    let skip = lines.len().saturating_sub(OUTPUT_TAIL_LINES);
    Some(lines[skip..].join("\n"))
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
        let layout = RoleWorkspace::resolve(&spec.cwd);
        let logs = layout.logs_dir();
        std::fs::create_dir_all(&logs)
            .map_err(|error| anyhow::anyhow!("create {}: {error}", logs.display()))?;
        let log_path = layout.session_log_path(&spec.task_id);
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
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NEW_PROCESS_GROUP: CTRL_BREAK reaches this group, CTRL_C does not.
            command.creation_flags(0x0000_0200);
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
            backend_ref: serde_json::json!({
                "id": spec.task_id,
                "pid": pid,
                "pgid": pid,
                "log": log_path.to_string_lossy(),
            }),
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
                Ok(Some(status)) => {
                    let mut detail = serde_json::json!({"exit": status.code()});
                    if let Some(path) = session.backend_ref.get("log").and_then(Value::as_str) {
                        if let Some(tail) = read_output_tail(path) {
                            detail["output_tail"] = Value::String(tail);
                        }
                    }
                    Ok(ResourceProbe {
                        alive: false,
                        attached: false,
                        detail: Some(detail),
                    })
                }
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
mod tests;
