//! Black-box scenario test harness: spawn real server/client/agent processes,
//! drive the system through admin socket and adapter sockets, observe ledger and
//! sessions.

use anyhow::{Context, Result, anyhow, bail};
use onlyne_proto::{
    AdminOp, Frame, LedgerEntry, LedgerQuery, QuerySessionsArgs, ResBody, SessionRow, new_id,
};
use onlyne_wire::socket::{connect_local, read_registration, registration_path, socket_path};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{Instant, timeout};
use tracing::{debug, warn};

use crate::AgentScript;

const SERVER_INIT_TIMEOUT: Duration = Duration::from_secs(30);
const ADMIN_TIMEOUT: Duration = Duration::from_secs(5);
const ROLE_ONLINE_TIMEOUT: Duration = Duration::from_secs(10);

/// Install one subscriber for the process, so the child output this harness
/// collects is actually readable.
///
/// Every spawned process's stdout and stderr is already captured into `debug!`
/// and `warn!` below, but without a subscriber those lines go nowhere, and a
/// failing scenario then reports a timeout with no reason beside it. `RUST_LOG`
/// raises or narrows the level; the default is `warn`, which is where child
/// stderr lands and where a scenario's own problem shows up unasked.
fn init_tracing() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(true)
            .try_init();
    });
}

/// The socket one owner root is bound to, read from its registration file.
///
/// The registration is the answer rather than a guess at `run/s`: a daemon that
/// has not bound yet leaves no registration, and one that has published a
/// different path is still reached.
fn resolve_socket(root: &Path) -> Result<PathBuf> {
    match read_registration(root) {
        Ok(Some(reg)) => {
            if let Some(runtime) = reg.runtime.as_deref().filter(|r| !r.is_empty()) {
                return Ok(PathBuf::from(runtime));
            }
            socket_path(root).context("derive socket path")
        }
        Ok(None) => socket_path(root).context("derive socket path (no registration yet)"),
        Err(e) => Err(anyhow!(e)).context("read registration"),
    }
}

/// Write one admin request frame, read the response, bounded by timeout.
async fn admin_request(root: &Path, op: AdminOp, timeout_ms: u64) -> Result<ResBody> {
    let socket = resolve_socket(root)?;
    let mut stream = timeout(Duration::from_millis(timeout_ms), connect_local(&socket))
        .await
        .context("admin socket connect timeout")?
        .context("admin socket connect failed")?;

    let request = AdminFrame::Req { id: new_id(), op };

    // Write frame
    timeout(
        Duration::from_millis(timeout_ms),
        onlyne_wire::write_frame(&mut stream, &request),
    )
    .await
    .context("admin write timeout")?
    .context("admin write failed")?;

    // Read response
    let frame: Frame<AdminOp> = timeout(
        Duration::from_millis(timeout_ms),
        onlyne_wire::read_frame(&mut stream),
    )
    .await
    .context("admin read timeout")?
    .context("admin read failed")?
    .ok_or_else(|| anyhow!("admin socket closed before response"))?;

    match frame {
        Frame::Res { body, .. } => Ok(body),
        other => bail!("expected res frame, got {:?}", other),
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "f")]
enum AdminFrame {
    Req {
        id: String,
        #[serde(flatten)]
        op: AdminOp,
    },
}

/// One line per ledger row, carrying the fields a failing poll is asking about.
fn render_ledger(rows: &[LedgerEntry]) -> String {
    if rows.is_empty() {
        return "(no rows)".to_string();
    }
    rows.iter()
        .map(|row| {
            format!(
                "msg={} task={} hop={} state={:?} family={} parent={}",
                row.msg_id,
                row.task.as_deref().unwrap_or("-"),
                row.hop,
                row.state,
                row.family.as_deref().unwrap_or("-"),
                row.parent_task.as_deref().unwrap_or("-"),
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Spawn one server process for `root`, collecting its output in the background.
///
/// The root is the cluster: the spec, the key material, and the stores all live
/// under it, which is why starting a second process on the same root is the
/// whole of a restart.
fn spawn_server(bin: &Path, root: &Path) -> Result<Child> {
    let mut child = Command::new(bin)
        .args(["run", "--root", root.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("server spawn")?;

    if let Some(stdout) = child.stdout.take() {
        let root = root.to_path_buf();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                debug!("server[{}]: {}", root.display(), line);
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let root = root.to_path_buf();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                warn!("server[{}] stderr: {}", root.display(), line);
            }
        });
    }
    Ok(child)
}

/// Wait until the server at `root` is serving.
///
/// The registration file is the daemon's own "I am serving here" statement, so a
/// cluster that has not published one has nothing to dial; `run/s` would be a
/// guess at a path the daemon may never have used.
async fn wait_server_ready(root: &Path) -> Result<()> {
    let registration = registration_path(root);
    let ready_start = Instant::now();
    let mut last_check = String::new();
    loop {
        if ready_start.elapsed() > SERVER_INIT_TIMEOUT {
            bail!(
                "server wait-ready timeout after {:?}, last: {}",
                ready_start.elapsed(),
                last_check
            );
        }
        if registration.exists() {
            // Wait a bit for socket to be bound
            tokio::time::sleep(Duration::from_millis(300)).await;
            // Try a roles query to confirm it's serving
            last_check = match admin_request(root, AdminOp::Roles(Default::default()), 2000).await {
                Ok(body) if body.ok => break,
                Ok(body) => format!(
                    "registration published at {}, but roles answered ok=false: {:?}",
                    registration.display(),
                    body.error
                ),
                Err(e) => format!(
                    "registration published at {}, but roles failed: {e}",
                    registration.display()
                ),
            };
            tokio::time::sleep(Duration::from_millis(200)).await;
        } else {
            last_check = "no registration file".to_string();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Ok(())
}

/// One spawned cluster: server, temp directories, and tracked child processes.
///
/// On drop, kills all tracked children (server, clients, agents) and removes
/// temp directories.
pub struct Cluster {
    server_root: PathBuf,
    _temp_dir: TempDir,
    server_child: Mutex<Option<Child>>,
    /// Tracked child processes: clients, agents, any other spawned processes.
    children: Arc<Mutex<Vec<Tracked>>>,
    /// Role name to workspace root, so a role's own registration file can be
    /// found without the caller threading the workspace through every call.
    role_workspaces: Arc<Mutex<HashMap<String, PathBuf>>>,
}

/// One tracked child process, under the label a scenario addresses it by.
///
/// A scenario that kills a client has to name the one it wants, and the only
/// thing a caller holds is the workspace the process was started for.
struct Tracked {
    label: String,
    child: Child,
}

impl Tracked {
    /// The label one child is tracked under: the process it is, and the
    /// workspace it serves.
    fn label(kind: &str, workspace: &Path) -> String {
        format!("{kind}:{}", workspace.display())
    }

    fn new(kind: &str, workspace: &Path, child: Child) -> Self {
        Self {
            label: Self::label(kind, workspace),
            child,
        }
    }
}

impl Cluster {
    /// Start one cluster from a spec TOML string: initialize server root, spawn
    /// server process, wait for ready.
    ///
    /// Returns the cluster handle, which kills all tracked processes and cleans
    /// up temp directories on drop.
    pub async fn start(spec_toml: &str) -> Result<Self> {
        init_tracing();
        let temp_dir = tempfile::tempdir().context("create temp dir")?;
        let server_root = temp_dir.path().join("server");

        // onlyne-server init
        let server_bin = Self::bin_path("onlyne-server")?;
        let port = Self::free_port().await?;
        let listen = format!("127.0.0.1:{}", port);

        let init_status = Command::new(&server_bin)
            .args([
                "init",
                "--root",
                server_root.to_str().unwrap(),
                "--listen",
                &listen,
            ])
            .status()
            .await
            .context("server init spawn")?;
        if !init_status.success() {
            bail!("server init failed: {}", init_status);
        }

        // Read init-generated spec to extract cert_pin
        let spec_path = server_root.join(".onlyne/spec.toml");
        let init_spec = tokio::fs::read_to_string(&spec_path)
            .await
            .context("read init spec")?;

        // Extract cert_pin line
        let cert_pin_line = init_spec
            .lines()
            .find(|line| line.trim().starts_with("cert_pin"))
            .ok_or_else(|| anyhow!("cert_pin not found in init spec"))?;

        // Merge: test spec + cert_pin + the port this run reserved.
        //
        // A case writes `listen = "127.0.0.1:0"` as a placeholder, but port 0
        // reaching the running server is not a placeholder: the server binds
        // whatever the file says, and every client reads the port out of this
        // same spec, so the placeholder makes them dial port 0 and the role
        // never links. The reserved address replaces the line inside `[server]`
        // only, leaving a `listen` key in any other table alone.
        let server_header = format!("[server]\n{}\nlisten = \"{}\"", cert_pin_line, listen);
        let merged_spec = if spec_toml.contains("[server]") {
            let (before, after) = spec_toml
                .split_once("[server]")
                .ok_or_else(|| anyhow!("[server] header vanished from the test spec"))?;
            // The `[server]` table runs until the next table header; a `listen`
            // past that belongs to someone else's table.
            let table_end = after.find("\n[").map(|at| at + 1).unwrap_or(after.len());
            let (table, rest) = after.split_at(table_end);
            let stripped: Vec<&str> = table
                .lines()
                .filter(|line| !line.trim_start().starts_with("listen"))
                .collect();
            format!(
                "{}{}\n{}\n{}",
                before,
                server_header,
                stripped.join("\n"),
                rest
            )
        } else {
            format!("{}\n{}", server_header, spec_toml)
        };

        tokio::fs::write(&spec_path, merged_spec)
            .await
            .context("write merged spec")?;

        let server_child = spawn_server(&server_bin, &server_root)?;
        wait_server_ready(&server_root).await?;

        Ok(Cluster {
            server_root,
            _temp_dir: temp_dir,
            server_child: Mutex::new(Some(server_child)),
            children: Arc::new(Mutex::new(Vec::new())),
            role_workspaces: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Stop the server process and start a fresh one on the same root.
    ///
    /// Everything durable about a cluster lives under the root — the spec, the
    /// key material, and the stores — so the second daemon is the same cluster
    /// in a new process, and a scenario can read exactly what the first one
    /// left behind.
    ///
    /// The registration the old process published is removed first. It names
    /// the socket a dead process no longer serves, and waiting on it would
    /// report the predecessor's readiness as the successor's.
    pub async fn restart_server(&self) -> Result<()> {
        let server_bin = Self::bin_path("onlyne-server")?;
        let previous = self.server_child.lock().await.take();
        if let Some(mut previous) = previous {
            let _ = previous.start_kill();
            let _ = previous.wait().await;
        }
        std::fs::remove_file(registration_path(&self.server_root))
            .or_else(|error| match error.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(error),
            })
            .context("clear the dead server's registration")?;
        let server_child = spawn_server(&server_bin, &self.server_root)?;
        *self.server_child.lock().await = Some(server_child);
        wait_server_ready(&self.server_root).await
    }

    /// Register one role workspace: run onlyne-client init, write the fragment to
    /// spec.toml, reload the server.
    ///
    /// Returns the workspace directory.
    pub async fn register_role(
        &self,
        role: &str,
        prose: &str,
        acl: Option<&str>,
    ) -> Result<PathBuf> {
        let workspace = self._temp_dir.path().join(role);
        tokio::fs::create_dir_all(&workspace)
            .await
            .context("create role workspace")?;

        let client_bin = Self::bin_path("onlyne-client")?;
        let mut cmd = Command::new(&client_bin);
        cmd.args([
            "init",
            "--workspace",
            workspace.to_str().unwrap(),
            "--role",
            role,
            "--server-root",
            self.server_root.to_str().unwrap(),
            "--prose",
            prose,
        ]);

        let output = cmd.output().await.context("client init spawn")?;
        if !output.status.success() {
            bail!(
                "client init failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let fragment = String::from_utf8(output.stdout).context("client init stdout utf8")?;

        // Append fragment to spec.toml
        let spec_path = self.spec_path();
        let mut spec = tokio::fs::read_to_string(&spec_path)
            .await
            .context("read spec.toml")?;

        // If ACL override is given, filter out the default ACL lines from fragment
        let filtered_fragment = if let Some(acl_lines) = acl {
            let acl_keys: Vec<&str> = acl_lines
                .lines()
                .filter_map(|line| {
                    let trimmed = line.trim();
                    trimmed.find('=').map(|eq_pos| trimmed[..eq_pos].trim())
                })
                .collect();

            let mut result = String::new();
            for line in fragment.lines() {
                let trimmed = line.trim();
                let keep = if let Some(eq_pos) = trimmed.find('=') {
                    let key = trimmed[..eq_pos].trim();
                    !acl_keys.contains(&key)
                } else {
                    true
                };
                if keep {
                    result.push_str(line);
                    result.push('\n');
                }
            }
            result.push_str(acl_lines);
            result.push('\n');
            result
        } else {
            fragment
        };

        spec.push_str(&filtered_fragment);
        tokio::fs::write(&spec_path, spec)
            .await
            .context("write spec.toml with fragment")?;

        self.role_workspaces
            .lock()
            .await
            .insert(role.to_string(), workspace.clone());

        // Reload server
        self.reload().await.context("reload after register_role")?;

        Ok(workspace)
    }

    /// Spawn onlyne-client run for one workspace, track the process.
    ///
    /// Logs are captured in background.
    ///
    /// The scenario suite is a fake-backend suite: `onlyne-client run` needs a
    /// terminal host for the pane it puts each session in, and `fake` is the
    /// host that needs no external tool. The variable rides this child's own
    /// environment rather than the test process's, so the suite stays
    /// parallel-safe and a caller's own `ONLYNE_BACKEND` cannot change the
    /// host the suite selects.
    pub async fn start_client(&self, workspace: &Path) -> Result<()> {
        let client_bin = Self::bin_path("onlyne-client")?;
        let mut child = Command::new(&client_bin)
            .args(["run", "--workspace", workspace.to_str().unwrap()])
            .env("ONLYNE_BACKEND", "fake")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("client spawn")?;

        if let Some(stdout) = child.stdout.take() {
            let ws = workspace.to_path_buf();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    debug!("client[{}]: {}", ws.display(), line);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let ws = workspace.to_path_buf();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    warn!("client[{}] stderr: {}", ws.display(), line);
                }
            });
        }

        self.children
            .lock()
            .await
            .push(Tracked::new("client", workspace, child));
        Ok(())
    }

    /// Kill the client this workspace owns and reap it.
    ///
    /// The process is the role's whole local half: the server keeps the role's
    /// queue and this workspace keeps the client's own store, so a scenario can
    /// take the client away and put it back with `start_client` and read what
    /// each side remembered. A workspace with no live client is an error rather
    /// than a no-op — a case that means to drop a client and misses should not
    /// pass as though it had.
    pub async fn kill_client(&self, workspace: &Path) -> Result<()> {
        self.kill_child(&Tracked::label("client", workspace)).await
    }

    /// Kill the fake agent mounted for this workspace and reap it.
    ///
    /// A workspace that is about to move needs both halves gone: the client
    /// holds the socket it bound for the old path, and the agent holds the path
    /// it resolved at its own start.
    pub async fn kill_agent(&self, workspace: &Path) -> Result<()> {
        self.kill_child(&Tracked::label("agent", workspace)).await
    }

    /// Kill one tracked child by label and reap it.
    async fn kill_child(&self, label: &str) -> Result<()> {
        let tracked = {
            let mut children = self.children.lock().await;
            let index = children
                .iter()
                .position(|tracked| tracked.label == label)
                .ok_or_else(|| anyhow!("no tracked child named {label}"))?;
            children.remove(index)
        };
        let mut child = tracked.child;
        let _ = child.start_kill();
        let _ = child.wait().await;
        Ok(())
    }

    /// Spawn onlyne-agent-fake for one workspace with a script, track the process.
    pub async fn start_fake_agent(&self, workspace: &Path, script: &AgentScript) -> Result<()> {
        let agent_bin = Self::bin_path("onlyne-agent-fake")?;
        let script_json = serde_json::to_string(script).context("serialize script")?;

        let mut child = Command::new(&agent_bin)
            .args(["--workspace", workspace.to_str().unwrap(), "--stdin-script"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("fake agent spawn")?;

        // Write script to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(script_json.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            drop(stdin);
        }

        if let Some(stdout) = child.stdout.take() {
            let ws = workspace.to_path_buf();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    debug!("agent[{}]: {}", ws.display(), line);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let ws = workspace.to_path_buf();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    warn!("agent[{}] stderr: {}", ws.display(), line);
                }
            });
        }

        self.children
            .lock()
            .await
            .push(Tracked::new("agent", workspace, child));
        Ok(())
    }

    /// Admin send operation.
    pub async fn admin_send(&self, from: &str, to: &str, text: &str) -> Result<serde_json::Value> {
        let envelope = Box::new(onlyne_proto::new_envelope(
            onlyne_proto::MsgKind::Task,
            onlyne_proto::Principal::role(from),
            onlyne_proto::Principal::role(to),
            onlyne_proto::Body::text(text.to_string()),
            Some(onlyne_proto::Causality::root(onlyne_proto::new_task_id())),
        )?);

        let op = AdminOp::Send(onlyne_proto::AdminSend {
            from: from.to_string(),
            envelope,
        });

        let body = admin_request(&self.server_root, op, ADMIN_TIMEOUT.as_millis() as u64).await?;
        if !body.ok {
            bail!("admin send failed: {:?}", body.error);
        }
        body.data
            .ok_or_else(|| anyhow!("send response missing data"))
    }

    /// Query ledger.
    pub async fn query_ledger(&self, query: LedgerQuery) -> Result<Vec<LedgerEntry>> {
        let op = AdminOp::Ledger(query);
        let body = admin_request(&self.server_root, op, ADMIN_TIMEOUT.as_millis() as u64).await?;
        if !body.ok {
            bail!("ledger query failed: {:?}", body.error);
        }
        let data = body
            .data
            .ok_or_else(|| anyhow!("ledger response missing data"))?;
        let rows: Vec<LedgerEntry> =
            serde_json::from_value(data.get("ledger").cloned().unwrap_or(serde_json::json!([])))
                .context("parse ledger rows")?;
        Ok(rows)
    }

    /// Query sessions.
    pub async fn query_sessions(&self, query: QuerySessionsArgs) -> Result<Vec<SessionRow>> {
        let op = AdminOp::Sessions(query);
        let body = admin_request(&self.server_root, op, ADMIN_TIMEOUT.as_millis() as u64).await?;
        if !body.ok {
            bail!("sessions query failed: {:?}", body.error);
        }
        let data = body
            .data
            .ok_or_else(|| anyhow!("sessions response missing data"))?;
        let rows: Vec<SessionRow> = serde_json::from_value(
            data.get("sessions")
                .cloned()
                .unwrap_or(serde_json::json!([])),
        )
        .context("parse session rows")?;
        Ok(rows)
    }

    /// Reload the server spec.
    pub async fn reload(&self) -> Result<()> {
        let op = AdminOp::Reload(serde_json::json!({}));
        let body = admin_request(&self.server_root, op, ADMIN_TIMEOUT.as_millis() as u64).await?;
        if !body.ok {
            bail!("reload failed: {:?}", body.error);
        }
        Ok(())
    }

    /// Poll until a role is online, with timeout.
    pub async fn wait_role_online(&self, role: &str) -> Result<()> {
        let start = Instant::now();
        let workspace = self.role_workspaces.lock().await.get(role).cloned();
        let client_registration = workspace.as_ref().map(|ws| registration_path(ws));
        loop {
            if start.elapsed() > ROLE_ONLINE_TIMEOUT {
                bail!(
                    "role {} never came online; client registration: {}",
                    role,
                    match &client_registration {
                        Some(p) if p.exists() => p.display().to_string(),
                        Some(p) => format!("{} (absent)", p.display()),
                        None => "unknown workspace".to_string(),
                    }
                );
            }

            // The server counts a role online from its TLS link, which can be
            // healthy while the client's own local half is still coming up. The
            // role's registration file is the client's statement that it is
            // serving, so wait for it before judging the role ready.
            if let Some(path) = &client_registration {
                if !path.exists() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            }

            let op = AdminOp::Roles(onlyne_proto::QueryRolesArgs {
                role: Some(role.to_string()),
            });
            let body =
                admin_request(&self.server_root, op, ADMIN_TIMEOUT.as_millis() as u64).await?;
            if body.ok {
                if let Some(data) = body.data {
                    let roles: Vec<serde_json::Value> = serde_json::from_value(
                        data.get("roles").cloned().unwrap_or(serde_json::json!([])),
                    )
                    .unwrap_or_default();
                    if roles
                        .iter()
                        .any(|r| r.get("state").and_then(|s| s.as_str()) == Some("online"))
                    {
                        return Ok(());
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Poll until a predicate on ledger rows returns true, with timeout.
    ///
    /// A timeout reports the last snapshot it read. A bare "timeout" throws away
    /// the only evidence the caller has, and the callers that reach this are
    /// exactly the ones whose rows say what went wrong.
    pub async fn poll_ledger<F>(
        &self,
        query: LedgerQuery,
        predicate: F,
        timeout: Duration,
    ) -> Result<Vec<LedgerEntry>>
    where
        F: Fn(&[LedgerEntry]) -> bool,
    {
        let start = Instant::now();
        let mut last: Vec<LedgerEntry> = Vec::new();
        loop {
            if start.elapsed() > timeout {
                bail!(
                    "poll_ledger timeout after {timeout:?}; last read {} row(s): {}",
                    last.len(),
                    render_ledger(&last)
                );
            }

            let rows = self.query_ledger(query.clone()).await?;
            if predicate(&rows) {
                return Ok(rows);
            }
            last = rows;

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Poll until a predicate on session rows returns true, with timeout.
    pub async fn poll_sessions<F>(
        &self,
        query: QuerySessionsArgs,
        predicate: F,
        timeout: Duration,
    ) -> Result<Vec<SessionRow>>
    where
        F: Fn(&[SessionRow]) -> bool,
    {
        let start = Instant::now();
        loop {
            if start.elapsed() > timeout {
                bail!("poll_sessions timeout");
            }

            let rows = self.query_sessions(query.clone()).await?;
            if predicate(&rows) {
                return Ok(rows);
            }

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub fn server_root(&self) -> &Path {
        &self.server_root
    }

    /// The cluster's spec file, the one point of truth about its roles.
    pub fn spec_path(&self) -> PathBuf {
        self.server_root.join(".onlyne/spec.toml")
    }

    /// Run one `onlyne` verb against this cluster and answer its stdout.
    ///
    /// The harness drives the admin surface directly everywhere else;
    /// `generate` is the verb with no admin op behind it, so a scenario that
    /// exercises it runs the CLI an operator runs, against the same root.
    pub async fn cli(&self, args: &[&str]) -> Result<String> {
        let bin = Self::bin_path("onlyne")?;
        let output = Command::new(&bin)
            .args(["--server-root", self.server_root.to_str().unwrap()])
            .args(args)
            .output()
            .await
            .with_context(|| format!("run onlyne {}", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "onlyne {} failed ({}): {}",
                args.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8(output.stdout).context("onlyne stdout is not utf8")
    }

    /// The server's admin socket, read from its registration file.
    pub fn admin_socket(&self) -> Result<PathBuf> {
        resolve_socket(&self.server_root)
    }

    /// Resolve one workspace binary by name.
    ///
    /// `CARGO_BIN_EXE_<name>` is only set for binaries of the package under test,
    /// and this harness is a library, so the lookup below is what normally
    /// answers: walk to the workspace root and read `target/debug/<name>`. A
    /// scenario that spawns a binary MUST come through here rather than spelling
    /// a relative path, which resolves against the test's own working directory.
    pub fn bin_path(name: &str) -> Result<PathBuf> {
        let env_name = format!("CARGO_BIN_EXE_{}", name.replace('-', "_"));
        if let Ok(path) = std::env::var(&env_name) {
            return Ok(PathBuf::from(path));
        }

        let bin = Self::repo_root()?.join("target/debug").join(name);
        if !bin.exists() {
            bail!(
                "binary not found: {}; run cargo build --workspace",
                bin.display()
            );
        }
        Ok(bin)
    }

    /// The repository root: the directory whose `Cargo.toml` carries `[workspace]`.
    ///
    /// A scenario that reads a file the repository ships — the example template
    /// tree `generate` renders from, for one — resolves it from here rather than
    /// from the test's working directory.
    pub fn repo_root() -> Result<PathBuf> {
        let mut current = std::env::current_dir().context("get current dir")?;
        loop {
            let cargo_toml = current.join("Cargo.toml");
            if cargo_toml.exists() {
                let content = std::fs::read_to_string(&cargo_toml).context("read Cargo.toml")?;
                if content.contains("[workspace]") {
                    return Ok(current);
                }
            }
            if !current.pop() {
                bail!("workspace root not found");
            }
        }
    }

    async fn free_port() -> Result<u16> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind ephemeral port")?;
        let addr = listener.local_addr().context("get local addr")?;
        Ok(addr.port())
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        // Kill server
        if let Some(mut server) = self
            .server_child
            .try_lock()
            .ok()
            .and_then(|mut guard| guard.take())
        {
            let _ = server.start_kill();
        }

        // Kill tracked children
        if let Ok(mut children) = self.children.try_lock() {
            for mut tracked in children.drain(..) {
                let _ = tracked.child.start_kill();
            }
        }
    }
}
