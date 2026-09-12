//! Process supervision for one role workspace.
//!
//! `start` spawns `onlyne-client run` in its own process group, sends the
//! child's stdout and stderr to `.onlyne/logs/client.log`, records the pid in
//! `.onlyne/run/client.pid` at `0600`, and returns once the adapter socket is
//! bound. `stop` reads that file, signals the process, and waits for it to
//! leave. `status` reports the same facts for an operator, plus whether the
//! process holds a ready server link.

use anyhow::{Context, Result, anyhow};
use onlyne_layout::{RoleWorkspace, apply_private_mode};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Byte-exact answer for every verb that needs a live client.
pub const NOT_RUNNING: &str = "onlyne: client not running";
/// Byte-exact answer for a live client whose server link is down.
pub const NOT_CONNECTED: &str = "onlyne: client not connected";
/// Signal and liveness probes go through `kill(1)`, present at this path on
/// every POSIX host.
const KILL_BIN: &str = "/bin/kill";
/// Bound on the wait for the adapter socket after spawning the child.
pub const SOCKET_WAIT_MS: u64 = 10_000;
/// Bound on the wait for a signalled process to leave.
pub const STOP_WAIT_MS: u64 = 10_000;
/// Poll interval for both bounded waits.
pub const POLL_MS: u64 = 50;
/// Event window scanned when `status` counts recorded faults.
pub const FAULT_SCAN_LIMIT: u32 = 10_000;

/// Result of one `stop` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The process accepted `SIGTERM` and left.
    Stopped(u32),
    /// No pid file exists.
    NotRunning,
    /// A pid file named a process that had already left.
    Stale(u32),
}

impl StopOutcome {
    /// Process exit code for this outcome.
    pub fn exit_code(self) -> i32 {
        if self.is_refusal() { 2 } else { 0 }
    }

    /// A refusal prints [`NOT_RUNNING`] to stderr.
    pub fn is_refusal(self) -> bool {
        matches!(self, Self::NotRunning | Self::Stale(_))
    }

    /// Operator line for a completed stop.
    pub fn line(self) -> Option<String> {
        match self {
            Self::Stopped(pid) => Some(format!("onlyne: client stopped pid {pid}")),
            Self::NotRunning | Self::Stale(_) => None,
        }
    }
}

/// Facts `status` reports for a live client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    pub pid: u32,
    pub uptime: Duration,
    pub socket: PathBuf,
    pub faults: usize,
    /// Whether the client holds a ready server link.
    pub connected: bool,
}

impl StatusReport {
    /// One operator line carrying every reported field.
    pub fn line(&self) -> String {
        format!(
            "onlyne: client running pid {} uptime {}s socket {} faults {}",
            self.pid,
            self.uptime.as_secs(),
            self.socket.display(),
            self.faults
        )
    }

    /// Process exit code for the `status` verb: zero for a client that is up
    /// and connected to its server, and the refusal code otherwise.
    pub fn exit_code(&self) -> i32 {
        if self.connected { 0 } else { 2 }
    }
}

/// One operator line for a successful `start`.
pub fn start_line(pid: u32, socket: &Path) -> String {
    format!(
        "onlyne: client started pid {pid} socket {}",
        socket.display()
    )
}

/// Pid file for a role workspace.
pub fn pid_file(workspace: &Path) -> PathBuf {
    RoleWorkspace::resolve(workspace).pid_path()
}

/// Adapter socket for a role workspace.
pub fn socket_file(workspace: &Path) -> PathBuf {
    RoleWorkspace::resolve(workspace).socket_path()
}

/// Write `pid` and apply `0600`.
pub fn write_pid_file(path: &Path, pid: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, format!("{pid}\n"))
        .with_context(|| format!("write {}", path.display()))?;
    apply_private_mode(path).map_err(|error| anyhow!(error))?;
    Ok(())
}

/// Read the recorded pid. A missing file and an unparsable file both answer
/// `None`, which every caller treats as a stale pid file.
pub fn read_pid_file(path: &Path) -> Result<Option<u32>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(text.trim().parse::<u32>().ok())
}

/// Whether `pid` names a process this user can signal.
pub fn process_alive(pid: u32) -> bool {
    Command::new(KILL_BIN)
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn signal(pid: u32, name: &str) -> Result<()> {
    let status = Command::new(KILL_BIN)
        .arg(name)
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("signal {pid} with {name}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("onlyne: signal {name} to pid {pid} failed"))
    }
}

fn remove_socket(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(anyhow!("remove {}: {error}", path.display())),
    }
}

/// Spawn the detached client and answer once the adapter socket is bound.
pub fn start(workspace: &Path) -> Result<u32> {
    let layout = RoleWorkspace::resolve(workspace);
    layout.bootstrap().context("bootstrap workspace")?;
    let pid_path = layout.pid_path();
    if let Some(pid) = read_pid_file(&pid_path)? {
        if process_alive(pid) {
            return Err(anyhow!("onlyne: client already running with pid {pid}"));
        }
    }
    let log_path = layout.log_path();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let errors = log
        .try_clone()
        .with_context(|| format!("clone {}", log_path.display()))?;
    let binary = std::env::current_exe().context("resolve the onlyne-client binary path")?;
    let mut command = Command::new(binary);
    command
        .arg("run")
        .arg("--workspace")
        .arg(layout.root())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(errors));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {}", layout.root().display()))?;
    let pid = child.id();
    write_pid_file(&pid_path, pid)?;
    let socket = layout.socket_path();
    if let Err(error) = wait_for_socket(&socket, &mut child) {
        let _ = child.kill();
        let _ = remove_socket(&socket);
        let _ = std::fs::remove_file(&pid_path);
        return Err(error.context(format!("client log at {}", log_path.display())));
    }
    Ok(pid)
}

fn wait_for_socket(path: &Path, child: &mut Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(SOCKET_WAIT_MS);
    loop {
        if path.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait().context("poll the client child")? {
            return Err(anyhow!(
                "onlyne: client exited with {status} before binding {}",
                path.display()
            ));
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "onlyne: client did not bind {} within {SOCKET_WAIT_MS}ms",
                path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

/// Signal the recorded process and wait for it to leave.
pub fn stop(workspace: &Path) -> Result<StopOutcome> {
    let layout = RoleWorkspace::resolve(workspace);
    let pid_path = layout.pid_path();
    let Some(pid) = read_pid_file(&pid_path)? else {
        return Ok(StopOutcome::NotRunning);
    };
    if !process_alive(pid) {
        std::fs::remove_file(&pid_path)
            .with_context(|| format!("remove {}", pid_path.display()))?;
        remove_socket(&layout.socket_path())?;
        return Ok(StopOutcome::Stale(pid));
    }
    signal(pid, "-TERM")?;
    let deadline = Instant::now() + Duration::from_millis(STOP_WAIT_MS);
    while process_alive(pid) {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "onlyne: client {pid} did not stop within {STOP_WAIT_MS}ms"
            ));
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
    std::fs::remove_file(&pid_path).with_context(|| format!("remove {}", pid_path.display()))?;
    remove_socket(&layout.socket_path())?;
    Ok(StopOutcome::Stopped(pid))
}

/// Report a live client and the state of its server link, and `None` when no
/// recorded process is running.
///
/// The link state comes from the client itself: an `admin` `hello` over its
/// adapter socket, which answers with the connection its runtime holds. A
/// running process that answers nothing is a client that is not serving, and
/// the verb reports that as a refusal.
pub async fn status(workspace: &Path) -> Result<Option<StatusReport>> {
    let layout = RoleWorkspace::resolve(workspace);
    let pid_path = layout.pid_path();
    let Some(pid) = read_pid_file(&pid_path)? else {
        return Ok(None);
    };
    if !process_alive(pid) {
        return Ok(None);
    }
    let uptime = std::fs::metadata(&pid_path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .unwrap_or_default();
    let socket = layout.socket_path();
    Ok(Some(StatusReport {
        pid,
        uptime,
        faults: fault_count(&layout)?,
        connected: crate::adapter_socket::server_link_up(&socket).await,
        socket,
    }))
}

/// Count the `session_fault` events recorded in the client database.
pub fn fault_count(layout: &RoleWorkspace) -> Result<usize> {
    let path = layout.client_db_path();
    if !path.exists() {
        return Ok(0);
    }
    let store = onlyne_store::ClientStore::open(&path)?;
    let events = store.events_since(0, FAULT_SCAN_LIMIT)?;
    Ok(events
        .iter()
        .filter(|event| event.kind == "session_fault")
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const ABSENT_PID: u32 = 999_999_999;

    #[test]
    fn pid_file_round_trips_at_0600() {
        let dir = tempdir().unwrap();
        let path = pid_file(dir.path());
        assert_eq!(read_pid_file(&path).unwrap(), None);
        write_pid_file(&path, std::process::id()).unwrap();
        assert_eq!(read_pid_file(&path).unwrap(), Some(std::process::id()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn unparsable_pid_file_reads_as_stale() {
        let dir = tempdir().unwrap();
        let path = pid_file(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not-a-pid\n").unwrap();
        assert_eq!(read_pid_file(&path).unwrap(), None);
    }

    #[test]
    fn stop_without_pid_file_is_a_refusal() {
        let dir = tempdir().unwrap();
        let outcome = stop(dir.path()).unwrap();
        assert_eq!(outcome, StopOutcome::NotRunning);
        assert!(outcome.is_refusal());
        assert_eq!(outcome.exit_code(), 2);
        assert_eq!(outcome.line(), None);
    }

    #[test]
    fn stale_pid_file_is_removed_and_reported() {
        let dir = tempdir().unwrap();
        let path = pid_file(dir.path());
        write_pid_file(&path, ABSENT_PID).unwrap();
        let outcome = stop(dir.path()).unwrap();
        assert_eq!(outcome, StopOutcome::Stale(ABSENT_PID));
        assert_eq!(outcome.exit_code(), 2);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn running_status_reports_pid_socket_and_faults() {
        let dir = tempdir().unwrap();
        let layout = RoleWorkspace::resolve(dir.path());
        layout.bootstrap().unwrap();
        write_pid_file(&layout.pid_path(), std::process::id()).unwrap();
        let report = status(dir.path())
            .await
            .unwrap()
            .expect("live pid is running");
        assert_eq!(report.pid, std::process::id());
        assert_eq!(report.socket, layout.socket_path());
        assert_eq!(report.faults, 0);
        assert!(
            report
                .line()
                .contains(&format!("pid {}", std::process::id()))
        );
        assert!(report.line().contains("faults 0"));
        // Nothing serves this workspace socket, so the client is up without a
        // server link and the verb refuses.
        assert!(!report.connected);
        assert_eq!(report.exit_code(), 2);
    }

    #[tokio::test]
    async fn absent_pid_file_has_no_status() {
        let dir = tempdir().unwrap();
        assert_eq!(status(dir.path()).await.unwrap(), None);
    }

    /// A script reads `status`'s exit code, so a client that is up without a
    /// server link is a refusal: its work cannot reach the cluster.
    #[test]
    fn status_exit_code_follows_the_link() {
        let connected = StatusReport {
            pid: 1,
            uptime: Duration::ZERO,
            socket: PathBuf::from("/tmp/s"),
            faults: 0,
            connected: true,
        };
        assert_eq!(connected.exit_code(), 0);
        assert_eq!(
            StatusReport {
                connected: false,
                ..connected
            }
            .exit_code(),
            2
        );
    }

    #[test]
    fn start_line_names_pid_and_socket() {
        let dir = tempdir().unwrap();
        let socket = socket_file(dir.path());
        let line = start_line(4242, &socket);
        assert!(line.contains("pid 4242"));
        assert!(line.ends_with(&format!("socket {}", socket.display())));
    }
}
