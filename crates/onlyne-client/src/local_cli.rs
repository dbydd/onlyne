use crate::intent::{IntentMachine, stamp_op_id};
use anyhow::{Context, Result, anyhow};
use onlyne_adapter::AdapterIo;
use onlyne_layout::RoleWorkspace;
use onlyne_layout::connect_local;
use onlyne_proto::{
    AdapterMsg, ClientOp, ControlArgs, Envelope, HelloArgs, HistoryArgs, LedgerQuery, MountKind,
    PROTOCOL_VERSION, PluginOp, QueryFaultsArgs, QueryRolesArgs, QuerySessionsArgs, ResBody,
    Subscribe,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

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

    /// Queue one outbound send and answer the stamped envelope.
    ///
    /// The queue keys its row with an `op_id`; a note arrives without one, so
    /// the stamp lands before the envelope becomes the `ClientOp` the caller
    /// writes, and the frame on the wire carries the id the row stored.
    pub fn send(&self, mut envelope: Envelope) -> Result<ClientOp> {
        stamp_op_id(&mut envelope);
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

    /// Queue one plugin `send` and answer the key its intent row carries.
    ///
    /// A note arrives without an `op_id`, so the stamp happens before the
    /// reply is built: the plugin reads the id the queue stored rather than
    /// `null`, and the stored envelope is the one that replays.
    pub fn offline_send(&self, envelope: &Envelope) -> Result<ResBody> {
        let mut stamped = envelope.clone();
        let op_id = stamp_op_id(&mut stamped);
        self.intents.enqueue(&stamped)?;
        Ok(ResBody::ok(
            serde_json::json!({"queued": true, "op_id": op_id}),
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

/// The top-level `plugins` array line, its parsed ids, and the text the file
/// keeps after the closing bracket (a trailing comment or the line terminator).
struct PluginsLine {
    index: usize,
    ids: Vec<String>,
    tail: String,
}

/// The first top-level `plugins` assignment and any duplicate assignments whose
/// ids must fold into it.
struct PluginsEdit {
    primary: PluginsLine,
    duplicates: Vec<usize>,
}

/// Locate the top-level `plugins = [...]` assignment. TOML binds every key
/// after a table header to that table, so only lines before the first header
/// are the client's own. Duplicate top-level assignments merge their ids onto
/// the first line; a `plugins` line whose value is not a single-line array of
/// plain quoted strings is refused, never guessed at.
fn find_plugins(text: &str) -> Result<Option<PluginsEdit>> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let key = line.trim_end_matches('\r').trim_start();
        if key.is_empty() || key.starts_with('#') {
            continue;
        }
        if key.starts_with('[') {
            break;
        }
        let Some(value) = key_value(line, "plugins") else {
            continue;
        };
        let (ids, tail) = parse_plugins_value(value, index)?;
        found.push(PluginsLine { index, ids, tail });
    }
    let mut found = found.into_iter();
    let Some(mut primary) = found.next() else {
        return Ok(None);
    };
    let mut duplicates = Vec::new();
    for duplicate in found {
        for id in duplicate.ids {
            if !primary.ids.contains(&id) {
                primary.ids.push(id);
            }
        }
        duplicates.push(duplicate.index);
    }
    Ok(Some(PluginsEdit {
        primary,
        duplicates,
    }))
}

/// Parse one physical `plugins` value, naming the line whenever its shape is
/// outside the closed, single-line form this module can safely rewrite.
fn parse_plugins_value(value: &str, index: usize) -> Result<(Vec<String>, String)> {
    parse_inline_array(value).map_err(|error| anyhow!("{error} (line {})", index + 1))
}

/// Split a `name = value` line into the value text when `name` matches.
fn key_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let key = line.trim_end_matches('\r').trim_start();
    let rest = key.strip_prefix(name)?.trim_start();
    let value = rest.strip_prefix('=')?.trim_start();
    Some(value)
}

/// Parse a one-line inline array of quoted strings. Everything after the
/// closing bracket must be empty or a comment; the returned tail keeps it.
fn parse_inline_array(value: &str) -> Result<(Vec<String>, String)> {
    let value = value.trim_end();
    let open = value.strip_prefix('[').ok_or_else(|| {
        anyhow!("onlyne: config.toml keeps `plugins` in a shape this verb cannot edit; write it as plugins = [\"id\"]")
    })?;
    let close = open.rfind(']').ok_or_else(|| {
        anyhow!(
            "onlyne: config.toml splits the `plugins` array across lines; put every id on one line"
        )
    })?;
    let tail = open[close + 1..].to_string();
    if !tail.trim().is_empty() && !tail.trim().starts_with('#') {
        return Err(anyhow!(
            "onlyne: config.toml has trailing text on the `plugins` line: {value}"
        ));
    }
    let ids = open[..close]
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            let bare = item.strip_prefix('"')?.strip_suffix('"')?;
            if bare.contains(['\\', '"']) {
                return None;
            }
            Some(bare.to_string())
        })
        .collect::<Option<Vec<String>>>()
        .ok_or_else(|| {
            anyhow!("onlyne: config.toml `plugins` array holds a value this verb cannot edit")
        })?;
    Ok((ids, tail))
}

/// The inline-array spelling this module always writes.
fn render_array(ids: &[String]) -> String {
    let items: Vec<String> = ids.iter().map(|id| format!("{id:?}")).collect();
    format!("[{}]", items.join(", "))
}

/// The config line the plugin verbs report to the operator.
fn render_plugins(ids: &[String]) -> String {
    format!("plugins = {}", render_array(ids))
}

/// Rewrite the top-level `plugins` array line in place, or insert a fresh one
/// after `key_path` (falling back to just before the first table header, then
/// to the end of the file). Every other line keeps its exact bytes.
fn set_plugins(text: &str, ids: &[String]) -> Result<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let body = render_plugins(ids);
    match find_plugins(text)? {
        Some(edit) => {
            let old = &lines[edit.primary.index];
            let indent = &old[..old.len() - old.trim_start().len()];
            lines[edit.primary.index] = format!("{indent}{body}{}", edit.primary.tail);
            for duplicate in edit.duplicates.iter().rev() {
                lines.remove(*duplicate);
            }
        }
        None => {
            // Only lines before the first table header are top-level, so a
            // table that happens to hold a `key_path` key cannot mislead the
            // insertion point.
            let first_header = lines
                .iter()
                .position(|line| line.trim_end_matches('\r').trim_start().starts_with('['))
                .unwrap_or(lines.len());
            let insert_at = lines[..first_header]
                .iter()
                .position(|line| key_value(line, "key_path").is_some())
                .map_or(first_header, |index| index + 1);
            lines.insert(insert_at, body);
        }
    }
    let mut out = lines.join("\n");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Recognize a legacy plugin table header even when it uses spaces or carries
/// a trailing comment. The header line itself contains no quoted value, so the
/// first `#` is unambiguously the start of a comment.
fn is_plugin_table_header(line: &str) -> bool {
    let line = line.trim_end_matches('\r').trim_start();
    let line = match line.split_once('#') {
        Some((header, _)) => header.trim_end(),
        None => line,
    };
    let Some(inner) = line
        .strip_prefix("[[")
        .and_then(|line| line.strip_suffix("]]"))
    else {
        return false;
    };
    inner.trim() == "plugin"
}

/// Fold `[[plugin]]` blocks (the table form the 1.2.1 installer wrote, which
/// `ClientConfig` rejects) out of a config text. `only` restricts the fold to
/// blocks naming that id; every other block stays byte-for-byte. A block that
/// carries no parsable `id` line names no plugin, so it stays too and the
/// operator keeps seeing the loader's refusal.
fn fold_plugin_blocks(text: &str, only: Option<&str>) -> (String, Vec<String>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let ends_with_newline = text.ends_with('\n');
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut ids: Vec<String> = Vec::new();
    let mut folded = 0usize;
    let mut cursor = 0;
    while cursor < lines.len() {
        if !is_plugin_table_header(lines[cursor]) {
            kept.push(lines[cursor]);
            cursor += 1;
            continue;
        }
        let start = cursor;
        cursor += 1;
        while cursor < lines.len()
            && !lines[cursor]
                .trim_end_matches('\r')
                .trim_start()
                .starts_with('[')
        {
            cursor += 1;
        }
        let block_id = block_id_value(&lines[start + 1..cursor]);
        let owned = match (only, &block_id) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(wanted), Some(id)) => id == wanted,
        };
        if owned {
            folded += 1;
            let id = block_id.unwrap();
            if !ids.contains(&id) {
                ids.push(id);
            }
        } else {
            kept.extend_from_slice(&lines[start..cursor]);
        }
    }
    let mut out = kept.join("\n");
    if ends_with_newline && !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    (out, ids, folded)
}

/// The `id = "..."` entry inside one `[[plugin]]` block.
fn block_id_value(block: &[&str]) -> Option<String> {
    block.iter().find_map(|line| {
        let value = key_value(line, "id")?.trim_end();
        let bare = value.strip_prefix('"')?.strip_suffix('"')?;
        if bare.contains(['\\', '"']) {
            return None;
        }
        Some(bare.to_string())
    })
}

/// Replace a file through a sibling temp name so a half-written config never
/// survives a crash.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let name = path
        .file_name()
        .map(|name| format!("{}.tmp", name.to_string_lossy()))
        .unwrap_or_else(|| "onlyne-config.tmp".to_string());
    let tmp = path.with_file_name(name);
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Whether the workspace config already registers this plugin id. A workspace
/// with no config file registers nothing.
fn config_lists_plugin(workspace: &Path, plugin_id: &str) -> Result<bool> {
    let config = RoleWorkspace::resolve(workspace).config_path();
    let Ok(text) = std::fs::read_to_string(&config) else {
        return Ok(false);
    };
    Ok(find_plugins(&text)?.is_some_and(|edit| edit.primary.ids.iter().any(|id| id == plugin_id)))
}

/// Register `plugin_id` in the workspace `plugins` array and return the ids
/// the file lists afterwards. An id the merged array already holds changes the
/// registered set; duplicate top-level lines still fold onto the first line.
fn append_plugin_entry(workspace: &Path, plugin_id: &str) -> Result<Vec<String>> {
    let config = RoleWorkspace::resolve(workspace).config_path();
    let text = std::fs::read_to_string(&config).unwrap_or_default();
    let edit = find_plugins(&text)?;
    let has_duplicates = edit
        .as_ref()
        .is_some_and(|edit| !edit.duplicates.is_empty());
    let mut ids = edit.map(|edit| edit.primary.ids).unwrap_or_default();
    let already_registered = ids.iter().any(|id| id == plugin_id);
    if !already_registered {
        ids.push(plugin_id.to_string());
    }
    if !already_registered || has_duplicates {
        let updated = set_plugins(&text, &ids)?;
        write_atomic(&config, &updated)?;
    }
    Ok(ids)
}

/// Deregister `plugin_id`: drop it from the `plugins` array and fold away any
/// leftover `[[plugin]]` block naming it, so uninstalling a 1.2.1-era entry
/// still recovers a workspace that never restarted the client.
fn remove_plugin_entry(workspace: &Path, plugin_id: &str) -> Result<()> {
    let config = RoleWorkspace::resolve(workspace).config_path();
    let text =
        std::fs::read_to_string(&config).with_context(|| format!("read {}", config.display()))?;
    let (body, _ids, _folded) = fold_plugin_blocks(&text, Some(plugin_id));
    let Some(edit) = find_plugins(&body)? else {
        if body != text {
            write_atomic(&config, &body)?;
        }
        return Ok(());
    };
    if !edit.primary.ids.iter().any(|id| id == plugin_id) {
        // The array holds nothing, but a legacy block naming this plugin may
        // have been folded above; that removal still has to land.
        if body != text {
            write_atomic(&config, &body)?;
        }
        return Ok(());
    }
    let kept: Vec<String> = edit
        .primary
        .ids
        .into_iter()
        .filter(|id| id != plugin_id)
        .collect();
    let updated = set_plugins(&body, &kept)?;
    write_atomic(&config, &updated)
}

/// Whether the top-level part of the config repeats `plugins`. This check only
/// recognizes the key; value safety stays with the parser that will rewrite it.
fn has_duplicate_plugins_lines(text: &str) -> bool {
    let mut seen = false;
    for line in text.lines() {
        let key = line.trim_end_matches('\r').trim_start();
        if key.is_empty() || key.starts_with('#') {
            continue;
        }
        if key.starts_with('[') {
            break;
        }
        if key_value(line, "plugins").is_some() {
            if seen {
                return true;
            }
            seen = true;
        }
    }
    false
}

/// Self-heal a workspace whose `config.toml` still carries the `[[plugin]]`
/// tables the 1.2.1 installer appended: merge their ids into the top-level
/// `plugins` array, drop the blocks, and rewrite the file atomically. Duplicate
/// top-level `plugins` lines merge onto the first line even when no legacy
/// block remains. Returns the number of folded blocks; `0` means no block was
/// folded, so an unrelated parse failure keeps reporting its own serde error.
/// A `plugins` array this module refuses to edit aborts the fold without
/// writing, and the loader's refusal then reaches the operator.
pub fn migrate_plugin_blocks(workspace: &Path) -> Result<usize> {
    let config = RoleWorkspace::resolve(workspace).config_path();
    let Ok(text) = std::fs::read_to_string(&config) else {
        return Ok(0);
    };
    let (body, block_ids, folded) = fold_plugin_blocks(&text, None);
    if folded == 0 && !has_duplicate_plugins_lines(&body) {
        return Ok(0);
    }
    let mut ids = find_plugins(&body)?
        .map(|edit| edit.primary.ids)
        .unwrap_or_default();
    for id in block_ids {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let updated = set_plugins(&body, &ids)?;
    if updated != body {
        write_atomic(&config, &updated)?;
    }
    Ok(folded)
}

/// Startup hook: fold the legacy blocks and print exactly one operator line
/// when the workspace self-healed or the fold was refused.
pub fn heal_workspace_config(workspace: &Path) {
    match migrate_plugin_blocks(workspace) {
        Ok(0) => {}
        Ok(_) => {
            eprintln!("onlyne: migrated [[plugin]] blocks into plugins = [...]");
        }
        Err(error) => eprintln!("{error}"),
    }
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

    /// The config shape `onlyne-client init` writes, comments included.
    fn write_role_config(workspace: &Path) -> PathBuf {
        let onlyne = workspace.join(".onlyne");
        std::fs::create_dir_all(&onlyne).unwrap();
        let config = onlyne.join("config.toml");
        std::fs::write(
            &config,
            "role = \"planner\"\n# local plugin list\ncert_pin = \"sha256/pin\"\nkey_path = \"keys/role.key\"\nplugins = []\n\n[server]\nhost = \"127.0.0.1\"\nport = 9443\n",
        )
        .unwrap();
        config
    }

    /// A flat package holding the single `onlyne-agent-<id>` binary the
    /// installer demands.
    fn write_package(package: &Path, id: &str) {
        std::fs::write(
            package.join(format!("onlyne-agent-{id}")),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
    }

    #[test]
    fn agent_install_and_uninstall_round_trip() {
        let workspace = tempdir().unwrap();
        let package = tempdir().unwrap();
        write_package(package.path(), "demo");
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
        // The workspace had no config.toml before, so the installer's line
        // says which file it created; a plugins-only file is what lands.
        let config = std::fs::read_to_string(workspace.path().join(".onlyne/config.toml")).unwrap();
        assert!(config.contains("plugins = [\"demo\"]"));
        assert!(!config.contains("[[plugin]]"));
        let removed = agent_uninstall(workspace.path(), "demo").unwrap();
        assert_eq!(removed.len(), 2);
        assert_eq!(removed[0], "deregistered plugin demo from plugins");
        assert!(!target.exists());
    }

    #[test]
    fn install_registers_plugin_id_in_the_plugins_array() {
        let workspace = tempdir().unwrap();
        let package = tempdir().unwrap();
        let config = write_role_config(workspace.path());
        write_package(package.path(), "demo");
        let lines = agent_install(workspace.path(), package.path(), "demo", None).unwrap();
        assert_eq!(lines[2], "registered plugin demo in plugins = [\"demo\"]");
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("plugins = [\"demo\"]"));
        assert!(!text.contains("[[plugin]]"));
        let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
        assert_eq!(parsed.plugins, vec!["demo".to_string()]);
        // A second plugin extends the same single-line array.
        let other = tempdir().unwrap();
        write_package(other.path(), "beta");
        agent_install(workspace.path(), other.path(), "beta", None).unwrap();
        let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
        assert_eq!(parsed.plugins, vec!["demo".to_string(), "beta".to_string()]);
    }

    #[test]
    fn reinstall_refuses_and_config_stays_already_installed_state() {
        // A drifted workspace: the package directory is gone but the array
        // still registers the id. Reinstalling refuses with the existing
        // "already installed" message and changes no byte.
        let workspace = tempdir().unwrap();
        let package = tempdir().unwrap();
        let config = write_role_config(workspace.path());
        write_package(package.path(), "demo");
        agent_install(workspace.path(), package.path(), "demo", None).unwrap();
        let before = std::fs::read_to_string(&config).unwrap();
        std::fs::remove_dir_all(agent_package_dir(workspace.path(), "demo")).unwrap();
        let error = agent_install(workspace.path(), package.path(), "demo", None).unwrap_err();
        assert_eq!(error.to_string(), "onlyne: plugin demo already installed");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
    }

    #[test]
    fn uninstall_drops_the_id_and_keeps_every_other_byte() {
        let workspace = tempdir().unwrap();
        let config = write_role_config(workspace.path());
        let demo = tempdir().unwrap();
        write_package(demo.path(), "demo");
        agent_install(workspace.path(), demo.path(), "demo", None).unwrap();
        let beta = tempdir().unwrap();
        write_package(beta.path(), "beta");
        agent_install(workspace.path(), beta.path(), "beta", None).unwrap();
        let before = std::fs::read_to_string(&config).unwrap();
        agent_uninstall(workspace.path(), "demo").unwrap();
        let after = std::fs::read_to_string(&config).unwrap();
        let before_lines: Vec<&str> = before.lines().collect();
        let after_lines: Vec<&str> = after.lines().collect();
        assert_eq!(before_lines.len(), after_lines.len());
        let mut plugins_line = None;
        for (old, new) in before_lines.iter().zip(&after_lines) {
            if old.starts_with("plugins") {
                assert_ne!(old, new);
                plugins_line = Some(new.to_string());
            } else {
                assert_eq!(old, new);
            }
        }
        assert_eq!(plugins_line.unwrap(), "plugins = [\"beta\"]");
        let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
        assert_eq!(parsed.plugins, vec!["beta".to_string()]);
    }

    #[test]
    fn heal_folds_legacy_blocks_into_an_existing_array() {
        // The exact state the 1.2.1 installer left behind: the init file with
        // `plugins = []` plus an appended `[[plugin]]` block that made every
        // later `client run` exit 1.
        let workspace = tempdir().unwrap();
        let config = write_role_config(workspace.path());
        std::fs::write(
            &config,
            format!(
                "{}[[plugin]]\nid = \"demo\"\n",
                std::fs::read_to_string(&config).unwrap()
            ),
        )
        .unwrap();
        assert!(onlyne_config::ClientConfig::load(&config).is_err());
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 1);
        let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
        assert_eq!(parsed.plugins, vec!["demo".to_string()]);
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(!text.contains("[[plugin]]"));
        assert!(text.contains("plugins = [\"demo\"]"));
        assert!(text.contains("# local plugin list"));
        // Healing a healed workspace is a no-op.
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
    }

    #[test]
    fn heal_inserts_the_array_when_the_legacy_file_has_none() {
        // Pre-array workspaces: two blocks above a `[server]` table and no
        // `plugins` line at all. Migration keeps the blocks' order, inserts
        // the line after `key_path`, and drops both tables.
        let workspace = tempdir().unwrap();
        let onlyne = workspace.path().join(".onlyne");
        std::fs::create_dir_all(&onlyne).unwrap();
        let config = onlyne.join("config.toml");
        std::fs::write(
            &config,
            "role = \"planner\"\ncert_pin = \"sha256/pin\"\nkey_path = \"keys/role.key\"\n\n[server]\nhost = \"127.0.0.1\"\nport = 9443\n\n[[plugin]]\nid = \"demo\"\n\n[[plugin]]\nid = \"beta\"\n",
        )
        .unwrap();
        assert!(onlyne_config::ClientConfig::load(&config).is_err());
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 2);
        let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
        assert_eq!(parsed.plugins, vec!["demo".to_string(), "beta".to_string()]);
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(!text.contains("[[plugin]]"));
    }

    #[test]
    fn uninstall_folds_a_legacy_block_the_array_missed() {
        // In a 1.2.1 workspace the id can live only inside `[[plugin]]`: the
        // uninstall must drop that block even though the array never held it.
        let workspace = tempdir().unwrap();
        let config = write_role_config(workspace.path());
        std::fs::write(
            &config,
            format!(
                "{}[[plugin]]\nid = \"demo\"\n",
                std::fs::read_to_string(&config).unwrap()
            ),
        )
        .unwrap();
        let target = agent_package_dir(workspace.path(), "demo");
        std::fs::create_dir_all(&target).unwrap();
        agent_uninstall(workspace.path(), "demo").unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(!text.contains("[[plugin]]"));
        assert!(text.contains("plugins = []"));
        assert!(!target.exists());
    }

    #[test]
    fn heal_leaves_unrelated_config_errors_untouched() {
        // A failure not caused by `[[plugin]]` must reach the operator as the
        // serde error it is: no rewrite, no swallowed report.
        let workspace = tempdir().unwrap();
        let onlyne = workspace.path().join(".onlyne");
        std::fs::create_dir_all(&onlyne).unwrap();
        let config = onlyne.join("config.toml");
        std::fs::write(
            &config,
            "role = \"planner\"\ncert_pin = \"sha256/pin\"\nkey_path = \"keys/role.key\"\nbogus = true\nplugins = []\n",
        )
        .unwrap();
        let before = std::fs::read_to_string(&config).unwrap();
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
        let error = onlyne_config::ClientConfig::load(&config).unwrap_err();
        assert!(
            error.to_string().contains("unknown field `bogus`"),
            "the serde refusal must survive: {error}"
        );
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

    fn plugin_ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn write_config(workspace: &Path, text: &str) -> PathBuf {
        let config = RoleWorkspace::resolve(workspace).config_path();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, text).unwrap();
        config
    }

    fn assert_no_temp_file(config: &Path) {
        let name = format!("{}.tmp", config.file_name().unwrap().to_string_lossy());
        let temp = config.with_file_name(name);
        assert!(
            !temp.exists(),
            "atomic config replacement left {} behind",
            temp.display()
        );
    }

    fn assert_migration_refused(workspace: &Path, text: &str, reason: &str) {
        let config = write_config(workspace, text);
        let error = migrate_plugin_blocks(workspace).unwrap_err();
        assert_eq!(error.to_string(), reason);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
        assert_no_temp_file(&config);
    }

    #[test]
    fn fold_plugin_blocks_removes_each_owned_block_and_reports_ids_in_order() {
        let text = r#"plugins = []
[[plugin]]
id = "demo"
path = "agent/demo"
[[plugin]]
id = "beta"
"#;
        let (out, ids, folded) = fold_plugin_blocks(text, None);
        assert_eq!(out, "plugins = []\n");
        assert_eq!(ids, plugin_ids(&["demo", "beta"]));
        assert_eq!(folded, 2);
    }

    #[test]
    fn fold_plugin_blocks_only_removes_the_requested_id() {
        let text = r#"plugins = []
[[plugin]]
id = "demo"
[[plugin]]
id = "beta"
"#;
        let (out, ids, folded) = fold_plugin_blocks(text, Some("demo"));
        assert_eq!(
            out,
            r#"plugins = []
[[plugin]]
id = "beta"
"#
        );
        assert_eq!(ids, plugin_ids(&["demo"]));
        assert_eq!(folded, 1);
    }

    #[test]
    fn fold_plugin_blocks_keeps_blocks_without_a_parsable_id() {
        let text = r#"plugins = []
[[plugin]]
path = "agent/demo"
[[plugin]]
id = "beta"
"#;
        let (out, ids, folded) = fold_plugin_blocks(text, None);
        assert_eq!(
            out,
            r#"plugins = []
[[plugin]]
path = "agent/demo"
"#
        );
        assert_eq!(ids, plugin_ids(&["beta"]));
        assert_eq!(folded, 1);
    }

    #[test]
    fn fold_plugin_blocks_counts_duplicate_ids_without_repeating_them() {
        let text = r#"plugins = []
[[plugin]]
id = "demo"
[[plugin]]
id = "demo"
"#;
        let (out, ids, folded) = fold_plugin_blocks(text, None);
        assert_eq!(out, "plugins = []\n");
        assert_eq!(ids, plugin_ids(&["demo"]));
        assert_eq!(folded, 2);
    }

    #[test]
    fn fold_plugin_blocks_folds_headers_with_spaces_and_trailing_comments() {
        let text = r#"plugins = []
[[ plugin ]] # legacy demo
id = "demo"
[[plugin]] # legacy beta
id = "beta"
"#;
        let (out, ids, folded) = fold_plugin_blocks(text, None);
        assert_eq!(out, "plugins = []\n");
        assert_eq!(ids, plugin_ids(&["demo", "beta"]));
        assert_eq!(folded, 2);
    }

    #[test]
    fn find_plugins_ignores_keys_inside_tables() {
        let top_level = r#"plugins = ["demo"]
[table]
plugins = ["wrong"]
"#;
        assert_eq!(
            find_plugins(top_level).unwrap().unwrap().primary.ids,
            plugin_ids(&["demo"])
        );
        assert!(
            find_plugins("[table]\nplugins = [\"wrong\"]\n")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn find_plugins_refuses_a_plugins_array_split_across_lines() {
        let error = find_plugins("plugins = [\n  \"alpha\",\n]\n")
            .err()
            .expect("a top-level plugins array split across lines must be refused");
        assert_eq!(
            error.to_string(),
            "onlyne: config.toml splits the `plugins` array across lines; put every id on one line (line 1)"
        );
    }

    #[test]
    fn parse_inline_array_reads_plain_ids_and_keeps_the_tail() {
        let (ids, tail) = parse_inline_array(r#"["a", "b"]   # keep"#).unwrap();
        assert_eq!(ids, plugin_ids(&["a", "b"]));
        assert_eq!(tail, "   # keep");
        let (empty, empty_tail) = parse_inline_array("[]").unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty_tail, "");
    }

    #[test]
    fn parse_inline_array_names_the_unsafe_shape_it_refuses() {
        let cases = [
            (
                r#""alpha""#,
                "onlyne: config.toml keeps `plugins` in a shape this verb cannot edit; write it as plugins = [\"id\"]",
            ),
            (
                r#"["alpha", beta]"#,
                "onlyne: config.toml `plugins` array holds a value this verb cannot edit",
            ),
            (
                r#"["alpha"] junk"#,
                "onlyne: config.toml has trailing text on the `plugins` line: [\"alpha\"] junk",
            ),
        ];
        for (value, reason) in cases {
            let error = parse_inline_array(value).unwrap_err();
            assert_eq!(error.to_string(), reason);
        }
    }

    #[test]
    fn set_plugins_replaces_the_existing_line_and_preserves_its_tail() {
        let text = "role = \"planner\"\n  plugins = [\"old\"]   # keep\n";
        let updated = set_plugins(text, &plugin_ids(&["new"])).unwrap();
        assert_eq!(
            updated,
            "role = \"planner\"\n  plugins = [\"new\"]   # keep\n"
        );
    }

    #[test]
    fn set_plugins_inserts_a_top_level_array_before_table_keys() {
        let text = "role = \"planner\"\n[other]\nkey_path = \"ignored\"\n";
        let updated = set_plugins(text, &plugin_ids(&["demo"])).unwrap();
        assert_eq!(
            updated,
            "role = \"planner\"\nplugins = [\"demo\"]\n[other]\nkey_path = \"ignored\"\n"
        );
        let value: toml::Value = updated.parse().unwrap();
        let root = value.as_table().unwrap();
        assert_eq!(root.get("plugins").unwrap().as_array().unwrap().len(), 1);
        assert_eq!(
            root.get("other")
                .and_then(|other| other.get("key_path"))
                .and_then(toml::Value::as_str),
            Some("ignored")
        );
    }

    #[test]
    fn set_plugins_terminates_the_config_with_one_newline() {
        let updated = set_plugins("plugins = []", &plugin_ids(&["demo"])).unwrap();
        assert_eq!(updated, "plugins = [\"demo\"]\n");
    }

    #[test]
    fn set_plugins_refuses_a_malformed_existing_array() {
        let error = set_plugins("plugins = \"demo\"\n", &plugin_ids(&["beta"])).unwrap_err();
        assert_eq!(
            error.to_string(),
            "onlyne: config.toml keeps `plugins` in a shape this verb cannot edit; write it as plugins = [\"id\"] (line 1)"
        );
    }

    #[test]
    fn migrate_plugin_blocks_preserves_existing_ids_and_deduplicates_blocks() {
        let workspace = tempdir().unwrap();
        let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"] # operator note

[[plugin]]
id = "demo"
[[plugin]]
id = "alpha"
[[plugin]]
id = "beta"
[server]
host = "127.0.0.1"
port = 9443
"#;
        let config = write_config(workspace.path(), text);
        assert!(
            onlyne_config::ClientConfig::load(&config).is_err(),
            "the fixture must start as a workspace the client loader refuses"
        );
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 3);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha", "demo", "beta"] # operator note

[server]
host = "127.0.0.1"
port = 9443
"#
        );
        let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
        assert_eq!(parsed.plugins, plugin_ids(&["alpha", "demo", "beta"]));
        assert_no_temp_file(&config);
    }

    #[test]
    fn migrate_plugin_blocks_is_idempotent_and_leaves_no_temp_file() {
        let workspace = tempdir().unwrap();
        let config = write_config(
            workspace.path(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = []

[[plugin]]
id = "demo"
[[plugin]]
id = "beta"
[server]
host = "127.0.0.1"
port = 9443
"#,
        );
        assert!(
            onlyne_config::ClientConfig::load(&config).is_err(),
            "the fixture must start as a workspace the client loader refuses"
        );
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 2);
        let migrated = std::fs::read_to_string(&config).unwrap();
        assert_no_temp_file(&config);
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), migrated);
        assert_no_temp_file(&config);
    }

    #[test]
    fn migrate_plugin_blocks_inserts_the_array_after_key_path() {
        let workspace = tempdir().unwrap();
        let config = write_config(
            workspace.path(),
            r#"role = "planner"
key_path = "keys/role.key"
cert_pin = "sha256/pin"
[server]
host = "127.0.0.1"
port = 9443
[[plugin]]
id = "demo"
"#,
        );
        assert!(
            onlyne_config::ClientConfig::load(&config).is_err(),
            "the fixture must start as a workspace the client loader refuses"
        );
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            r#"role = "planner"
key_path = "keys/role.key"
plugins = ["demo"]
cert_pin = "sha256/pin"
[server]
host = "127.0.0.1"
port = 9443
"#
        );
        let parsed = onlyne_config::ClientConfig::load(&config)
            .expect("migration must leave the workspace loadable");
        assert_eq!(parsed.plugins, plugin_ids(&["demo"]));
        assert_no_temp_file(&config);
    }

    #[test]
    fn migrate_plugin_blocks_keeps_a_block_without_an_id_visible_to_the_loader() {
        let workspace = tempdir().unwrap();
        let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = []
[[plugin]]
path = "agent/demo"
[server]
host = "127.0.0.1"
port = 9443
"#;
        let config = write_config(workspace.path(), text);
        let error = onlyne_config::ClientConfig::load(&config).expect_err(
            "an id-less plugin table is exactly the operator-visible refusal migration must preserve",
        );
        assert!(
            error.to_string().contains("unknown field `plugin`"),
            "the loader must still name the retained plugin table: {error}"
        );
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
        assert!(
            onlyne_config::ClientConfig::load(&config)
                .err()
                .is_some_and(|error| error.to_string().contains("unknown field `plugin`")),
            "migration must not report a no-id block as healed"
        );
        assert_no_temp_file(&config);
    }

    #[test]
    fn migrate_plugin_blocks_refuses_unsafely_formatted_arrays_without_writing() {
        let workspace = tempdir().unwrap();
        assert_migration_refused(
            workspace.path(),
            "plugins = [\n  \"alpha\",\n]\n[[plugin]]\nid = \"demo\"\n",
            "onlyne: config.toml splits the `plugins` array across lines; put every id on one line (line 1)",
        );
        assert_migration_refused(
            workspace.path(),
            "plugins = \"alpha\"\n[[plugin]]\nid = \"demo\"\n",
            "onlyne: config.toml keeps `plugins` in a shape this verb cannot edit; write it as plugins = [\"id\"] (line 1)",
        );
        assert_migration_refused(
            workspace.path(),
            "plugins = [\"alpha\", beta]\n[[plugin]]\nid = \"demo\"\n",
            "onlyne: config.toml `plugins` array holds a value this verb cannot edit (line 1)",
        );
        assert_migration_refused(
            workspace.path(),
            "plugins = [\"alpha\"] junk\n[[plugin]]\nid = \"demo\"\n",
            "onlyne: config.toml has trailing text on the `plugins` line: [\"alpha\"] junk (line 1)",
        );
    }

    #[test]
    fn migrate_plugin_blocks_ignores_an_unsafely_formatted_array_when_no_block_needs_migrating() {
        let workspace = tempdir().unwrap();
        let text = "plugins = [\n  \"alpha\",\n]\n";
        let config = write_config(workspace.path(), text);
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
        assert_no_temp_file(&config);
    }

    #[test]
    fn heal_workspace_config_migrates_a_legacy_workspace() {
        let workspace = tempdir().unwrap();
        let config = write_config(
            workspace.path(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = []
[[plugin]]
id = "demo"
[server]
host = "127.0.0.1"
port = 9443
"#,
        );
        assert!(
            onlyne_config::ClientConfig::load(&config).is_err(),
            "the fixture must start as a workspace the client loader refuses"
        );
        heal_workspace_config(workspace.path());
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["demo"]
[server]
host = "127.0.0.1"
port = 9443
"#
        );
        let parsed = onlyne_config::ClientConfig::load(&config)
            .expect("the startup hook must leave the healed workspace loadable");
        assert_eq!(parsed.plugins, plugin_ids(&["demo"]));
        assert_no_temp_file(&config);
    }

    #[test]
    fn heal_workspace_config_leaves_a_healthy_workspace_untouched() {
        let workspace = tempdir().unwrap();
        let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["demo"]
[server]
host = "127.0.0.1"
port = 9443
"#;
        let config = write_config(workspace.path(), text);
        heal_workspace_config(workspace.path());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
        assert_no_temp_file(&config);
    }

    #[test]
    fn heal_workspace_config_leaves_an_unfixable_workspace_untouched() {
        let workspace = tempdir().unwrap();
        let text = "plugins = [\n  \"alpha\",\n]\n[[plugin]]\nid = \"demo\"\n";
        let config = write_config(workspace.path(), text);
        heal_workspace_config(workspace.path());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
        assert_no_temp_file(&config);
    }

    #[test]
    fn heal_workspace_config_creates_no_config_for_a_workspace_without_one() {
        let workspace = tempdir().unwrap();
        let config = RoleWorkspace::resolve(workspace.path()).config_path();
        heal_workspace_config(workspace.path());
        assert!(!config.exists());
        assert_no_temp_file(&config);
    }

    #[test]
    fn migrate_plugin_blocks_folds_a_plugin_header_with_a_trailing_comment() {
        let workspace = tempdir().unwrap();
        let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = []
[[plugin]] # id block
id = "demo"
[server]
host = "127.0.0.1"
port = 9443
"#;
        let config = write_config(workspace.path(), text);
        assert!(
            onlyne_config::ClientConfig::load(&config).is_err(),
            "the fixture must start as a workspace the client loader refuses"
        );
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["demo"]
[server]
host = "127.0.0.1"
port = 9443
"#
        );
        let parsed = onlyne_config::ClientConfig::load(&config)
            .expect("a folded comment-headed legacy block must leave a loadable workspace");
        assert_eq!(parsed.plugins, plugin_ids(&["demo"]));
        assert_no_temp_file(&config);
    }

    #[test]
    fn migrate_plugin_blocks_merges_duplicate_top_level_plugins_lines() {
        let workspace = tempdir().unwrap();
        let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"]
plugins = ["beta"]
[[plugin]]
id = "demo"
[server]
host = "127.0.0.1"
port = 9443
"#;
        let config = write_config(workspace.path(), text);
        assert!(
            onlyne_config::ClientConfig::load(&config).is_err(),
            "duplicate top-level plugins keys must make the fixture unloadable"
        );
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha", "beta", "demo"]
[server]
host = "127.0.0.1"
port = 9443
"#
        );
        let parsed = onlyne_config::ClientConfig::load(&config)
            .expect("merged duplicate plugins lines must leave one loadable top-level key");
        assert_eq!(parsed.plugins, plugin_ids(&["alpha", "beta", "demo"]));
        assert_no_temp_file(&config);
    }

    #[test]
    fn migrate_plugin_blocks_merges_duplicate_plugins_lines_without_legacy_blocks() {
        let workspace = tempdir().unwrap();
        let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"] # first line
plugins = ["beta", "alpha"]
[server]
host = "127.0.0.1"
port = 9443
"#;
        let config = write_config(workspace.path(), text);
        assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha", "beta"] # first line
[server]
host = "127.0.0.1"
port = 9443
"#
        );
        let parsed = onlyne_config::ClientConfig::load(&config)
            .expect("duplicate-only plugins lines must heal into one loadable array");
        assert_eq!(parsed.plugins, plugin_ids(&["alpha", "beta"]));
        assert_no_temp_file(&config);
    }

    #[test]
    fn migrate_plugin_blocks_refuses_an_unsafe_duplicate_plugins_line_without_writing() {
        let workspace = tempdir().unwrap();
        let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"]
plugins = ["beta", gamma]
[[plugin]]
id = "demo"
[server]
host = "127.0.0.1"
port = 9443
"#;
        let config = write_config(workspace.path(), text);
        let error = migrate_plugin_blocks(workspace.path())
            .expect_err("an unsafe duplicate plugins line must abort the whole merge");
        assert_eq!(
            error.to_string(),
            "onlyne: config.toml `plugins` array holds a value this verb cannot edit (line 5)"
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
        assert_no_temp_file(&config);
    }

    #[test]
    fn append_plugin_entry_merges_duplicates_when_id_is_already_registered() {
        let workspace = tempdir().unwrap();
        let config = write_config(
            workspace.path(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"]
plugins = ["beta"]
[server]
host = "127.0.0.1"
port = 9443
"#,
        );
        let ids = append_plugin_entry(workspace.path(), "alpha").unwrap();
        assert_eq!(ids, plugin_ids(&["alpha", "beta"]));
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha", "beta"]
[server]
host = "127.0.0.1"
port = 9443
"#
        );
        let parsed = onlyne_config::ClientConfig::load(&config)
            .expect("append must leave one loadable merged array");
        assert_eq!(parsed.plugins, plugin_ids(&["alpha", "beta"]));
    }

    #[test]
    fn remove_plugin_entry_merges_duplicate_lines_while_dropping_the_id() {
        let workspace = tempdir().unwrap();
        let config = write_config(
            workspace.path(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"]
plugins = ["beta"]
[server]
host = "127.0.0.1"
port = 9443
"#,
        );
        remove_plugin_entry(workspace.path(), "beta").unwrap();
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"]
[server]
host = "127.0.0.1"
port = 9443
"#
        );
        let parsed = onlyne_config::ClientConfig::load(&config)
            .expect("removal must leave one loadable merged array");
        assert_eq!(parsed.plugins, plugin_ids(&["alpha"]));
    }
}
