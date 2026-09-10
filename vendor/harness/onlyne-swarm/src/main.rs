mod daemon;
mod db;
mod events;
mod hierarchy;
mod init;
mod ipc;
mod lifecycle;
mod orca;
mod orca_term;
mod proto;
mod reconcile;
mod repair;
mod root;
mod runtime;
mod sched;
mod skill;
mod sync;
mod template;
mod tui;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{
    generate,
    shells::{Fish, Zsh},
};
use std::io;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "onlyne-swarm",
    version,
    about = "Reactive multi-agent DAG workflow scheduler derived from Onlyne"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// The seven repair actions, one per `swarm repair <action>`.
///
/// Every arm sends the same `{"op":"repair","action":...}` request the socket
/// serves, so the CLI carries no policy of its own and the scheduler's `ok`
/// decides the exit code.
#[derive(Subcommand)]
enum RepairCmd {
    /// Show the stored tuple, the live probe, the task row, its faults and its intents
    Inspect { task_id: String },
    /// Move the task onto a new Pi generation after proving the old one gone
    Adopt { task_id: String },
    /// Supersede the generation with this handle, on the operator's attestation
    Rebind {
        task_id: String,
        #[arg(long)]
        handle: String,
    },
    /// Re-dispatch a fault's recovery task and re-arm its spent intents
    Retry { task_id: String },
    /// Settle the work as failed and queue the reason
    Fail {
        task_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Close the resource through the recycle ack path
    Close { task_id: String },
    /// Take one fault off the supervisor queue
    Ack { fault_id: i64 },
}

#[derive(Subcommand)]
enum Cmd {
    /// Initialize this directory as a swarm root (Onlyne workspace + full template starter)
    Init,
    /// Write or refresh the supervisor skill at .agents/skills/onlyne-swarm/SKILL.md
    ExportSkill,
    /// Start the scheduler (cwd is the swarm root; syncs .schedule first)
    Run {
        /// Bypass the ancestor-marker check (allows nested starts)
        #[arg(long)]
        force: bool,
        /// R4: run detached (double-fork + setsid); logs to .onlyne/logs,
        /// pid to .onlyne/run/scheduler.pid. Ctrl-C on the launching shell
        /// does not stop the ring; use `stop`.
        #[arg(long)]
        detach: bool,
    },
    /// R4: stop a detached scheduler (SIGTERM, then SIGKILL after 5s)
    Stop,
    /// Show status from an already-running scheduler
    Attach,
    /// Submit a Markdown payload file as a task to a workspace
    Submit {
        /// Target workspace tree-relative path (e.g. planner, a/b; root is ".")
        #[arg(long)]
        to: String,
        /// Markdown payload file
        #[arg(long)]
        payload: PathBuf,
    },
    /// Cancel a task family (signal + ack + tab close; no shell injection)
    Cancel {
        task_id: String,
        #[arg(long)]
        reason: Option<String>,
        /// Manual escape hatch: scoped pkill inside the tab before closing.
        /// Off the default path; the session normally dies by its own hand.
        #[arg(long)]
        force: bool,
    },
    /// Operator repair over a stuck session generation (needs a running scheduler)
    #[command(subcommand)]
    Repair(RepairCmd),
    /// List tasks or workspaces
    List {
        /// tasks | workspaces
        #[arg(long, default_value = "tasks")]
        what: String,
        #[arg(long)]
        state: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show status from an already-running scheduler (same as attach)
    Status,
    /// Open the monitoring TUI (connects to the running scheduler)
    Tui,
    /// Print shell completions (zsh or fish) to stdout
    ShellCompletions { shell: CompletionShell },
    /// Manage generated workspace instances
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Generate .ws from .agents/.schedule
    Create,
    /// Same as create (reconcile; never deletes instances or hand-written files)
    Sync,
}

#[derive(Copy, Clone, clap::ValueEnum)]
enum CompletionShell {
    Zsh,
    Fish,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init => {
            let cwd = std::env::current_dir()?;
            let root_p = crate::init::run_init(&cwd)?;
            println!("initialized swarm root at {}", root_p.display());
            Ok(())
        }
        Cmd::ExportSkill => {
            let cwd = std::env::current_dir()?;
            let root_p = root::cwd_root(&cwd);
            let path = crate::skill::export_skill(&root_p)?;
            println!("exported skill {}", path.display());
            Ok(())
        }
        Cmd::Run { force, detach } => {
            let cwd = std::env::current_dir()?;
            let root_p = root::ensure_root(&cwd, force)?;
            if detach {
                // R4: re-exec ourselves detached (child carries the
                // already-resolved root so the guard runs once). The parent
                // prints and returns; the child continues into the sync +
                // serve path below with SWARM_DETACHED_CHILD set.
                if std::env::var_os("ONLYNE_SWARM_DETACHED_CHILD").is_none() {
                    return launch_detached(&root_p);
                }
            }
            let report = sync::run_sync(&root_p)?;
            println!(
                "sync: +{} created, {} orphans, {} dangling links, {} legacy views",
                report.created.len(),
                report.orphans.len(),
                report.dangling.len(),
                report.legacy_views.len()
            );
            for o in &report.orphans {
                println!("orphan-instance: {o}");
            }
            for d in &report.dangling {
                println!("dangling-link: {d}");
            }
            for v in &report.legacy_views {
                println!("legacy-view: {v}");
            }
            for h in &report.hierarchy {
                println!("orca-node: {h}");
            }
            let _db = db::Db::open(&root_p)?;
            let mut children = daemon::ensure_all(&root_p)?;
            // R4: a detached scheduler owns its pid file; foreground leaves
            // no pid (Ctrl-C in the pane is the stop). The pid is what
            // `stop` and a stale-file `status` read.
            let detached = std::env::var_os("ONLYNE_SWARM_DETACHED_CHILD").is_some();
            if detached {
                if let Some(p) = root::swarm_pid(&root_p).parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                let _ = std::fs::write(
                    root::swarm_pid(&root_p),
                    format!("{}\n", std::process::id()),
                );
                println!(
                    "detached scheduler: pid {} log {}",
                    std::process::id(),
                    root::swarm_log(&root_p).display()
                );
            } else {
                println!("onlyne-swarm scheduler running at {}", root_p.display());
                // R5.4: the scheduler is a foreground process sharing this
                // pane's lifetime. Say so once: two rings have died to a
                // stray Ctrl-C in a shared pane already.
                println!(
                    "foreground scheduler in this pane; keep it exclusive — Ctrl-C here stops the ring"
                );
            }
            // serve() owns Ctrl-C/SIGTERM: it sets the Sched shutdown flag
            // (pump + reaper threads observe it and exit), then returns.
            // Only afterwards do we reap the managed daemons here.
            let served = ipc::serve(&root_p).await;
            if detached {
                let _ = std::fs::remove_file(root::swarm_pid(&root_p));
            }
            served?;
            println!("shutting down; stopping managed daemons");
            daemon::stop_all(&mut children).await;
            Ok(())
        }
        Cmd::Attach | Cmd::Status => {
            let cwd = std::env::current_dir()?;
            let root_p = root::client_root(&cwd);
            match orca::client_request(
                &root::swarm_sock(&root_p),
                serde_json::json!({"id": "cli", "op": "status"}),
            ) {
                Ok(v) => {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                    Ok(())
                }
                Err(e) => {
                    // R4: `status` doubles as the up-probe in scripts (e2e),
                    // so "nothing to attach to" must exit non-zero. A
                    // friendly line with exit 0 would read as "scheduler is
                    // up" and race a real start. The socket is never removed
                    // here: serve() takes over stale sockets, and a
                    // foreground scheduler has no pid file to prove it is
                    // alive.
                    if proc_alive(&root_p) {
                        // Pid says alive but the socket is dead: a real fault.
                        Err(e)
                    } else {
                        eprintln!("no scheduler running at {}", root_p.display());
                        std::process::exit(1);
                    }
                }
            }
        }
        Cmd::Stop => {
            let cwd = std::env::current_dir()?;
            let root_p = root::client_root(&cwd);
            let pid_path = root::swarm_pid(&root_p);
            let pid: i32 = match std::fs::read_to_string(&pid_path) {
                Ok(s) => s
                    .trim()
                    .parse()
                    .map_err(|_| anyhow::anyhow!("malformed pid file {}", pid_path.display()))?,
                Err(_) => {
                    println!("no detached scheduler pid at {}", pid_path.display());
                    return Ok(());
                }
            };
            stop_pid(&root_p, pid)?;
            Ok(())
        }
        Cmd::Submit { to, payload } => {
            let cwd = std::env::current_dir()?;
            let root_p = root::client_root(&cwd);
            let text = std::fs::read_to_string(&payload)?;
            let v = orca::client_request(
                &root::swarm_sock(&root_p),
                serde_json::json!({"id": "cli", "op": "submit", "to": to, "payload_markdown": text}),
            )?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        Cmd::Cancel {
            task_id,
            reason,
            force,
        } => {
            let cwd = std::env::current_dir()?;
            let root_p = root::client_root(&cwd);
            let v = orca::client_request(
                &root::swarm_sock(&root_p),
                serde_json::json!({"id": "cli", "op": "cancel", "task_id": task_id, "reason": reason, "force": force}),
            )?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        Cmd::Repair(cmd) => {
            let cwd = std::env::current_dir()?;
            let root_p = root::client_root(&cwd);
            let req = match cmd {
                RepairCmd::Inspect { task_id } => {
                    serde_json::json!({"id": "cli", "op": "repair", "action": "inspect", "task_id": task_id})
                }
                RepairCmd::Adopt { task_id } => {
                    serde_json::json!({"id": "cli", "op": "repair", "action": "adopt", "task_id": task_id})
                }
                RepairCmd::Rebind { task_id, handle } => {
                    serde_json::json!({"id": "cli", "op": "repair", "action": "rebind", "task_id": task_id, "handle": handle})
                }
                RepairCmd::Retry { task_id } => {
                    serde_json::json!({"id": "cli", "op": "repair", "action": "retry", "task_id": task_id})
                }
                RepairCmd::Fail { task_id, reason } => {
                    serde_json::json!({"id": "cli", "op": "repair", "action": "fail", "task_id": task_id, "reason": reason.unwrap_or_else(|| "operator repair fail".to_string())})
                }
                RepairCmd::Close { task_id } => {
                    serde_json::json!({"id": "cli", "op": "repair", "action": "close", "task_id": task_id})
                }
                RepairCmd::Ack { fault_id } => {
                    serde_json::json!({"id": "cli", "op": "repair", "action": "ack", "fault_id": fault_id})
                }
            };
            let v = orca::client_request(&root::swarm_sock(&root_p), req)?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            // The scheduler answered, so its answer decides: a refused action
            // must not look like a successful one to a shell script.
            if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::List { what, state, limit } => {
            let cwd = std::env::current_dir()?;
            let root_p = root::client_root(&cwd);
            let op = if what == "workspaces" {
                "list_workspaces"
            } else {
                "list_tasks"
            };
            let v = orca::client_request(
                &root::swarm_sock(&root_p),
                serde_json::json!({"id": "cli", "op": op, "state": state, "limit": limit}),
            )?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        Cmd::Tui => {
            let cwd = std::env::current_dir()?;
            let root_p = root::client_root(&cwd);
            tui::run_tui(&root::swarm_sock(&root_p))
        }
        Cmd::ShellCompletions { shell } => {
            shell_completions(shell);
            Ok(())
        }
        Cmd::Workspace { cmd } => {
            let cwd = std::env::current_dir()?;
            let root_p = root::ensure_root(&cwd, false)?;
            match cmd {
                WorkspaceCmd::Create | WorkspaceCmd::Sync => {
                    let report = sync::run_sync(&root_p)?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    Ok(())
                }
            }
        }
    }
}

fn shell_completions(shell: CompletionShell) {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    match shell {
        CompletionShell::Zsh => generate(Zsh, &mut cmd, name, &mut io::stdout()),
        CompletionShell::Fish => generate(Fish, &mut cmd, name, &mut io::stdout()),
    }
}

/// R4: double-fork + setsid, then exec ourselves with the child sentinel so
/// the grandchild re-enters main() already detached and continues the normal
/// `run` path (sync + serve). stdout/stderr go to `.onlyne/logs/scheduler.log`,
/// stdin to /dev/null. The launching shell prints and returns immediately.
#[cfg(unix)]
fn launch_detached(root_p: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    if let Some(p) = root::swarm_log(root_p).parent() {
        std::fs::create_dir_all(p)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root::swarm_log(root_p))?;
    let devnull = std::fs::OpenOptions::new().read(true).open("/dev/null")?;
    // fork #1: the parent returns to the shell; the child becomes a session
    // leader so no terminal signal (Ctrl-C/SIGHUP) reaches the grandchild.
    // SAFETY: fork in a single-threaded moment before tokio's worker threads
    // start (this runs at the top of main, before the runtime is driven).
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        anyhow::bail!("fork failed: {}", std::io::Error::last_os_error());
    }
    if pid > 0 {
        println!(
            "scheduler detached (pid file {}); follow {}",
            root::swarm_pid(root_p).display(),
            root::swarm_log(root_p).display()
        );
        return Ok(());
    }
    // Child: new session, then fork #2 so the session leader can exit and the
    // grandchild is reparented to init (never a controlling-terminal holder).
    unsafe {
        libc::setsid();
        let pid2 = libc::fork();
        if pid2 < 0 {
            libc::_exit(1);
        }
        if pid2 > 0 {
            libc::_exit(0);
        }
    }
    let exe = std::env::current_exe()?;
    let err = std::process::Command::new(exe)
        .arg("run")
        .env("ONLYNE_SWARM_DETACHED_CHILD", "1")
        .current_dir(root_p)
        .stdin(std::process::Stdio::from(devnull))
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log))
        .exec();
    // exec only returns on failure.
    Err(anyhow::anyhow!("exec detached scheduler failed: {err}"))
}

#[cfg(not(unix))]
fn launch_detached(_root_p: &std::path::Path) -> anyhow::Result<()> {
    anyhow::bail!("--detach is unix-only in this build")
}

/// True when the pid file names a live process. A missing or stale file is
/// `false` — the same answer for "never detached" and "crashed", which is
/// what `status`/`stop` need (adoption itself probes terminals, not pids).
fn proc_alive(root_p: &std::path::Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(root::swarm_pid(root_p)) else {
        return false;
    };
    let Ok(pid) = raw.trim().parse::<i32>() else {
        return false;
    };
    // kill(pid, 0): 0 = alive (or a zombie we cannot see); EPERM = alive but
    // not ours; ESRCH = gone.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// R4 `stop`: SIGTERM, wait up to 5s for the scheduler's own shutdown path
/// (it unlinks its socket and pid), then SIGKILL. The pid file and socket are
/// removed either way so the next `status` reports `no scheduler`.
fn stop_pid(root_p: &std::path::Path, pid: i32) -> anyhow::Result<()> {
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            anyhow::bail!("kill {pid}: {err}");
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if unsafe { libc::kill(pid, 0) } != 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        eprintln!("scheduler {pid} ignored SIGTERM; sending SIGKILL");
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    let _ = std::fs::remove_file(root::swarm_pid(root_p));
    let _ = std::fs::remove_file(root::swarm_sock(root_p));
    println!("scheduler {pid} stopped");
    Ok(())
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap_complete::shells::{Fish, Zsh};

    fn completion_text(shell: impl clap_complete::Generator) -> String {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        let mut out = Vec::new();
        generate(shell, &mut cmd, name, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn completion_command_is_exposed() {
        let cmd = Cli::command();
        assert!(cmd
            .get_subcommands()
            .any(|sc| sc.get_name() == "shell-completions"));
    }

    #[test]
    fn completions_include_swarm_commands() {
        let text = completion_text(Zsh);
        assert!(text.contains("export-skill"));
        assert!(text.contains("shell-completions"));
        assert!(!text.contains("--shell-completions"));
    }

    #[test]
    fn zsh_completion_mentions_swarm() {
        let text = completion_text(Zsh);
        assert!(text.contains("#compdef onlyne-swarm"));
    }

    /// `repair` is the operator door: all seven actions must be reachable from
    /// the CLI, and the shells must offer them.
    #[test]
    fn repair_exposes_every_action_the_scheduler_serves() {
        let cmd = Cli::command();
        let repair = cmd
            .get_subcommands()
            .find(|sc| sc.get_name() == "repair")
            .expect("repair subcommand");
        let actions: Vec<String> = repair
            .get_subcommands()
            .map(|sc| sc.get_name().to_string())
            .collect();
        for action in crate::repair::ACTIONS {
            assert!(
                actions.iter().any(|a| a == action),
                "{action} missing: {actions:?}"
            );
        }
        let rebind = repair
            .get_subcommands()
            .find(|sc| sc.get_name() == "rebind")
            .expect("rebind");
        assert!(
            rebind
                .get_arguments()
                .any(|a| a.get_long() == Some("handle")),
            "rebind is meaningless without the handle it binds"
        );
        let text = completion_text(Zsh);
        assert!(text.contains("rebind"), "shell completion lost repair");
    }

    #[test]
    fn detach_flag_and_stop_subcommand_exist() {
        // R4 §7: `run --detach` and `stop` are the process-model surface.
        let cmd = Cli::command();
        assert!(cmd.get_subcommands().any(|sc| sc.get_name() == "stop"));
        let run = cmd
            .get_subcommands()
            .find(|sc| sc.get_name() == "run")
            .expect("run subcommand");
        assert!(run.get_arguments().any(|a| a.get_long() == Some("detach")));
    }

    #[test]
    fn proc_alive_false_for_missing_and_stale_pid() {
        // A missing pid file and a dead pid both read as "no scheduler";
        // that is what lets `status` print the clean message instead of
        // an ECONNREFUSED from a leftover socket.
        let dir = tempfile::tempdir().unwrap();
        assert!(!proc_alive(dir.path()));
        std::fs::create_dir_all(dir.path().join(".onlyne/run")).unwrap();
        std::fs::write(root::swarm_pid(dir.path()), "999999999\n").unwrap();
        assert!(!proc_alive(dir.path()));
        // Our own pid is alive.
        std::fs::write(
            root::swarm_pid(dir.path()),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        assert!(proc_alive(dir.path()));
    }

    #[test]
    fn fish_completion_mentions_swarm() {
        let text = completion_text(Fish);
        assert!(text.contains("complete -c onlyne-swarm"));
    }
}
