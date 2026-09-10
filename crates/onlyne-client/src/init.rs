use anyhow::{Context, Result, anyhow};
use base64::Engine;
use ed25519_dalek::SigningKey;
use onlyne_config::Spec;
use onlyne_layout::{RoleWorkspace, ServerRoot, apply_private_mode, detect_legacy, LEGACY_WORKSPACE_MESSAGE};
use rand::rngs::OsRng;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct InitArgs {
    pub workspace: PathBuf,
    pub role: String,
    pub server_root: PathBuf,
    /// Role control plane text, copied into the printed spec slice (§5).
    pub prose: String,
}

fn key_material(path: &Path) -> Result<Vec<u8>> {
    if path.exists() {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        if bytes.len() != 32 { return Err(anyhow!("role key must contain 32 bytes")); }
        return Ok(bytes);
    }
    let signing = SigningKey::generate(&mut OsRng);
    let bytes = signing.to_bytes().to_vec();
    std::fs::write(path, &bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(bytes)
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
    let key = key_material(&workspace.key_path())?;
    apply_private_mode(&workspace.key_path()).map_err(|e| anyhow!(e))?;
    let (listen, cert_pin, _server_name) = server_section(&args.server_root)?;
    let (host, port) = listen.rsplit_once(':').map(|(h, p)| (h.to_string(), p.parse::<u16>().unwrap_or(0))).unwrap_or((listen, 0));
    let config = format!(
        "role = {role:?}\ncert_pin = {pin:?}\nkey_path = {key_path:?}\nplugins = []\n\n[server]\nhost = {host:?}\nport = {port}\n",
        role = args.role,
        pin = cert_pin,
        key_path = workspace.key_path().display().to_string(),
        host = host,
        port = port,
    );
    std::fs::write(workspace.config_path(), config)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&key);
    Ok(fragment(&args.role, &encoded, &args.prose))
}

/// The `[[client]]` slice `init` prints for `spec.toml`.
///
/// The ACL lines make the role deliverable to itself, which is what the
/// end-to-end script exercises with `send --from planner --to planner`.
pub fn fragment(role: &str, encoded_key: &str, prose: &str) -> String {
    let role = toml_string(role);
    format!(
        "[[client]]\nrole = {role}\nkey = \"ed25519/{encoded_key}\"\nadmin = false\nmax_sessions = 1\nallowed_senders = [\"*\", {role}]\nallowed_targets = [{role}]\nprose = {prose}\nreuse = true\n",
        prose = toml_string(prose),
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

pub fn legacy_error_code() -> i32 { 2 }
