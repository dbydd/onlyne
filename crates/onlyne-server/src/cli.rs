use std::fs::{self, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant, SystemTime};

use clap::{CommandFactory, Parser, Subcommand};
use onlyne_config::Spec;
use onlyne_layout::{ServerRoot, apply_private_mode};

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
        return refuse(GenerateError::WorkspaceExists(spec_path));
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
         agent_package = \"\"\n\
         template_root = \".onlyne/templates\"\n\
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

/// Remove a run socket left behind by a server that is no longer running.
///
/// A `SIGKILL`ed server leaves its bound path on disk. The readiness poll below
/// watches that path, so the start path removes it first; the recorded pid
/// decides, and a live pid keeps the path for the already-running answer above.
pub fn clear_stale_socket(layout: &ServerRoot) -> bool {
    let socket = layout.socket_path();
    if !socket.exists() {
        return false;
    }
    if read_pid(&layout.pid_path())
        .filter(|pid| process_alive(*pid))
        .is_some()
    {
        return false;
    }
    match fs::remove_file(&socket) {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "onlyne-server: remove the stale socket {}: {error}",
                socket.display()
            );
            false
        }
    }
}

/// Spawn `run` detached, record its pid, and wait for the admin socket.
async fn start_command(root: &Path, output: Output) -> i32 {
    let layout = ServerRoot::resolve(root);
    let pid_path = layout.pid_path();
    if let Some(pid) = read_pid(&pid_path).filter(|pid| process_alive(*pid)) {
        eprintln!("onlyne: server already running at pid {pid}");
        return 2;
    }
    clear_stale_socket(&layout);
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
    let child = ProcessCommand::new(executable)
        .arg("run")
        .arg("--root")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_copy))
        .process_group(0)
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(error) => {
            eprintln!("onlyne-server: cannot spawn the daemon: {error}");
            return 1;
        }
    };
    let pid = child.id();
    if let Err(error) = fs::write(&pid_path, format!("{pid}\n")) {
        eprintln!("onlyne-server: {}: {error}", pid_path.display());
        return 1;
    }
    if let Err(error) = apply_private_mode(&pid_path) {
        eprintln!("onlyne-server: {}: {error}", pid_path.display());
        return 1;
    }
    let deadline = Instant::now() + Duration::from_millis(START_READY_MS);
    while Instant::now() < deadline {
        if layout.socket_path().exists() {
            if output.json {
                output.emit(serde_json::json!({
                    "event": "started",
                    "pid": pid,
                    "socket": layout.socket_path().display().to_string(),
                }));
            } else {
                output.status(&format!("onlyne-server: started pid {pid}"));
            }
            return 0;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
    eprintln!(
        "onlyne: server did not open {} within {START_READY_MS}ms",
        layout.socket_path().display()
    );
    let _ = ProcessCommand::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status();
    let _ = fs::remove_file(&pid_path);
    1
}

/// Signal the recorded process and wait a bounded time for it to exit.
async fn stop_command(root: &Path, output: Output) -> i32 {
    let layout = ServerRoot::resolve(root);
    let pid_path = layout.pid_path();
    let Some(pid) = read_pid(&pid_path) else {
        eprintln!("onlyne: server not running");
        return 2;
    };
    if !process_alive(pid) {
        let _ = fs::remove_file(&pid_path);
        eprintln!("onlyne: server not running");
        return 2;
    }
    let _ = ProcessCommand::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
    let deadline = Instant::now() + Duration::from_millis(STOP_WAIT_MS);
    while Instant::now() < deadline {
        if !process_alive(pid) {
            let _ = fs::remove_file(&pid_path);
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
    let pid = read_pid(&layout.pid_path()).filter(|pid| process_alive(*pid));
    let uptime_s = pid.and_then(|_| pid_file_age_seconds(&layout.pid_path()));
    let socket = layout.socket_path();
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

fn read_pid(path: &Path) -> Option<u32> {
    let text = fs::read_to_string(path).ok()?;
    text.trim().parse().ok()
}

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

fn pid_file_age_seconds(path: &Path) -> Option<u64> {
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
