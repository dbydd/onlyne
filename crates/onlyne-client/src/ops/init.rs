use anyhow::{Result, anyhow};
use onlyne_config::Spec;
use onlyne_config::layout::{RoleWorkspace, ServerRoot, detect_legacy};
use onlyne_net::KeyPair;
use std::path::{Path, PathBuf};

/// the placement key `init` seeds (as a comment) among the top-level keys of a
/// fresh workspace config: a commented key above `[server]` uncomments into
/// the table it belongs to, and the parse never sees a value the template
/// invented. The value range and the precedence are the ones `onlyne-config`'s
/// `ClientConfig::placement` documents.
///
/// Placement is the machine's half of the pair; the drive is the runtime's and
/// lives in the server's `spec.toml` (`[client.runtime]`), which is why no
/// drive is seeded here.
const PLACEMENT_COMMENTS: &str = "\
# Where this machine displays the role's runtime: orca | zellij | tern |
# headless | external. An absent value probes tern, orca, zellij in that
# order and falls back to headless; a nonempty ONLYNE_BACKEND wins over this
# key.
# placement = \"headless\"
";

/// the `[acp]` table `init` seeds (as a comment) at the foot of a fresh
/// workspace config, where an uncommented table header opens a table of its
/// own. The four keys are the ones `onlyne-config`'s `AcpSection` reads.
const ACP_COMMENTS: &str = "\
# ACP options, read only when the role's `[client.runtime] drive` is `acp`.
# `mode`, `model`,
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

/// The workspace `init` was pointed at is one this build will not write into.
///
/// A type rather than a string, because the caller answers it with its own exit
/// code: refusing to start on a tree from another revision is a different
/// situation from a run that failed, and a supervisor needs to tell them apart.
/// Matching on `"legacy workspace"` compared a sentence.
#[derive(Debug)]
pub struct LegacyWorkspace;

impl std::fmt::Display for LegacyWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("legacy workspace")
    }
}

impl std::error::Error for LegacyWorkspace {}

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
    if let Some(reason) = detect_legacy(&args.workspace) {
        eprintln!(
            "{}",
            onlyne_proto::legacy_workspace_message(&[reason.to_string()])
        );
        return Err(anyhow::Error::new(LegacyWorkspace));
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
        "role = {role:?}\ncert_pin = {pin:?}\nkey_path = {key_path:?}\nplugins = []\n\n{PLACEMENT_COMMENTS}\n{RECONNECT_COMMENTS}\n[server]\nhost = {host:?}\nport = {port}\n\n{ACP_COMMENTS}",
        role = args.role,
        pin = cert_pin,
        key_path = workspace.key_path().display().to_string(),
        host = host,
        port = port,
    );
    std::fs::write(workspace.config_path(), config)?;
    Ok(fragment(&args.role, &key.public_str(), &args.prose))
}

/// Append an already-rendered `[[client]]` fragment to the server's
/// `spec.toml`. The fragment arrives as plain text from the caller; this
/// function answers whether the spec survives it. A role the spec already
/// declares is a refusal, not a second entry: two entries with one role is
/// an ambiguity no reader resolves.
pub fn register_in_spec(server_root: &Path, role: &str, fragment: &str) -> Result<()> {
    let spec_path = ServerRoot::resolve(server_root).spec_path();
    let spec = Spec::load(&spec_path)?;
    if spec.client.iter().any(|c| c.role == role) {
        return Err(anyhow!(
            "spec.toml already declares a `[client]` entry for role {role:?}; \
             remove it before registering again"
        ));
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(&spec_path)?;
    file.write_all(b"\n")?;
    file.write_all(fragment.as_bytes())?;
    drop(file);
    Spec::load(&spec_path)?;
    Ok(())
}

/// The `[client.runtime]` seed the printed fragment ships.
///
/// The bytes match the seed `examples/supervisor/run.py` writes into its ring
/// entries, so a pasted fragment spawns the same session the demo cluster runs.
/// The table is written last, because a key after it belongs to it.
///
/// `resume` is the same runtime asked for the *other* thing: `pi --session <id>`
/// opens a conversation this client released (a `task` scope's `idle_close`,
/// `AGENTS.md` §10) instead of starting a second one under the same id. The key is
/// only half the pair: the client releases a session only when the runtime's own
/// mount declared the `resume` capability, and `pi`'s plugin declares it. A role
/// that pastes this fragment onto a runtime that cannot come back is inert rather
/// than wrong.
const SEED_RUNTIME: &str = "\
[client.runtime]\n\
drive = \"plugin\"\n\
command = [\"pi\", \"--session-id\", \"{session}\", \"--session-dir\", \".pi/sessions\", \"-ns\"]\n\
resume = [\"pi\", \"--session\", \"{session}\", \"--session-dir\", \".pi/sessions\", \"-ns\"]";

/// The `[[client]]` slice `init` prints for `spec.toml`.
///
/// The ACL lines make the role deliverable to itself, which is what the
/// end-to-end script exercises with `send --from planner --to planner`.
/// `public_key` arrives in the `ed25519/<base64>` form `KeyPair::public_str`
/// produces, which is the same string the handshake verifies against. The
/// `[client.runtime]` table is the drive and the argv the client runs per
/// session (§5, §6): a role entry whose table carries no command leaves every
/// delivery staged with no process behind it.
pub fn fragment(role: &str, public_key: &str, prose: &str) -> String {
    let role = toml_string(role);
    let key = toml_string(public_key);
    let prose = toml_string(prose);
    format!(
        "[[client]]\nrole = {role}\nkey = {key}\nadmin = false\nmax_sessions = 1\nallowed_senders = [\"*\", {role}]\nallowed_targets = [{role}]\nprose = {prose}\n{KNOB_COMMENTS}\n{runtime}\n",
        runtime = SEED_RUNTIME,
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
# The [client.runtime] table below is the drive and the argv a session runs.
# `drive` names how the client talks to the runtime: plugin (the default)
# starts it and lets its plugin dial back, acp runs the agent as a child over
# stdio (placement headless only), and exec runs the command and reads its exit
# code. `command` is the argv, with {session} and {task} substituted; any other
# brace is refused.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_appends_and_refuses_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let spec_path = ServerRoot::resolve(root.path()).spec_path();
        std::fs::create_dir_all(spec_path.parent().unwrap()).unwrap();
        let planner_key = KeyPair::generate().public_str();
        let witness_frag = fragment("witness", &KeyPair::generate().public_str(), "prose");
        std::fs::write(
            &spec_path,
            format!(
                "[server]\nname = \"test\"\nlisten = \"127.0.0.1:1\"\ncert_pin = \"sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n\n[[client]]\nrole = \"planner\"\nkey = {planner_key:?}\n"
            ),
        )
        .unwrap();

        register_in_spec(root.path(), "witness", &witness_frag).unwrap();
        let spec = Spec::load(&spec_path).unwrap();
        assert!(spec.client.iter().any(|c| c.role == "witness"));

        let duplicate = register_in_spec(root.path(), "witness", &witness_frag);
        assert!(duplicate.is_err());
    }
}
