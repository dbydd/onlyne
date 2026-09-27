use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant, SystemTime};

use clap::{CommandFactory, Parser, Subcommand};
use onlyne_config::Spec;
use onlyne_config::layout::ServerRoot;
use onlyne_wire::socket::{
    RegistrationFile, SOCKET_SUFFIX, read_registration, registration_path, remove_registration,
    runtime_dir_path, socket_path, workspace_digest,
};

use crate::generate::{GenerateArgs, GenerateError, generate};
use crate::{Server, ServerInit};

const START_READY_MS: u64 = 10_000;
const STOP_WAIT_MS: u64 = 10_000;
const POLL_MS: u64 = 50;

#[derive(Debug, Parser)]
#[command(name = "onlyne-server", version, about = "Onlyne v1 routing daemon")]
struct Cli {
    /// Emit progress as one JSON object per line on stderr.
    #[arg(long, global = true)]
    json: bool,
    /// Suppress progress output.
    #[arg(long, global = true)]
    quiet: bool,
    /// Add detail to progress output.
    #[arg(long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create `<root>/.onlyne/` with a `[server]` spec and a self-signed keypair.
    Init {
        /// Server root directory.
        #[arg(long)]
        root: PathBuf,
        /// TCP address the routing daemon binds, as `host:port`.
        #[arg(long)]
        listen: String,
        /// Overwrite an existing `spec.toml`.
        #[arg(long)]
        force: bool,
    },
    /// Bind the listeners and serve the cluster.
    Run {
        /// Server root directory.
        #[arg(long)]
        root: PathBuf,
    },
    /// Spawn `run` detached and wait for the admin socket.
    Start {
        /// Server root directory.
        #[arg(long)]
        root: PathBuf,
    },
    /// Signal the recorded server process and wait for it to exit.
    Stop {
        /// Server root directory.
        #[arg(long)]
        root: PathBuf,
    },
    /// Report process state read from this server root.
    Status {
        /// Server root directory.
        #[arg(long)]
        root: PathBuf,
    },
    /// Render role workspaces from the templates under the server root.
    Generate {
        /// Server root directory.
        #[arg(long)]
        root: PathBuf,
        /// Template path relative to the template root; repeat for several.
        #[arg(long)]
        template: Vec<String>,
        /// Role name from `spec.toml`; repeat for several.
        #[arg(long)]
        role: Vec<String>,
        /// Output directory; defaults to `<root>/.onlyne/ws`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Replace generated files in an existing workspace.
        #[arg(long)]
        force: bool,
    },
}

/// Progress verbosity shared by every subcommand.
#[derive(Debug, Clone, Copy)]
struct Output {
    json: bool,
    quiet: bool,
    verbose: bool,
}

impl Output {
    /// One progress line on stderr; silent under `--quiet`.
    fn status(&self, message: &str) {
        if !self.quiet {
            eprintln!("{message}");
        }
    }

    /// One JSON progress object on stderr; silent under `--quiet`.
    fn emit(&self, value: serde_json::Value) {
        if !self.quiet {
            eprintln!("{value}");
        }
    }

    /// Extra progress line, printed only under `--verbose`.
    fn detail(&self, message: &str) {
        if self.verbose {
            self.status(message);
        }
    }
}

/// Route `tracing` output to stderr, which `start` redirects into
/// `.onlyne/logs/server.log` (plan §2).
///
/// A process that already installed a subscriber keeps it, so a test harness
/// can capture its own spans.
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

pub async fn entrypoint() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    entrypoint_with(args).await
}

pub async fn entrypoint_with(args: Vec<String>) -> i32 {
    init_logging();
    let argv = std::iter::once("onlyne-server".to_string()).chain(args);
    let cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        Err(error) => {
            use clap::error::ErrorKind;
            match error.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                    print!("{error}");
                    return 0;
                }
                _ => {
                    eprint!("{error}");
                    return 2;
                }
            }
        }
    };
    let output = Output {
        json: cli.json,
        quiet: cli.quiet,
        verbose: cli.verbose,
    };
    match cli.command {
        Some(Command::Init {
            root,
            listen,
            force,
        }) => init_command(&root, &listen, force, output),
        Some(Command::Run { root }) => run_command(&root, output).await,
        Some(Command::Start { root }) => start_command(&root, output).await,
        Some(Command::Stop { root }) => stop_command(&root, output).await,
        Some(Command::Status { root }) => status_command(&root, output),
        Some(Command::Generate {
            root,
            template,
            role,
            out,
            force,
        }) => {
            let args = GenerateArgs {
                root: root.clone(),
                templates: template,
                roles: role,
                out,
                force,
            };
            generate_command(&root, args, output)
        }
        None => {
            let mut cmd = Cli::command();
            let _ = cmd.print_help();
            println!();
            2
        }
    }
}

fn init_command(root: &Path, listen: &str, force: bool, output: Output) -> i32 {
    let layout = ServerRoot::resolve(root);
    let spec_path = layout.spec_path();
    if spec_path.exists() && !force {
        return refuse(GenerateError::RefuseOverwrite { path: spec_path });
    }
    if let Err(error) = layout.bootstrap() {
        eprintln!("onlyne-server: {error}");
        return 1;
    }
    let name = cluster_name(root);
    let cert = match onlyne_net::load_or_create(&layout.key_path(), &name) {
        Ok(cert) => cert,
        Err(error) => {
            eprintln!("onlyne-server: {error}");
            return 1;
        }
    };
    let text = spec_template(&name, listen, &cert.spki_pin);
    if let Err(error) = Spec::parse_named(&text, "spec.toml") {
        eprintln!("onlyne-server: generated spec does not parse: {error}");
        return 1;
    }
    if let Err(error) = std::fs::write(&spec_path, text.as_bytes()) {
        eprintln!("onlyne-server: {}: {error}", spec_path.display());
        return 1;
    }
    if output.json {
        output.emit(serde_json::json!({"event": "init", "spec": spec_path.display().to_string()}));
    } else {
        output.status(&format!("onlyne-server: wrote {}", spec_path.display()));
    }
    if output.json {
        println!(
            "{}",
            serde_json::json!({"cert_pin": cert.spki_pin, "spec": spec_path.display().to_string()})
        );
    } else {
        println!("{}", cert.spki_pin);
    }
    0
}

fn refuse(error: GenerateError) -> i32 {
    eprintln!("{error}");
    error.exit_code()
}

fn cluster_name(root: &Path) -> String {
    root.canonicalize()
        .ok()
        .as_deref()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "onlyne-cluster".to_string())
}

fn spec_template(name: &str, listen: &str, cert_pin: &str) -> String {
    format!(
        "# Onlyne v1 cluster spec. Written by `onlyne-server init`.\n\
         #\n\
         # `name` is the cluster name handed to every client.\n\
         # `listen` is the TCP address the routing daemon binds.\n\
         # `cert_pin` pins the server certificate stored in `.onlyne/keys/server.key`,\n\
         # and every client verifies the TLS endpoint against it.\n\
         [server]\n\
         name = {name:?}\n\
         listen = {listen:?}\n\
         cert_pin = {cert_pin:?}\n\
         note_queue = false\n\
         fault_history_days = 14\n\
         resync_lag = 256\n\
         heartbeat_timeout_ms = 30000\n\
         stale_watch_secs = 60\n\
         heartbeat_grace_secs = 90\n\
         # `ghost_sweep_secs` sets the ghost sweep's scan interval. One pass\n\
         # settles a `working` session row whose own task ledger row already\n\
         # reached a terminal state, and records the write in `ghost_sweeps`.\n\
         # 0 disables the sweep.\n\
         ghost_sweep_secs = 60\n\
         # `requeue_max_attempts` caps the automatic requeues one in-flight row\n\
         # may take, and `requeue_ttl_secs` bounds its age from enqueue. Both\n\
         # default to 0, which leaves the requeue gate uncapped.\n\
         requeue_max_attempts = 0\n\
         requeue_ttl_secs = 0\n\
         agent_package = \"\"\n\
         template_root = \".onlyne/templates\"\n\
         # A session's backend and its `acp` parameters live in the role\n\
         # workspace's `.onlyne/config.toml`, which `onlyne-client init` writes,\n\
         # not in this spec.\n\
         \n\
         # Each role is one `[[client]]` row. `onlyne-client init` prints a ready row\n\
         # whose `key` value matches the workspace it just created.\n"
    )
}

async fn run_command(root: &Path, output: Output) -> i32 {
    let layout = ServerRoot::resolve(root);
    let spec = match Spec::load(layout.spec_path()) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("onlyne-server: {error}");
            return 1;
        }
    };
    let init = ServerInit {
        root: root.to_path_buf(),
        listen: Some(spec.server.listen.clone()),
    };
    let server = match Server::open(&init) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("onlyne-server: {error:#}");
            return 1;
        }
    };
    if output.json {
        output.emit(serde_json::json!({
            "event": "serving",
            "root": root.display().to_string(),
            "listen": spec.server.listen,
        }));
    } else {
        output.status(&format!(
            "onlyne-server: serving {} on {}",
            root.display(),
            spec.server.listen
        ));
    }
    match crate::serve(server).await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("onlyne-server: {error:#}");
            1
        }
    }
}

/// The registration `root`'s daemon published, when one is readable.
fn published(root: &Path) -> Option<RegistrationFile> {
    read_registration(root).ok().flatten()
}

/// The pid the registration for `root` names, when that process is alive.
///
/// v2 dropped `run/server.pid`: the registration is the file that names the
/// process serving a tree, and it is written by that process at bind time, so
/// start, stop, and status read the same fact the CLI client resolves.
fn live_pid(root: &Path) -> Option<u32> {
    published(root)
        .map(|registration| registration.pid)
        .filter(|pid| process_alive(*pid))
}

/// The socket a reader resolves for `root`, without creating anything.
///
/// `status` reports on a machine that may never have run a daemon, so it names
/// the path without asking the runtime directory to appear.
fn socket_path_of(root: &Path) -> PathBuf {
    runtime_dir_path().join(format!("{}{SOCKET_SUFFIX}", workspace_digest(root)))
}

/// Drop the endpoint files a just-exited `pid` published.
///
/// Only while the registration still names that pid: a daemon that has since
/// restarted owns these files, and clearing them would unlink the socket out
/// from under it.
fn clear_dead_endpoint(root: &Path, pid: u32) {
    if published(root).map(|registration| registration.pid) != Some(pid) {
        return;
    }
    let _ = remove_registration(root);
    if let Ok(socket) = socket_path(root) {
        let _ = fs::remove_file(socket);
    }
}

/// Remove a run socket and registration left behind by a server that is no
/// longer running.
///
/// A `SIGKILL`ed server leaves both files in the runtime directory. The
/// readiness poll below watches the registration, so the start path clears the
/// pair first; the published pid decides, and a live pid keeps both for the
/// already-running answer above.
pub fn clear_stale_socket(root: &Path) -> bool {
    if live_pid(root).is_some() {
        return false;
    }
    let socket = match socket_path(root) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("onlyne-server: resolve the runtime directory: {error}");
            return false;
        }
    };
    let mut removed = false;
    if socket.exists() {
        match fs::remove_file(&socket) {
            Ok(()) => removed = true,
            Err(error) => {
                eprintln!(
                    "onlyne-server: remove the stale socket {}: {error}",
                    socket.display()
                );
            }
        }
    }
    if let Err(error) = remove_registration(root) {
        eprintln!(
            "onlyne-server: remove the stale registration {}: {error}",
            registration_path(root).display()
        );
    }
    removed
}

/// Spawn `run` detached and wait for the registration that says it is serving.
async fn start_command(root: &Path, output: Output) -> i32 {
    if let Some(pid) = live_pid(root) {
        eprintln!("onlyne: server already running at pid {pid}");
        return 2;
    }
    clear_stale_socket(root);
    let layout = ServerRoot::resolve(root);
    if let Err(error) = layout.bootstrap() {
        eprintln!("onlyne-server: {error}");
        return 1;
    }
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("onlyne-server: cannot locate the running binary: {error}");
            return 1;
        }
    };
    let log = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(layout.log_path())
    {
        Ok(file) => file,
        Err(error) => {
            eprintln!("onlyne-server: {}: {error}", layout.log_path().display());
            return 1;
        }
    };
    let log_copy = match log.try_clone() {
        Ok(file) => file,
        Err(error) => {
            eprintln!("onlyne-server: {}: {error}", layout.log_path().display());
            return 1;
        }
    };
    let mut daemon = ProcessCommand::new(executable);
    daemon
        .arg("run")
        .arg("--root")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_copy));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        daemon.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        daemon.creation_flags(0x0000_0200); // CREATE_NEW_PROCESS_GROUP
    }
    let child = daemon.spawn();
    let child = match child {
        Ok(child) => child,
        Err(error) => {
            eprintln!("onlyne-server: cannot spawn the daemon: {error}");
            return 1;
        }
    };
    let pid = child.id();
    let socket = socket_path_of(root);
    let deadline = Instant::now() + Duration::from_millis(START_READY_MS);
    while Instant::now() < deadline {
        if published(root).is_some() {
            if output.json {
                output.emit(serde_json::json!({
                    "event": "started",
                    "pid": pid,
                    "socket": socket.display().to_string(),
                }));
            } else {
                output.status(&format!("onlyne-server: started pid {pid}"));
            }
            return 0;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
    eprintln!(
        "onlyne: server did not publish {} within {START_READY_MS}ms",
        registration_path(root).display()
    );
    #[cfg(unix)]
    {
        let _ = ProcessCommand::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .status();
    }
    #[cfg(windows)]
    {
        terminate_pid(pid);
    }
    clear_dead_endpoint(root, pid);
    1
}

/// Signal the published process and wait a bounded time for it to exit.
async fn stop_command(root: &Path, output: Output) -> i32 {
    let Some(pid) = live_pid(root) else {
        clear_stale_socket(root);
        eprintln!("onlyne: server not running");
        return 2;
    };
    #[cfg(unix)]
    {
        let _ = ProcessCommand::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();
    }
    #[cfg(windows)]
    {
        terminate_pid(pid);
    }
    let deadline = Instant::now() + Duration::from_millis(STOP_WAIT_MS);
    while Instant::now() < deadline {
        if !process_alive(pid) {
            clear_dead_endpoint(root, pid);
            if output.json {
                output.emit(serde_json::json!({"event": "stopped", "pid": pid}));
            } else {
                output.status(&format!("onlyne-server: stopped pid {pid}"));
            }
            return 0;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
    eprintln!("onlyne: server {pid} did not stop within {STOP_WAIT_MS}ms");
    1
}

/// Process-level answer read from this root, distinct from the admin `status` op.
fn status_command(root: &Path, output: Output) -> i32 {
    let layout = ServerRoot::resolve(root);
    let pid = live_pid(root);
    let uptime_s = pid.and_then(|_| file_age_seconds(&registration_path(root)));
    let socket = socket_path_of(root);
    let spec_path = layout.spec_path();
    let spec_hash = match Spec::load(&spec_path) {
        Ok(spec) => Some(spec.semantic_hash()),
        Err(error) => {
            eprintln!("onlyne-server: {error}");
            None
        }
    };
    let store_ready = store_reachable(&layout.state_db_path());
    let spec_readable = spec_hash.is_some();
    let report = serde_json::json!({
        "running": pid.is_some(),
        "pid": pid,
        "root": root.display().to_string(),
        "socket": socket.display().to_string(),
        "socket_present": socket.exists(),
        "uptime_s": uptime_s,
        "spec": spec_path.display().to_string(),
        "spec_hash": spec_hash.clone(),
        "store": layout.state_db_path().display().to_string(),
        "store_reachable": store_ready,
    });
    if output.json {
        println!("{report}");
    } else {
        println!("running: {}", pid.is_some());
        println!(
            "pid: {}",
            pid.map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
        println!("root: {}", root.display());
        println!("socket: {}", socket.display());
        println!("socket_present: {}", socket.exists());
        println!(
            "uptime_s: {}",
            uptime_s
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
        println!(
            "spec_hash: {}",
            spec_hash.unwrap_or_else(|| "-".to_string())
        );
        println!("store_reachable: {store_ready}");
    }
    if spec_readable { 0 } else { 1 }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    ProcessCommand::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

#[cfg(windows)]
fn terminate_pid(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            let _ = TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

fn file_age_seconds(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(modified).ok()?.as_secs())
}

fn store_reachable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|connection| {
            connection.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .is_ok()
}

fn generate_command(root: &Path, args: GenerateArgs, output: Output) -> i32 {
    let spec = match Spec::load(ServerRoot::resolve(root).spec_path()) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("onlyne-server: {error}");
            return 1;
        }
    };
    match generate(&args, &spec) {
        Ok(report) => {
            if output.json {
                output.emit(serde_json::json!({
                    "event": "generated",
                    "out": report.out.display().to_string(),
                    "roles": report.roles,
                }));
            } else {
                output.status(&format!(
                    "onlyne-server: generated {} role(s) at {}",
                    report.roles.len(),
                    report.out.display()
                ));
                for role in &report.roles {
                    output.detail(&format!(
                        "onlyne-server: role {} template {} dir {}",
                        role.role, role.template, role.dir
                    ));
                }
            }
            print!("{}", report.fragment);
            0
        }
        Err(error) => refuse(error),
    }
}
