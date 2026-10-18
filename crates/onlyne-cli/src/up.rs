//! `onlyne up`: one foreground process that supervises the cluster.
//!
//! It resolves the server root, spawns `onlyne-server run`, waits for the
//! admin socket to answer, then spawns the discovered role clients and the
//! optional IM gateway, all with piped output prefixed so an operator can tell
//! which daemon said the line. Ctrl-C becomes one ordered `shutdown` on the
//! admin socket with the `--drain-secs` bound; a child that dies on its own
//! stops the supervisor with exit 1.

use onlyne_config::layout::ServerRoot;
use onlyne_proto::{AdminOp, ShutdownArgs};
use onlyne_wire::socket::socket_path;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::flags::GlobalFlags;
use crate::runtime::{EXIT_ANSWER_FAILED, EXIT_NO_SIBLING, EXIT_OK, block_on};
use crate::wire::{self, AdminFrame};
use crate::{forward, runtime};

/// How long the supervisor waits for the server's admin socket to answer.
const SERVER_READY_SECS: u64 = 10;
/// One ready-poll step: a connect attempt plus the pause before the next one.
const POLL_MS: u64 = 250;

/// Flags for `onlyne up`.
#[derive(Debug, Clone, clap::Args)]
pub struct UpArgs {
    /// Server root. Falls back to `--server-root`, then discovery from the
    /// current directory.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Explicit role workspaces. When none is given, workspaces are
    /// auto-discovered under `<root>/<role>` and `<root>/.onlyne/ws/*/<role>`
    /// for the roles the spec declares.
    #[arg(long = "ws", visible_alias = "role-workspace")]
    pub workspaces: Vec<PathBuf>,
    /// IM gateway root. When omitted, `<root>/im` is used if it holds a
    /// `.onlyne/config.toml`.
    #[arg(long)]
    pub im_root: Option<PathBuf>,
    /// Drain seconds sent with the shutdown when stopping.
    #[arg(long, default_value_t = 2)]
    pub drain_secs: u64,
}

/// The workspaces for the declared roles, in declaration order.
///
/// A role is served from `<root>/<role>` when that directory holds
/// `.onlyne/config.toml`, otherwise from `<root>/.onlyne/ws/<topology>/<role>`
/// for the first topology whose copy does. Roles with neither layout are
/// skipped: `up` starts what exists, it does not provision.
pub fn discover_workspaces(
    server_root: &Path,
    declared_roles: &[String],
) -> Vec<(String, PathBuf)> {
    let ws_root = server_root.join(".onlyne/ws");
    let mut found = Vec::new();
    for role in declared_roles {
        let flat = server_root.join(role);
        if flat.join(".onlyne/config.toml").is_file() {
            found.push((role.clone(), flat));
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&ws_root) else {
            continue;
        };
        let mut topologies: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        topologies.sort();
        if let Some(path) = topologies
            .into_iter()
            .map(|topo| topo.join(role))
            .find(|path| path.join(".onlyne/config.toml").is_file())
        {
            found.push((role.clone(), path));
        }
    }
    found
}

/// Resolve the server root the flags and args name.
fn resolve_root(flags: &GlobalFlags, args: &UpArgs) -> Result<PathBuf, String> {
    if let Some(root) = &args.root {
        return Ok(root.clone());
    }
    if let Some(root) = &flags.server_root {
        return Ok(root.clone());
    }
    ServerRoot::discover(std::env::current_dir().map_err(|e| format!("cwd: {e}"))?)
        .map(|root| root.root().to_path_buf())
        .map_err(|e| format!("server root discovery: {e}"))
}

/// One line of a child's output, prefixed with which daemon said it.
async fn pipe_prefix(stream: tokio::process::ChildStdout, prefix: String) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        println!("{prefix}{line}");
    }
}

async fn pipe_prefix_err(stream: tokio::process::ChildStderr, prefix: String) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        eprintln!("{prefix}{line}");
    }
}

/// Spawn one daemon with piped output and a name the logs carry.
///
/// The spawned task owns the child for the process's lifetime: it reports the
/// exit through `exits`, and on `shutdown` it waits the drain window before
/// killing a child that will not leave on its own. Nothing here returns the
/// child, so reaping and supervision never fight over it.
async fn spawn_child(
    bin: &Path,
    args: &[String],
    prefix: String,
    drain: Duration,
    shutdown: tokio::sync::watch::Receiver<bool>,
    exits: &mpsc::Sender<(String, bool)>,
) -> Result<(), String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {} {prefix}: {e}", bin.display()))?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let name = prefix.trim_end().to_string();
    let err_prefix = prefix.clone();
    tokio::spawn(pipe_prefix(stdout, prefix));
    tokio::spawn(pipe_prefix_err(stderr, err_prefix));
    let exits = exits.clone();
    tokio::spawn(async move {
        let mut stopping = shutdown.clone();
        tokio::select! {
            status = child.wait() => {
                let clean = status.map(|s| s.success()).unwrap_or(false);
                let _ = exits.send((name, clean)).await;
            }
            _ = stopping.changed() => {
                // The server drains its clients, so give this child the drain
                // window before killing the stragglers.
                if drain.is_zero() || tokio::time::timeout(drain, child.wait()).await.is_err() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                let _ = exits.send((name, true)).await;
            }
        }
    });
    Ok(())
}

/// Whether the server's admin socket answers `status`.
async fn admin_answer(root: &Path) -> bool {
    let Ok(path) = socket_path(root) else {
        return false;
    };
    let Ok(mut stream) = wire::connect(&path, POLL_MS).await else {
        return false;
    };
    let frame = AdminFrame::Req {
        id: "up-ready".to_string(),
        op: AdminOp::Status(Value::Null),
    };
    wire::request_res(&mut stream, &frame, POLL_MS)
        .await
        .is_ok()
}

/// Wait up to `SERVER_READY_SECS` for the admin socket to answer.
async fn wait_server_ready(root: &Path, exit_rx: &mut mpsc::Receiver<(String, bool)>) -> bool {
    let steps = SERVER_READY_SECS * 1000 / POLL_MS;
    for _ in 0..steps {
        if admin_answer(root).await {
            return true;
        }
        if exit_rx.try_recv().is_ok() {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
    false
}

/// Send the ordered shutdown and leave the drain to the server.
async fn shutdown_server(root: &Path, drain_secs: u64) {
    let Ok(path) = socket_path(root) else {
        return;
    };
    let Ok(mut stream) = wire::connect(&path, 1_000).await else {
        return;
    };
    let frame = AdminFrame::Req {
        id: "up-shutdown".to_string(),
        op: AdminOp::Shutdown(ShutdownArgs {
            reason: "onlyne up stopping".to_string(),
            grace_ms: Some(drain_secs.saturating_mul(1000)),
        }),
    };
    let _ = wire::request_res(&mut stream, &frame, 1_000).await;
}

/// `onlyne up`: run the cluster in the foreground.
pub fn run(flags: &GlobalFlags, args: UpArgs) -> i32 {
    block_on(async move {
        let root = match resolve_root(flags, &args) {
            Ok(root) => root,
            Err(message) => return runtime::usage_error(format!("onlyne up: {message}")),
        };
        let spec_path = root.join(".onlyne/spec.toml");
        let spec = match onlyne_config::Spec::load(&spec_path) {
            Ok(spec) => spec,
            Err(e) => {
                return runtime::usage_error(format!("onlyne up: {}: {e}", spec_path.display()));
            }
        };

        let server_bin = match forward::resolve_sibling("onlyne-server") {
            Some(bin) => bin,
            None => {
                eprintln!("onlyne up: onlyne-server not found next to this binary or on PATH");
                return EXIT_NO_SIBLING;
            }
        };

        let roles: Vec<String> = spec.client.iter().map(|entry| entry.role.clone()).collect();
        let mut explicit_ws = args.workspaces.clone();
        if let Some(ws) = &flags.workspace {
            if !explicit_ws.contains(ws) {
                explicit_ws.push(ws.clone());
            }
        }
        let workspaces: Vec<PathBuf> = if explicit_ws.is_empty() {
            discover_workspaces(&root, &roles)
                .into_iter()
                .map(|(_, path)| path)
                .collect()
        } else {
            explicit_ws
        };

        let client_bin = if workspaces.is_empty() {
            None
        } else {
            match forward::resolve_sibling("onlyne-client") {
                Some(bin) => Some(bin),
                None => {
                    eprintln!("onlyne up: onlyne-client not found next to this binary or on PATH");
                    return EXIT_NO_SIBLING;
                }
            }
        };

        let im_root = match &args.im_root {
            Some(root) => Some(root.clone()),
            None => {
                let candidate = root.join("im");
                candidate
                    .join(".onlyne/config.toml")
                    .is_file()
                    .then_some(candidate)
            }
        };
        let im_bin = if im_root.is_some() {
            match forward::resolve_sibling("onlyne-im") {
                Some(bin) => Some(bin),
                None => {
                    eprintln!("onlyne up: onlyne-im not found next to this binary or on PATH");
                    return EXIT_NO_SIBLING;
                }
            }
        } else {
            None
        };

        let (exits, mut exit_rx) = mpsc::channel::<(String, bool)>(16);
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let drain = Duration::from_secs(args.drain_secs);
        let server_args = vec![
            "run".to_string(),
            "--root".to_string(),
            root.display().to_string(),
        ];
        if let Err(e) = spawn_child(
            &server_bin,
            &server_args,
            "[server] ".to_string(),
            drain,
            stop_rx.clone(),
            &exits,
        )
        .await
        {
            return runtime::usage_error(format!("onlyne up: {e}"));
        }

        if !wait_server_ready(&root, &mut exit_rx).await {
            eprintln!(
                "onlyne up: the server's admin socket did not answer within {SERVER_READY_SECS}s"
            );
            shutdown_server(&root, args.drain_secs).await;
            return EXIT_ANSWER_FAILED;
        }

        for ws in &workspaces {
            let ws_args = vec![
                "run".to_string(),
                "--workspace".to_string(),
                ws.display().to_string(),
            ];
            let prefix = format!(
                "[client:{}] ",
                ws.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            );
            if let Err(e) = spawn_child(
                client_bin.as_ref().expect("client resolved"),
                &ws_args,
                prefix,
                drain,
                stop_rx.clone(),
                &exits,
            )
            .await
            {
                eprintln!("onlyne up: {e}");
                let _ = stop_tx.send(true);
                shutdown_server(&root, args.drain_secs).await;
                return EXIT_ANSWER_FAILED;
            }
        }

        if let (Some(bin), Some(im)) = (im_bin, &im_root) {
            let im_args = vec![
                "run".to_string(),
                "--root".to_string(),
                im.display().to_string(),
            ];
            if let Err(e) = spawn_child(
                &bin,
                &im_args,
                "[im] ".to_string(),
                drain,
                stop_rx.clone(),
                &exits,
            )
            .await
            {
                eprintln!("onlyne up: {e}");
                let _ = stop_tx.send(true);
                shutdown_server(&root, args.drain_secs).await;
                return EXIT_ANSWER_FAILED;
            }
        }
        drop(exits);

        // Ctrl-C, or one child that died while the others are still up. The
        // reporters own every child: notify them, then wait until each has
        // reported (or exited), which is when reaping is over.
        let crashed = tokio::select! {
            _ = tokio::signal::ctrl_c() => None,
            exited = exit_rx.recv() => match exited {
                Some((name, false)) => Some(name),
                _ => None,
            },
        };
        let _ = stop_tx.send(true);

        shutdown_server(&root, args.drain_secs).await;

        // Every reporter sends one exit line, so the channel closes only when
        // every child is reaped. One child already exited before the notify;
        // that line was the one `recv` above answered, and the rest still come.
        while let Some(_exited) = exit_rx.recv().await {}
        match crashed {
            None => EXIT_OK,
            Some(name) => {
                eprintln!("onlyne up: {name} exited unexpectedly");
                1
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn role_ws(dir: &Path, role: &str) {
        fs::create_dir_all(dir.join(role).join(".onlyne")).unwrap();
        fs::write(dir.join(role).join(".onlyne/config.toml"), "").unwrap();
    }

    fn topo_ws(dir: &Path, topo: &str, role: &str) {
        fs::create_dir_all(dir.join(".onlyne/ws").join(topo).join(role).join(".onlyne")).unwrap();
        fs::write(
            dir.join(".onlyne/ws")
                .join(topo)
                .join(role)
                .join(".onlyne/config.toml"),
            "",
        )
        .unwrap();
    }

    #[test]
    fn finds_flat_role_workspaces() {
        let dir = std::env::temp_dir().join(format!("onlyne-up-flat-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        role_ws(&dir, "planner");
        let found = discover_workspaces(&dir, &["planner".to_string(), "ghost".to_string()]);
        assert_eq!(found, vec![("planner".to_string(), dir.join("planner"))]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_topologized_role_workspaces() {
        let dir = std::env::temp_dir().join(format!("onlyne-up-topo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        topo_ws(&dir, "solo", "planner");
        topo_ws(&dir, "pair", "writer");
        let found = discover_workspaces(
            &dir,
            &[
                "writer".to_string(),
                "planner".to_string(),
                "absent".to_string(),
            ],
        );
        assert_eq!(
            found,
            vec![
                ("writer".to_string(), dir.join(".onlyne/ws/pair/writer")),
                ("planner".to_string(), dir.join(".onlyne/ws/solo/planner")),
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
