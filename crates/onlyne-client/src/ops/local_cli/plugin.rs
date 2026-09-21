//! Plugin package verbs: install or remove a vendored coding-agent package,
//! then tell a running client that the package changed.

use super::config::{
    append_plugin_entry, config_lists_plugin, remove_plugin_entry, render_plugins,
};
use anyhow::{Context, Result, anyhow};
use onlyne_adapter::AdapterIo;
use onlyne_layout::RoleWorkspace;
use onlyne_layout::connect_local;
use onlyne_proto::{AdapterMsg, HelloArgs, MountKind, PROTOCOL_VERSION, PluginOp};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Wait bound for one handshake with a running client.
pub const CLIENT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Line an operator sees when a plugin change needs a client restart.
pub const RESTART_HINT: &str =
    "onlyne: no running client; restart `onlyne-client run` to mount this plugin";

/// Plugin id rule: the id becomes a path component under `agent/<id>/`.
pub fn valid_plugin_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    let first = bytes[0];
    let first_ok = first.is_ascii_lowercase() || first.is_ascii_digit();
    if !first_ok {
        return false;
    }
    bytes.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
    })
}

/// Directory holding one vendored coding-agent plugin package.
pub fn agent_package_dir(workspace: &Path, plugin_id: &str) -> PathBuf {
    RoleWorkspace::resolve(workspace).agent_dir(plugin_id)
}

/// Install a coding-agent plugin package into `agent/<id>/`.
pub fn agent_install(
    workspace: &Path,
    package: &Path,
    plugin_id: &str,
    agent: Option<&str>,
) -> Result<Vec<String>> {
    if !valid_plugin_id(plugin_id) {
        return Err(anyhow!("onlyne: invalid plugin id {plugin_id}"));
    }
    let target = agent_package_dir(workspace, plugin_id);
    if target.exists() {
        return Err(anyhow!("onlyne: plugin {plugin_id} already installed"));
    }
    // The config is the other half of "installed": a workspace that already
    // lists the id is installed even when its package directory is gone, so
    // the refusal lands before any filesystem change.
    if config_lists_plugin(workspace, plugin_id)? {
        return Err(anyhow!("onlyne: plugin {plugin_id} already installed"));
    }
    std::fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
    install_package_files(package, &target)?;
    let binary = find_plugin_binary(&target)?;
    set_executable(&target.join(&binary))?;
    let agent_name = agent.unwrap_or(plugin_id).to_string();
    let manifest = format!(
        "id = {plugin_id:?}\nbinary = {binary:?}\nagent = {agent_name:?}\ncapabilities = []\n"
    );
    std::fs::write(target.join("plugin.toml"), manifest)?;
    let ids = append_plugin_entry(workspace, plugin_id)?;
    Ok(vec![
        format!("installed {plugin_id}"),
        format!("wrote {}", target.join("plugin.toml").display()),
        format!("registered plugin {plugin_id} in {}", render_plugins(&ids)),
    ])
}

/// Remove a plugin package and its config entry.
pub fn agent_uninstall(workspace: &Path, plugin_id: &str) -> Result<Vec<String>> {
    if !valid_plugin_id(plugin_id) {
        return Err(anyhow!("onlyne: invalid plugin id {plugin_id}"));
    }
    let target = agent_package_dir(workspace, plugin_id);
    if !target.exists() {
        return Err(anyhow!("onlyne: no plugin {plugin_id} installed"));
    }
    remove_plugin_entry(workspace, plugin_id)?;
    std::fs::remove_dir_all(&target).with_context(|| format!("remove {}", target.display()))?;
    Ok(vec![
        format!("deregistered plugin {plugin_id} from plugins"),
        format!("removed {}", target.display()),
    ])
}

/// Frame the client would send to hot-mount a plugin over its local socket.
pub fn agent_mount_frame(plugin_id: &str) -> serde_json::Value {
    serde_json::json!({"f": "req", "id": plugin_id, "op": "control", "args": {"plugin": plugin_id, "action": "mount"}})
}

/// Frame the client would send to stop a mounted plugin.
pub fn agent_stop_frame(plugin_id: &str) -> serde_json::Value {
    serde_json::json!({"f": "req", "id": plugin_id, "op": "control", "args": {"plugin": plugin_id, "action": "stop"}})
}

/// What one plugin verb did, split by the stream each line belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginAction {
    /// Lines for stdout, one per filesystem action.
    pub lines: Vec<String>,
    /// Line for stderr when no client answered the local socket.
    pub hint: Option<String>,
}

/// Install a package, then tell a running client about it.
pub async fn install_verb(
    workspace: &Path,
    package: &Path,
    plugin_id: &str,
    agent: Option<&str>,
) -> Result<PluginAction> {
    let lines = agent_install(workspace, package, plugin_id, agent)?;
    let running = notify_client(workspace, plugin_id).await;
    Ok(PluginAction {
        lines,
        hint: (!running).then(|| RESTART_HINT.to_string()),
    })
}

/// Remove a package and its config entry, then tell a running client.
pub async fn uninstall_verb(workspace: &Path, plugin_id: &str) -> Result<PluginAction> {
    let lines = agent_uninstall(workspace, plugin_id)?;
    let running = notify_client(workspace, plugin_id).await;
    Ok(PluginAction {
        lines,
        hint: (!running).then(|| RESTART_HINT.to_string()),
    })
}

/// Operator refusal for a plugin verb.
pub fn plugin_exit_code(error: &anyhow::Error) -> i32 {
    if error.to_string().starts_with("onlyne: ") {
        2
    } else {
        1
    }
}

/// Tell a running client that a plugin package changed.
///
/// The adapter socket has one host-side entry point, the `hello` handshake, so
/// the notification is that handshake on an `admin` mount. `false` means no
/// client is listening on the workspace socket.
pub async fn notify_client(workspace: &Path, plugin_id: &str) -> bool {
    let socket = RoleWorkspace::resolve(workspace).socket_path();
    let Ok(stream) = connect_local(&socket).await else {
        return false;
    };
    let io = AdapterIo::new(stream, CLIENT_PROBE_TIMEOUT, CLIENT_PROBE_TIMEOUT);
    let hello = HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: format!("onlyne-client-cli:{plugin_id}"),
        version: env!("CARGO_PKG_VERSION").to_string(),
        kind: MountKind::Admin,
        capabilities: Vec::new(),
        mount: None,
    };
    matches!(io.request(AdapterMsg::Plugin(PluginOp::Hello(hello))).await, Ok(body) if body.ok)
}

fn install_package_files(package: &Path, target: &Path) -> Result<()> {
    if package.is_dir() {
        let entries: Vec<_> = std::fs::read_dir(package)
            .with_context(|| format!("read {}", package.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for entry in &entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let source = entry.path();
            let destination = target.join(&name);
            if source.is_dir() {
                return Err(anyhow!("onlyne: package directory must be flat"));
            }
            std::fs::copy(&source, &destination).with_context(|| format!("copy {}", name))?;
        }
        return Ok(());
    }
    if package.extension().and_then(|ext| ext.to_str()) == Some("gz") {
        let file =
            std::fs::File::open(package).with_context(|| format!("open {}", package.display()))?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive
            .unpack(target)
            .with_context(|| format!("unpack {}", package.display()))?;
        return Ok(());
    }
    Err(anyhow!(
        "onlyne: package must be a directory or a .tar.gz file"
    ))
}

fn find_plugin_binary(target: &Path) -> Result<String> {
    let mut binaries = Vec::new();
    for entry in std::fs::read_dir(target).with_context(|| format!("read {}", target.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("onlyne-agent-") && entry.file_type()?.is_file() {
            binaries.push(name);
        }
    }
    binaries.sort();
    if binaries.len() == 1 {
        Ok(binaries.remove(0))
    } else {
        Err(anyhow!(
            "onlyne: package must contain exactly one onlyne-agent-* executable"
        ))
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)
        .with_context(|| format!("chmod {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
