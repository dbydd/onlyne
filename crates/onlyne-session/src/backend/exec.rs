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
            placement: None,
            rename: None,
        }
    }

    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    /// The field shape this backend has to survive: an agent starts work of its
    /// own, and closing the session stops that work too. Signalling the leader
    /// pid alone leaves the `sleep` running under no owner — the reported case was
    /// a driver script that kept rewriting the measured surface minutes after its
    /// session was closed.
    #[cfg(unix)]
    #[test]
    fn close_stops_the_children_the_session_started() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild");
        let backend = ExecBackend::new();
        let script = format!(
            "sleep 120 & printf '%s' \"$!\" > {}; wait",
            marker.display()
        );
        let session = backend
            .spawn(spec(dir.path(), "group", vec!["sh", "-c", &script]))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild = loop {
            let reported = std::fs::read_to_string(&marker).unwrap_or_default();
            if !reported.is_empty() {
                break reported.trim().parse::<u32>().expect("a pid");
            }
            assert!(Instant::now() < deadline, "the child never reported");
            std::thread::sleep(REAP_POLL);
        };
        backend
            .close(&session, CloseReason::Operator, false)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while ExecBackend::pid_alive(grandchild) && Instant::now() < deadline {
            std::thread::sleep(REAP_POLL);
        }
        assert!(
            !ExecBackend::pid_alive(grandchild),
            "pid {grandchild} outlived the session that started it"
        );
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

    fn spec_cmd(cwd: &Path, task: &str, command: Vec<String>) -> SpawnSpec {
        SpawnSpec {
            cwd: cwd.to_path_buf(),
            task_id: task.into(),
            command,
            env: BTreeMap::new(),
            focus: None,
            placement: None,
            rename: None,
        }
    }

    fn echo_and_exit(message: &str, code: i32) -> Vec<String> {
        #[cfg(unix)]
        {
            vec![
                "sh".into(),
                "-c".into(),
                format!("printf '%s\\n' '{message}'; exit {code}"),
            ]
        }
        #[cfg(windows)]
        {
            vec![
                "cmd".into(),
                "/C".into(),
                format!("echo {message}& exit {code}"),
            ]
        }
    }

    fn sleep_cmd() -> Vec<String> {
        #[cfg(unix)]
        {
            vec!["sleep".into(), "30".into()]
        }
        #[cfg(windows)]
        {
            vec!["ping".into(), "-n".into(), "31".into(), "127.0.0.1".into()]
        }
    }

    fn wait_until_exit(backend: &ExecBackend, session: &SessionRef) -> ResourceProbe {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let probe = backend.probe(session).unwrap();
            if !probe.alive {
                return probe;
            }
            assert!(
                Instant::now() < deadline,
                "the child never exited: {probe:?}"
            );
            std::thread::sleep(REAP_POLL);
        }
    }

    #[test]
    fn a_finished_child_reports_its_exit_code_and_log_tail() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ExecBackend::new();
        let session = backend
            .spawn(spec_cmd(
                dir.path(),
                "exit-code",
                echo_and_exit("onlyne-exec-tail", 7),
            ))
            .unwrap();
        assert_eq!(session.backend, "exec");
        assert_eq!(session.backend_ref["id"], "exit-code");
        assert!(
            session
                .backend_ref
                .get("pid")
                .and_then(Value::as_u64)
                .is_some()
        );
        let log = session.backend_ref["log"].as_str().expect("log path");
        assert!(
            log.ends_with("session-exit-code.log"),
            "log path must name the session file: {log}"
        );

        let probe = wait_until_exit(&backend, &session);
        let detail = probe.detail.expect("exit detail");
        assert_eq!(detail["exit"], 7, "{detail}");
        let tail = detail["output_tail"].as_str().unwrap_or("");
        assert!(
            tail.contains("onlyne-exec-tail"),
            "output_tail must carry the child's line: {tail:?}"
        );
        let meta = std::fs::metadata(log).expect("log file");
        assert!(meta.len() > 0, "the session log must grow");
        let again = backend.probe(&session).unwrap();
        assert_eq!(again.detail.unwrap()["exit"], 7);
    }

    #[test]
    fn close_reaps_a_sleeping_child() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ExecBackend::new();
        let session = backend
            .spawn(spec_cmd(dir.path(), "sleeping", sleep_cmd()))
            .unwrap();
        let pid = session.backend_ref["pid"].as_u64().unwrap() as u32;
        assert!(backend.probe(&session).unwrap().alive);
        assert!(ExecBackend::pid_alive(pid));
        backend
            .close(&session, CloseReason::Cancelled, false)
            .unwrap();
        assert!(!backend.probe(&session).unwrap().alive);
        assert!(
            !ExecBackend::pid_alive(pid),
            "the session must leave no process behind"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_close_reaps_when_ctrl_break_has_no_console() {
        let dir = tempfile::tempdir().unwrap();
        let backend = ExecBackend::new();
        let session = backend
            .spawn(spec_cmd(dir.path(), "win-kill", sleep_cmd()))
            .unwrap();
        let pid = session.backend_ref["pid"].as_u64().unwrap() as u32;
        backend
            .close(&session, CloseReason::Shutdown, false)
            .unwrap();
        assert!(!backend.probe(&session).unwrap().alive);
        assert!(
            !ExecBackend::pid_alive(pid),
            "GenerateConsoleCtrlEvent failure must fall through to child.kill"
        );
    }

    /// A sibling whose argv contains `onlyne` must outlive session close.
    /// `kill(2)` on pid 0/`-1` or a cmdline glob would take it — and a GHA
    /// runner whose argv contains `/home/runner/work/onlyne/onlyne`.
    #[cfg(unix)]
    #[test]
    fn close_does_not_signal_an_unrelated_onlyne_named_process() {
        let mut canary = Command::new("bash")
            .args(["-c", "exec -a onlyne-canary sleep 120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("canary");
        let canary_pid = canary.id();
        let dir = tempfile::tempdir().unwrap();
        let backend = ExecBackend::new();
        let session = backend
            .spawn(spec(dir.path(), "canary-session", vec!["sleep", "30"]))
            .unwrap();
        assert_eq!(
            session.backend_ref["pgid"].as_u64().unwrap() as u32,
            session.backend_ref["pid"].as_u64().unwrap() as u32
        );
        backend
            .close(&session, CloseReason::Cancelled, false)
            .unwrap();
        assert!(
            ExecBackend::pid_alive(canary_pid),
            "pid {canary_pid} (argv onlyne-canary) must not die with the session"
        );
        let _ = canary.kill();
        let _ = canary.wait();
    }
}
