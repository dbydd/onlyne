use anyhow::{Result, anyhow};
use onlyne_config::Spec;
use onlyne_layout::{LEGACY_WORKSPACE_MESSAGE, RoleWorkspace, ServerRoot, detect_legacy};
use onlyne_net::KeyPair;
use std::path::{Path, PathBuf};

/// the backend key `init` seeds (as a comment) among the top-level keys of a
/// fresh workspace config: a commented key above `[server]` uncomments into
/// the table it belongs to, and the parse never sees a value the template
/// invented. The value range and the precedence are the ones `onlyne-config`'s
/// `ClientConfig::backend` documents.
const BACKEND_COMMENTS: &str = "\
# The session backend `onlyne client run` starts from: herdr | orca | zellij |
# exec | headless | acp | fake | auto. An empty or absent value probes the
# host, and a nonempty ONLYNE_BACKEND wins over this key.
# backend = \"auto\"
";

/// the `[acp]` table `init` seeds (as a comment) at the foot of a fresh
/// workspace config, where an uncommented table header opens a table of its
/// own. The four keys are the ones `onlyne-config`'s `AcpSection` reads.
const ACP_COMMENTS: &str = "\
# ACP backend options, read only when the backend is `acp`. `mode`, `model`,
# and `reasoning_effort` name the agent's own configuration values: the agent
# validates them, and an empty one keeps the agent's default. `permission` is
# this machine's answer to a permission request from the agent: `deny` (the
# default, it refuses and records a fault) or `allow`.
# [acp]
# mode = \"\"
# model = \"\"
# reasoning_effort = \"\"
# permission = \"deny\"
";

#[derive(Debug, Clone)]
pub struct InitArgs {
    pub workspace: PathBuf,
    pub role: String,
    pub server_root: PathBuf,
    /// Role control plane text, copied into the printed spec slice (§5).
    pub prose: String,
}

/// the reconnect window `init` seeds (as a comment) among the top-level keys of
/// a fresh workspace config, above the `[server]` header beside `backend`. The
/// key and its default are the ones `onlyne-config`'s `ClientConfig` declares,
/// and the sweep that reads it runs inside `onlyne client run`.
const RECONNECT_COMMENTS: &str = "\
# Seconds a dropped plugin connection may stay away before this client retires
# the session it left behind. A session with a task bound goes with it: an
# unsettled task ends `failed`, the delivery that session still held is refused
# with reason `session_dead`, and the session's own exit is published. An agent
# that reconnects inside the window keeps its session; a connection that returns
# after a newer session took the task is held read-only, and what it sends rides
# that session's closing handoff. 0 disables the sweep.
# reconnect_grace_secs = 60
";

/// The role identity, loaded from `role.key` or generated there.
///
/// The file holds the 32-byte ed25519 seed; the spec fragment publishes the
/// matching public key. Every producer of a role key routes through
/// `onlyne_net::KeyPair`, so the file, the fragment, and the handshake all
/// describe one identity.
fn role_key(path: &Path) -> Result<KeyPair> {
    if path.exists() {
        return KeyPair::load(path).map_err(|error| anyhow!(error.to_string()));
    }
    let key = KeyPair::generate();
    key.save(path).map_err(|error| anyhow!(error.to_string()))?;
    Ok(key)
}

fn server_section(root: &Path) -> Result<(String, String, String)> {
    let layout = ServerRoot::resolve(root);
    let spec = Spec::load(layout.spec_path())?;
    Ok((spec.server.listen, spec.server.cert_pin, spec.server.name))
}

pub async fn init(args: InitArgs) -> Result<String> {
    if detect_legacy(&args.workspace).is_some() {
        eprint!("{LEGACY_WORKSPACE_MESSAGE}");
        return Err(anyhow!("legacy workspace"));
    }
    let workspace = RoleWorkspace::resolve(&args.workspace);
    workspace.bootstrap()?;
    let key = role_key(&workspace.key_path())?;
    let (listen, cert_pin, _server_name) = server_section(&args.server_root)?;
    let (host, port) = listen
        .rsplit_once(':')
        .map(|(h, p)| (h.to_string(), p.parse::<u16>().unwrap_or(0)))
        .unwrap_or((listen, 0));
    let config = format!(
        "role = {role:?}\ncert_pin = {pin:?}\nkey_path = {key_path:?}\nplugins = []\n\n{BACKEND_COMMENTS}\n{RECONNECT_COMMENTS}\n[server]\nhost = {host:?}\nport = {port}\n\n{ACP_COMMENTS}",
        role = args.role,
        pin = cert_pin,
        key_path = workspace.key_path().display().to_string(),
        host = host,
        port = port,
    );
    std::fs::write(workspace.config_path(), config)?;
    Ok(fragment(&args.role, &key.public_str(), &args.prose))
}

/// The `session_command` seed the printed fragment ships.
///
/// The bytes match the seed `examples/supervisor/run.py` writes into its ring
/// entries, so a pasted fragment spawns the same session the demo cluster runs.
const SEED_SESSION_COMMAND: &str = "session_command = [\"pi\", \"--session-id\", \"{session}\", \"--session-dir\", \".pi/sessions\", \"-ns\"]";

/// The `[[client]]` slice `init` prints for `spec.toml`.
///
/// The ACL lines make the role deliverable to itself, which is what the
/// end-to-end script exercises with `send --from planner --to planner`.
/// `public_key` arrives in the `ed25519/<base64>` form `KeyPair::public_str`
/// produces, which is the same string the handshake verifies against. The
/// `session_command` line is what the client runs per task (§5, §6): a role
/// entry without one leaves every delivery staged with no process behind it.
pub fn fragment(role: &str, public_key: &str, prose: &str) -> String {
    let role = toml_string(role);
    let key = toml_string(public_key);
    let prose = toml_string(prose);
    format!(
        "[[client]]\nrole = {role}\nkey = {key}\nadmin = false\nmax_sessions = 1\nallowed_senders = [\"*\", {role}]\nallowed_targets = [{role}]\nprose = {prose}\n{command}\n{KNOB_COMMENTS}",
        command = SEED_SESSION_COMMAND,
    )
}

/// The implemented-but-unprinted `[[client]]` keys, carried as comment lines
/// so the entry an operator pastes is the whole vocabulary. Each line shows
/// the default the parser applies; uncommenting one changes what the entry
/// says, leaving every line commented changes nothing. The field names and
/// values are the ones `onlyne-config`'s `ClientEntry` declares.
const KNOB_COMMENTS: &str = "\
# timeout = { ready_ms = 30000, idle_ms = 60000 }
# Per-session budgets in milliseconds; the server projects them into the hello
# reply as `timeout_ready_ms` and `timeout_idle_ms`.
# intent = { attempts = 3, backoff_ms = [1000, 2000, 4000] }
# Retry policy for one intent: total attempts, then the per-retry waits in
# milliseconds; a longer list repeats its last entry.
# aggregate = \"\"
# The child-cluster name this role stands for. It is an annotation only: no
# delivery decision reads it, and a plain role leaves it empty.
# relay_required = []
# Downstream roles one of this role's sessions must have handed work to before
# it may report a terminal outcome. Absent or empty is the default: no guard.
# relay_count = <n>
# The count form of relay_required: this many distinct downstream roles. When
# both keys are present the non-empty list wins.
";

/// One TOML basic string, with the characters a basic string cannot hold escaped.
pub fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

pub fn legacy_error_code() -> i32 {
    2
}
