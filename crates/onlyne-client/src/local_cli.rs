use crate::intent::IntentMachine;
use anyhow::{Context, Result, anyhow};
use onlyne_adapter::AdapterIo;
use onlyne_layout::RoleWorkspace;
use onlyne_proto::{
    AdapterMsg, ClientOp, ControlArgs, Envelope, HelloArgs, HistoryArgs, LedgerQuery, MountKind,
    PROTOCOL_VERSION, PluginOp, QueryFaultsArgs, QueryRolesArgs, QuerySessionsArgs, ResBody,
    Subscribe,
};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;

/// Wait bound for one handshake with a running client.
pub const CLIENT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Line an operator sees when a plugin change needs a client restart.
pub const RESTART_HINT: &str =
    "onlyne: no running client; restart `onlyne-client run` to mount this plugin";

/// Local CLI handlers share the intent queue with plugin-originated sends.
#[derive(Clone)]
pub struct LocalCli {
    pub intents: IntentMachine,
    pub role: String,
}

impl LocalCli {
    pub fn new(intents: IntentMachine) -> Self {
        let role = intents
            .store
            .config("role")
            .ok()
            .flatten()
            .unwrap_or_default();
        Self { intents, role }
    }

    pub fn with_role(intents: IntentMachine, role: impl Into<String>) -> Self {
        Self {
            intents,
            role: role.into(),
        }
    }

    pub fn send(&self, envelope: Envelope) -> Result<ClientOp> {
        self.intents.enqueue(&envelope)?;
        Ok(ClientOp::Send(Box::new(envelope)))
    }

    pub fn reply(&self, envelope: Envelope) -> Result<ClientOp> {
        self.send(envelope)
    }
    pub fn complete(&self, envelope: Envelope) -> Result<ClientOp> {
        self.send(envelope)
    }
    pub fn handoff(&self, envelope: Envelope) -> Result<ClientOp> {
        self.send(envelope)
    }
    pub fn control(&self, args: ControlArgs) -> ClientOp {
        ClientOp::Control(args)
    }
    pub fn query_sessions(&self, args: QuerySessionsArgs) -> ClientOp {
        ClientOp::QuerySessions(args)
    }

    /// Build the server query shape for callers that need a wire request.
    pub fn query_roles(&self, args: QueryRolesArgs) -> ClientOp {
        ClientOp::QueryRoles(args)
    }

    /// Answer the role query from durable local prose cache. `QueryRolesArgs`
    /// is the only role-query type in onlyne-proto and has the field `role`.
    pub fn query_roles_local(&self, args: &QueryRolesArgs) -> Result<ResBody> {
        let role = args.role.as_deref().unwrap_or(&self.role);
        let Some((prose, spec_hash)) = self.intents.store.prose(role)? else {
            return Ok(ResBody::ok(serde_json::json!({"roles": []})));
        };
        Ok(ResBody::ok(serde_json::json!({
            "roles": [{"name": role, "role": role, "prose": prose, "spec_hash": spec_hash}]
        })))
    }

    /// Client-surface export used by `cluster export-prose`.
    pub fn export_prose(&self) -> Result<ResBody> {
        let query = QueryRolesArgs {
            role: Some(self.role.clone()),
        };
        self.query_roles_local(&query)
    }

    pub fn query_ledger(&self, args: LedgerQuery) -> ClientOp {
        ClientOp::QueryLedger(args)
    }
    pub fn subscribe(&self, args: Subscribe) -> ClientOp {
        ClientOp::Subscribe(args)
    }
    pub fn history(&self, args: HistoryArgs) -> Result<ClientOp> {
        if args.limit == 0 {
            return Err(anyhow!("history limit must be positive"));
        }
        Ok(ClientOp::QueryFaults(QueryFaultsArgs {
            role: None,
            task_id: args.task_id,
            kind: args.kind,
            open_only: false,
            limit: args.limit,
        }))
    }

    pub fn offline_send(&self, envelope: &Envelope) -> Result<ResBody> {
        self.intents.enqueue(envelope)?;
        Ok(ResBody::ok(
            serde_json::json!({"queued": true, "op_id": envelope.op_id}),
        ))
    }

    pub async fn handle(&self, message: AdapterMsg) -> Result<ResBody> {
        match message {
            AdapterMsg::Plugin(PluginOp::Send(envelope)) => self.offline_send(&envelope),
            AdapterMsg::Plugin(PluginOp::Detach(_)) => Ok(ResBody::ok(serde_json::Value::Null)),
            AdapterMsg::Plugin(PluginOp::Hello(_)) => Ok(ResBody::err(
                onlyne_proto::ErrorCode::Invalid,
                "hello already completed",
                Some("op".into()),
            )),
            _ => Ok(ResBody::err(
                onlyne_proto::ErrorCode::UnknownOp,
                "unsupported local cli operation",
                Some("op".into()),
            )),
        }
    }
}

pub fn map_send(envelope: Envelope) -> Result<ClientOp> {
    envelope.validate().map_err(|e| anyhow!(e.to_string()))?;
    Ok(ClientOp::Send(Box::new(envelope)))
}
pub fn map_reply(envelope: Envelope) -> Result<ClientOp> {
    map_send(envelope)
}
pub fn map_complete(envelope: Envelope) -> Result<ClientOp> {
    map_send(envelope)
}
pub fn map_handoff(envelope: Envelope) -> Result<ClientOp> {
    map_send(envelope)
}
pub fn map_control(args: ControlArgs) -> ClientOp {
    ClientOp::Control(args)
}
pub fn map_query_sessions(args: QuerySessionsArgs) -> ClientOp {
    ClientOp::QuerySessions(args)
}
pub fn map_query_roles(args: QueryRolesArgs) -> ClientOp {
    ClientOp::QueryRoles(args)
}
pub fn map_query_ledger(args: LedgerQuery) -> ClientOp {
    ClientOp::QueryLedger(args)
}
pub fn map_subscribe(args: Subscribe) -> ClientOp {
    ClientOp::Subscribe(args)
}
pub fn map_history(args: HistoryArgs) -> ClientOp {
    ClientOp::QueryFaults(QueryFaultsArgs {
        role: None,
        task_id: args.task_id,
        kind: args.kind,
        open_only: false,
        limit: args.limit,
    })
}

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
    std::fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
    install_package_files(package, &target)?;
    let binary = find_plugin_binary(&target)?;
    set_executable(&target.join(&binary))?;
    let agent_name = agent.unwrap_or(plugin_id).to_string();
    let manifest = format!(
        "id = {plugin_id:?}\nbinary = {binary:?}\nagent = {agent_name:?}\ncapabilities = []\n"
    );
    std::fs::write(target.join("plugin.toml"), manifest)?;
    append_plugin_entry(workspace, plugin_id)?;
    Ok(vec![
        format!("installed {plugin_id}"),
        format!("wrote {}", target.join("plugin.toml").display()),
        format!("appended [[plugin]] {plugin_id}"),
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
        format!("removed [[plugin]] {plugin_id}"),
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
    let Ok(stream) = UnixStream::connect(&socket).await else {
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

fn append_plugin_entry(workspace: &Path, plugin_id: &str) -> Result<()> {
    let config = RoleWorkspace::resolve(workspace).config_path();
    let mut text = std::fs::read_to_string(&config).unwrap_or_default();
    if !text.ends_with('\n') && !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!("[[plugin]]\nid = {plugin_id:?}\n"));
    std::fs::write(&config, text).with_context(|| format!("write {}", config.display()))?;
    Ok(())
}

fn remove_plugin_entry(workspace: &Path, plugin_id: &str) -> Result<()> {
    let config = RoleWorkspace::resolve(workspace).config_path();
    let text =
        std::fs::read_to_string(&config).with_context(|| format!("read {}", config.display()))?;
    let mut output = String::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() == "[[plugin]]" {
            let mut block = vec![line.to_string()];
            while let Some(next) = lines.peek() {
                if next.trim().starts_with('[') {
                    break;
                }
                block.push(lines.next().unwrap().to_string());
            }
            let owns = block
                .iter()
                .any(|entry| entry.trim() == format!("id = {plugin_id:?}"));
            if owns {
                continue;
            }
            for entry in block {
                output.push_str(&entry);
                output.push('\n');
            }
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    std::fs::write(&config, output).with_context(|| format!("write {}", config.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn role_query_reads_cached_prose_after_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("client.db");
        let store = onlyne_store::ClientStore::open(&path).unwrap();
        store
            .put_prose("planner", "cluster b exposes planner", "hash-b")
            .unwrap();
        store.put_config("role", "planner").unwrap();
        let machine = IntentMachine::new(store, 3, vec![1]);
        let cli = LocalCli::new(machine);
        let value = cli.export_prose().unwrap();
        assert_eq!(
            value.data.unwrap()["roles"][0]["prose"],
            "cluster b exposes planner"
        );
        drop(cli);
        let reopened = onlyne_store::ClientStore::open(&path).unwrap();
        assert_eq!(
            reopened.prose("planner").unwrap(),
            Some(("cluster b exposes planner".into(), "hash-b".into()))
        );
    }
}

#[cfg(test)]
mod agent_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn agent_install_and_uninstall_round_trip() {
        let workspace = tempdir().unwrap();
        let package = tempdir().unwrap();
        std::fs::write(
            package.path().join("onlyne-agent-demo"),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        let lines =
            agent_install(workspace.path(), package.path(), "demo", Some("demo-agent")).unwrap();
        assert_eq!(lines.len(), 3);
        let target = agent_package_dir(workspace.path(), "demo");
        assert!(target.join("onlyne-agent-demo").exists());
        assert!(target.join("plugin.toml").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(target.join("onlyne-agent-demo"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o755);
        }
        let config = std::fs::read_to_string(workspace.path().join(".onlyne/config.toml")).unwrap();
        assert!(config.contains("[[plugin]]"));
        let removed = agent_uninstall(workspace.path(), "demo").unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!target.exists());
    }

    #[test]
    fn agent_bad_id_touches_no_path() {
        let workspace = tempdir().unwrap();
        let package = tempdir().unwrap();
        let result = agent_install(workspace.path(), package.path(), "Bad/Id", None);
        assert!(result.is_err());
        assert!(!workspace.path().join(".onlyne/agent").exists());
    }

    #[test]
    fn agent_missing_id_reports_not_installed() {
        let workspace = tempdir().unwrap();
        let result = agent_uninstall(workspace.path(), "ghost");
        assert_eq!(
            result.unwrap_err().to_string(),
            "onlyne: no plugin ghost installed"
        );
    }

    #[test]
    fn agent_mount_frame_names_plugin() {
        let frame = agent_mount_frame("demo");
        assert_eq!(frame["args"]["plugin"], "demo");
        assert_eq!(frame["args"]["action"], "mount");
        let stop = agent_stop_frame("demo");
        assert_eq!(stop["args"]["plugin"], "demo");
        assert_eq!(stop["args"]["action"], "stop");
    }
}
