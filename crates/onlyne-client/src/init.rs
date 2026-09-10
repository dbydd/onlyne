use anyhow::{Result, anyhow};
use onlyne_config::Spec;
use onlyne_layout::{LEGACY_WORKSPACE_MESSAGE, RoleWorkspace, ServerRoot, detect_legacy};
use onlyne_net::KeyPair;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct InitArgs {
    pub workspace: PathBuf,
    pub role: String,
    pub server_root: PathBuf,
    /// Role control plane text, copied into the printed spec slice (§5).
    pub prose: String,
}

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
        "role = {role:?}\ncert_pin = {pin:?}\nkey_path = {key_path:?}\nplugins = []\n\n[server]\nhost = {host:?}\nport = {port}\n",
        role = args.role,
        pin = cert_pin,
        key_path = workspace.key_path().display().to_string(),
        host = host,
        port = port,
    );
    std::fs::write(workspace.config_path(), config)?;
    Ok(fragment(&args.role, &key.public_str(), &args.prose))
}

/// The `[[client]]` slice `init` prints for `spec.toml`.
///
/// The ACL lines make the role deliverable to itself, which is what the
/// end-to-end script exercises with `send --from planner --to planner`.
/// `public_key` arrives in the `ed25519/<base64>` form `KeyPair::public_str`
/// produces, which is the same string the handshake verifies against.
pub fn fragment(role: &str, public_key: &str, prose: &str) -> String {
    let role = toml_string(role);
    let key = toml_string(public_key);
    let prose = toml_string(prose);
    format!(
        "[[client]]\nrole = {role}\nkey = {key}\nadmin = false\nmax_sessions = 1\nallowed_senders = [\"*\", {role}]\nallowed_targets = [{role}]\nprose = {prose}\nreuse = true\n",
    )
}

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
