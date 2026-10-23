//! The tools mount an ACP session is handed at `session/new`.
//!
//! An ACP session has no plugin connection — this client spawned the agent
//! itself, so nothing on the adapter socket carries the session's obligations
//! (`docs/v2-CONTRACT.md` §3b). `onlyne mcp` is that connection: the agent
//! starts it from the mount below, the process dials this client's adapter
//! socket, and it presents the token as the session it speaks for. The token is
//! a capability, so it travels in this mount's own environment and nowhere the
//! agent or the model can read it: `SpawnSpec.tools_token` is the client's
//! mint, and the agent child's environment never carries it.

use crate::backend::SpawnSpec;
use anyhow::{Result, bail};
use onlyne_acp::{EnvVariable, McpServer};
use onlyne_config::layout::RoleWorkspace;
use std::path::{Path, PathBuf};

/// The name the mount answers to and the CLI it runs.
const TOOLS_NAME: &str = "onlyne";
/// The one verb the mount runs: the MCP bridge's stdio face.
const TOOLS_ARGS: [&str; 1] = ["mcp"];

/// The stdio mount one ACP session is opened with.
///
/// The command is resolved here rather than left to the agent, because an agent
/// that resolved `onlyne` again on its own PATH could land on a different build
/// than the client it is talking to (`docs/v2-CONTRACT.md` §3b). The env is the
/// only place the token is written.
pub(super) fn mount(spec: &SpawnSpec) -> Result<McpServer> {
    if spec.tools_token.is_empty() {
        bail!(
            "acp: task {} was spawned without a tools token; the client mints one for every \
             session it opens, and without it `onlyne mcp` could not speak for this one \
             (`SpawnSpec.tools_token`)",
            spec.task_id
        );
    }
    let command = entrypoint(spec)?;
    let socket = socket_of(spec);
    Ok(McpServer::Stdio {
        name: TOOLS_NAME.to_string(),
        command: command.to_string_lossy().into_owned(),
        args: TOOLS_ARGS.iter().map(|arg| arg.to_string()).collect(),
        env: vec![
            EnvVariable {
                name: "ONLYNE_SOCKET".to_string(),
                value: socket.to_string_lossy().into_owned(),
            },
            EnvVariable {
                name: "ONLYNE_MCP_TOKEN".to_string(),
                value: spec.tools_token.clone(),
            },
        ],
    })
}

/// The adapter socket the mount dials, as this session's own environment names
/// it.
///
/// One tree answers both halves of a spawn: the workspace the session runs in
/// resolves to the endpoint this client serves. A session whose workspace
/// resolves to a short endpoint is handed the served path directly, and that
/// spelling wins when it is there, exactly as it does for the agent child
/// (`session::dispatch::env`).
fn socket_of(spec: &SpawnSpec) -> PathBuf {
    match spec.env.get("ONLYNE_SOCKET") {
        Some(served) if !served.is_empty() => PathBuf::from(served),
        _ => RoleWorkspace::resolve(&spec.cwd).socket_path(),
    }
}

/// The `onlyne` entrypoint the mount is commanded with.
///
/// Beside this client's own binary first, then along the `PATH` of the process
/// that will run it — the agent child's, which is the session's own environment
/// when it names one. Beside this binary is the authoritative answer on a
/// machine with several builds installed, so it wins; the PATH probe is the
/// second answer, for a client started from a wrapper that keeps the CLI
/// somewhere else. Nothing is found: the spawn is refused by name rather than
/// handed on, because a command the agent cannot start is a session whose
/// obligations have no carrier at all.
fn entrypoint(spec: &SpawnSpec) -> Result<PathBuf> {
    if let Some(found) = beside_exe() {
        return Ok(found);
    }
    if let Some(found) = on_path(spec) {
        return Ok(found);
    }
    bail!(
        "{} — an ACP session is handed `onlyne mcp` as its tools mount, so the CLI has to sit \
         beside this client's binary or on the PATH the agent runs with",
        onlyne_proto::binary_not_found(TOOLS_NAME)
    )
}

fn beside_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    candidates_in(dir, TOOLS_NAME)
        .into_iter()
        .find(|candidate| is_executable_file(candidate))
}

fn on_path(spec: &SpawnSpec) -> Option<PathBuf> {
    let path = match spec.env.get("PATH") {
        Some(path) => path.clone(),
        None => std::env::var("PATH").ok()?,
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() || !dir.is_dir() {
            continue;
        }
        if let Some(found) = candidates_in(&dir, TOOLS_NAME)
            .into_iter()
            .find(|candidate| is_executable_file(candidate))
        {
            return Some(found);
        }
    }
    None
}

/// A file that exists and can be launched as the mount's command.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The names one directory is probed for.
///
/// The same spelling the CLI's own sibling resolution uses, for the same
/// reason: a machine that has one of these has the daemon.
fn candidates_in(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut names = vec![dir.join(name), dir.join(format!("{name}.exe"))];
    if cfg!(windows) {
        names.push(dir.join(format!("{name}.cmd")));
        names.push(dir.join(format!("{name}.bat")));
    }
    names
}
