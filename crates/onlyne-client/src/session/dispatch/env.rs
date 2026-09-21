use super::*;

/// Refuse a protocol-speaking session command on a pane backend.
///
/// `herdr`, `orca` and `zellij` hand the agent a terminal and read its screen,
/// so a command that speaks JSON-RPC on its own stdio would print frames into
/// the pane and answer nobody. The fix belongs in the workspace config: swapping
/// the backend at spawn time instead would turn "orca configured, exec
/// running" into a silent drift the operator never sees, so the delivery fails
/// and the reason reaches the ledger.
pub(super) fn reject_protocol_command_in_pane(backend: &str, command: &[String]) -> Result<()> {
    if !matches!(backend, "herdr" | "orca" | "zellij") {
        return Ok(());
    }
    let token = command.iter().enumerate().find_map(|(index, arg)| {
        if arg == "--acp" || arg == "--mode=rpc" {
            Some(arg.as_str())
        } else if arg == "--mode" && command.get(index + 1).is_some_and(|next| next == "rpc") {
            Some("--mode rpc")
        } else {
            None
        }
    });
    if let Some(token) = token {
        return Err(anyhow!(
            "{backend} backend cannot host a protocol session: {token} speaks JSON-RPC on its own stdio and the pane would print the frames; set backend = \"exec\" or backend = \"acp\" in the workspace config"
        ));
    }
    Ok(())
}

/// The socket a session spawned in `workspace` dials.
///
/// `dispatch` passes this to [`session_env`] and the same tree lands in
/// `SpawnSpec.cwd`, so one resolve answers both halves of the spawn, and a
/// workspace past the unix bound yields the short path the client bound.
pub(super) fn served_socket(workspace: &Path) -> PathBuf {
    RoleWorkspace::resolve(workspace).socket_path()
}

/// The environment one spawned session process carries.
///
/// The three `ONLYNE_` identity variables are what the plugin mounts with. The
/// relay pair is the guard's policy as the spec wrote it: a list joined by
/// commas, and the count in decimal. A policy the spec does not name injects no
/// variable at all, which is what leaves a hand-written `relay.toml` in charge
/// of a box that never put the policy in its spec.
///
/// `ONLYNE_CLUSTER` names the server's topology and is the address a host
/// backend groups sessions under. No welcome yet means no variable, and herdr
/// then keeps its own default-labelled workspace.
///
/// `ONLYNE_SOCKET` is the path the client is serving: the same accessor the
/// daemon bound, so a short endpoint reaches the session as the served path and
/// the plugin needs no guess of its own. A hand-started pi keeps its own
/// resolution as the fallback, which is the reason an empty path injects no key
/// at all.
pub(super) fn session_env(
    role: &str,
    session_id: &str,
    task_id: &str,
    relay_required: &[String],
    relay_count: Option<u32>,
    topology: &str,
    adapter_socket: &Path,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("ONLYNE_SESSION_ID".into(), session_id.to_string());
    env.insert("ONLYNE_TASK_ID".into(), task_id.to_string());
    env.insert("ONLYNE_ROLE".into(), role.to_string());
    if !adapter_socket.as_os_str().is_empty() {
        env.insert(
            "ONLYNE_SOCKET".into(),
            adapter_socket.to_string_lossy().into_owned(),
        );
    }
    if !topology.is_empty() {
        env.insert("ONLYNE_CLUSTER".into(), topology.to_string());
    }
    if !relay_required.is_empty() {
        env.insert("ONLYNE_RELAY_REQUIRED".into(), relay_required.join(","));
    }
    if let Some(count) = relay_count {
        env.insert("ONLYNE_RELAY_COUNT".into(), count.to_string());
    }
    env
}

pub fn missing_capability(capabilities: &[Capability], capability: Capability) -> bool {
    !capabilities.contains(&capability)
}

pub fn plugin_gap(capabilities: &[Capability]) -> Vec<onlyne_adapter::HostGap> {
    onlyne_adapter::degrade_for(
        &[Capability::Recycle, Capability::Report, Capability::Inject]
            .iter()
            .copied()
            .filter(|cap| missing_capability(capabilities, *cap))
            .collect::<Vec<_>>(),
    )
}

/// Agent name this binary reports during the handshake.
pub(super) const AGENT: &str = "onlyne-client";
/// Wait bound for one request round trip.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Server heartbeat interval from the observation rules of §4.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// How many heartbeat intervals a live connection may go quiet before the
/// reconnect sweep reads the silence as an agent that stopped.
///
/// The protocol already answers this question once: `heartbeat_timeout_ms`
/// (`onlyne_config::DEFAULT_HEARTBEAT_TIMEOUT_MS`) is the presence liveness
/// timeout, and it is exactly three of these intervals — the plugin's own
/// comment on its cadence names it as the pair. Taking the margin from that
/// number keeps one answer in the tree instead of inventing a second threshold
/// beside `[client] reconnect_grace_secs`, and it is a margin over the cadence
/// rather than a fixed duration, so a plugin configured to beat slower than the
/// default is not swept for keeping its own word.
pub const HEARTBEAT_SILENCE_MARGIN: u32 = 3;

#[cfg(test)]
mod tests;
