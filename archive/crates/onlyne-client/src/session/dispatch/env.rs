use super::state::DispatchInner;
use super::*;

/// Refuse to open a session when the role's drive cannot run under this
/// machine's placement.
///
/// The one rule is `onlyne_config::validate_drive_placement`: `acp` pairs only
/// with `headless`, because stdio carries the ACP channel and cannot also be a
/// pane's terminal. It is checked in the config crate, where both halves of
/// what `backend` used to be are defined, and again here, where the two of them
/// meet for the first time — the drive arrives with `welcome` from the spec and
/// the placement from the machine.
///
/// A refusal is the delivery's, not a retry's: nothing this client can do makes
/// the pair work, so the row is refused with the sentence and an operator reads
/// the fix in the ledger. Running anyway — a pane printing protocol frames, or
/// an ACP child with no pane it could ever have — is the silent drift the split
/// exists to remove.
pub(super) fn reject_unpaired_runtime(inner: &DispatchInner) -> Result<()> {
    if let Some(refusal) = &inner.runtime_refusal {
        return Err(anyhow!("{refusal}"));
    }
    let (Some(drive), Some(placement)) = (inner.drive, inner.placement) else {
        // No role slice yet, or a test that installed no machine fact: nothing
        // has been declared to be inconsistent.
        return Ok(());
    };
    let Some(named) = placement.named() else {
        // The in-process test runtime owns no place on the machine.
        return Ok(());
    };
    onlyne_config::validate_drive_placement(drive, named).map_err(|message| anyhow!("{message}"))
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
/// obligation a session owes is not among them: the guard is this client's own
/// (`guards.rs`), read off the role's `owes_targets`, so there is no policy for
/// a session process to carry and no variable for it to read.
///
/// `ONLYNE_CLUSTER` names the server's topology and is the address a host
/// backend groups sessions under. No welcome yet means no variable, and a pane
/// host then keeps its own default-labelled tree.
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
