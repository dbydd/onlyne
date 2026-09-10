use anyhow::Context;
use chrono::Utc;
use onlyne_config::{ClientEntry, Spec};
use onlyne_config::template::{discover, load_tree, local_override, merge_fragment, scan_for_prefixes, substitute_at, Placeholders, Template, TemplateError};
use onlyne_layout::{apply_private_mode, ServerRoot};
use onlyne_net::KeyPair;
use serde::Serialize;
use std::borrow::Cow;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GenerateArgs {
    pub root: PathBuf,
    pub templates: Vec<String>,
    pub roles: Vec<String>,
    pub out: Option<PathBuf>,
    pub force: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeneratedRole {
    pub role: String,
    pub dir: String,
    pub key: String,
    pub template: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerateReport {
    pub fragment: String,
    pub manifest: String,
    pub roles: Vec<GeneratedRole>,
    pub out: PathBuf,
}

#[derive(Debug)]
pub enum GenerateError {
    Template(TemplateError),
    Io { path: PathBuf, source: io::Error },
    Net(String),
    Config(String),
}

impl GenerateError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Template(error) => error.exit_code(),
            _ => 4,
        }
    }
}

impl std::fmt::Display for GenerateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Template(error) => error.fmt(f),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Net(message) | Self::Config(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for GenerateError {}
impl From<TemplateError> for GenerateError { fn from(value: TemplateError) -> Self { Self::Template(value) } }

#[derive(Debug)]
struct PendingRole {
    role: ClientEntry,
    template: Template,
    files: Vec<(String, Vec<u8>)>,
    config: Vec<u8>,
    key: KeyPair,
    public_key: String,
    topology: String,
}

pub fn generate(args: &GenerateArgs, spec: &Spec) -> Result<GenerateReport, GenerateError> {
    let root = absolute(&args.root).map_err(|source| GenerateError::Io { path: args.root.clone(), source })?;
    let server_root = ServerRoot::resolve(&root);
    server_root.bootstrap().map_err(|source| GenerateError::Io { path: server_root.dir().to_path_buf(), source })?;
    let template_root = server_root.templates_root().to_path_buf();
    let out = absolute(args.out.as_deref().unwrap_or(&server_root.ws_root()))
        .map_err(|source| GenerateError::Io { path: args.out.clone().unwrap_or_else(|| server_root.ws_root()), source })?;

    let selected_roles: Vec<&ClientEntry> = spec.client.iter().filter(|entry| {
        args.roles.is_empty() || args.roles.iter().any(|wanted| wanted == &entry.role)
    }).collect();
    if selected_roles.is_empty() {
        return Err(TemplateError::NoRoleMatches.into());
    }

    let cert_pin = certificate_pin(&server_root, spec)?;
    let mut pending = Vec::new();
    for source_role in selected_roles {
        let template = choose_template(&template_root, &source_role.role, &args.templates)?;
        let mut role = source_role.clone();
        let key = key_for_target(&out, &template, &role.role, args.force)?;
        let public_key = key.public_str();
        role.key = public_key.clone();
        let package = package_name(spec.server.agent_package.as_str());
        let placeholder = Placeholders {
            role: role.role.clone(),
            cluster: spec.server.name.clone(),
            server_name: spec.server.name.clone(),
            listen: spec.server.listen.clone(),
            cert_pin: cert_pin.clone(),
            admin: role.admin.to_string(),
            max_sessions: role.max_sessions.to_string(),
            agent_package: package.as_ref().map(|name| format!(".onlyne/agent/{name}")),
        };
        let source_files = load_tree(&template)?;
        let mut files = Vec::with_capacity(source_files.len());
        for (relative, bytes) in source_files {
            let path = template.role_dir.join(&relative);
            let substituted = substitute_at(&bytes, &placeholder, path.display().to_string())?;
            files.push((relative, substituted.into_owned()));
        }
        let derived = client_config_value(&role, spec, &cert_pin);
        let config_value = local_override(&template)
            .map(|override_| merge_fragment(&derived, &override_))
            .unwrap_or(derived);
        let config = toml::to_string(&config_value)
            .map_err(|error| GenerateError::Config(error.to_string()))?
            .into_bytes();
        pending.push(PendingRole {
            role,
            topology: template.topology.clone(),
            template,
            files,
            config,
            key,
            public_key,
        });
    }

    if pending.is_empty() {
        return Err(TemplateError::NoRoleMatches.into());
    }
    let mut targets = Vec::new();
    for item in &pending {
        let target = out.join(&item.template.relative);
        if target.exists() && !args.force {
            return Err(GenerateError::Template(TemplateError::Io {
                path: target,
                source: io::Error::new(io::ErrorKind::AlreadyExists, "onlyne: refusing to overwrite; pass --force"),
            }));
        }
        targets.push(target);
    }

    let mut created = Vec::new();
    let mut scan = Vec::new();
    for (item, target) in pending.iter().zip(targets.iter()) {
        ensure_dir(target, &mut created).map_err(|source| GenerateError::Io { path: target.clone(), source })?;
        let onlyne = target.join(".onlyne");
        ensure_dir(&onlyne, &mut created).map_err(|source| GenerateError::Io { path: onlyne.clone(), source })?;
        let keys = onlyne.join("keys");
        ensure_dir(&keys, &mut created).map_err(|source| GenerateError::Io { path: keys.clone(), source })?;
        write_file(&onlyne.join("config.toml"), &item.config).map_err(|source| GenerateError::Io { path: onlyne.join("config.toml"), source })?;
        let key_path = keys.join("role.key");
        if !key_path.exists() {
            item.key.save(&key_path).map_err(|error| GenerateError::Net(error.to_string()))?;
            apply_private_mode(&key_path).map_err(|error| GenerateError::Io { path: key_path.clone(), source: io::Error::other(error.to_string()) })?;
        }
        for (relative, bytes) in &item.files {
            let path = target.join(relative);
            if let Some(parent) = path.parent() {
                ensure_dir(parent, &mut created).map_err(|source| GenerateError::Io { path: parent.to_path_buf(), source })?;
            }
            let mut content = bytes.clone();
            if relative == ".pi/settings.json" {
                if let Some(source) = spec.server.agent_package.as_deref().filter(|value| !value.is_empty()) {
                    content = replace_bytes(&content, source.as_bytes(), format!(".onlyne/agent/{}", package_name(source).unwrap_or_default()).as_bytes());
                }
            }
            write_file(&path, &content).map_err(|source| GenerateError::Io { path: path.clone(), source })?;
            scan.push((path.display().to_string(), content));
        }
        if let Some(source) = spec.server.agent_package.as_deref().filter(|value| !value.is_empty()) {
            if item.files.iter().any(|(_, bytes)| bytes.windows(b".onlyne/agent/".len()).any(|window| window == b".onlyne/agent/")) {
                let package_target = target.join(".onlyne/agent").join(package_name(source).unwrap_or_default());
                copy_package(Path::new(source), &package_target).map_err(|source_error| GenerateError::Io { path: package_target, source: source_error })?;
            }
        }
        scan.push((onlyne.join("config.toml").display().to_string(), item.config.clone()));
    }

    let prefixes = vec![root.display().to_string(), out.display().to_string()];
    if let Err(error) = scan_for_prefixes(&scan, &prefixes) {
        for path in created.iter().rev() {
            let _ = fs::remove_dir(path);
        }
        return Err(error.into());
    }

    let roles = pending.iter().zip(targets.iter()).map(|(item, target)| GeneratedRole {
        role: item.role.role.clone(),
        dir: path_relative(&out, target),
        key: item.public_key.clone(),
        template: item.template.relative.clone(),
    }).collect::<Vec<_>>();
    let manifest_value = serde_json::json!({
        "generated_at": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "server_root": root.display().to_string(),
        "roles": roles,
    });
    let manifest = serde_json::to_string(&manifest_value).map_err(|error| GenerateError::Config(error.to_string()))?;
    write_file(&out.join(".onlyne-generation.json"), manifest.as_bytes()).map_err(|source| GenerateError::Io { path: out.join(".onlyne-generation.json"), source })?;
    let fragment = pending.iter().map(|item| render_fragment(&item.role)).collect::<Vec<_>>().join("\n");
    Ok(GenerateReport { fragment, manifest, roles: serde_json::from_value(manifest_value["roles"].clone()).unwrap_or_default(), out })
}

fn choose_template(root: &Path, role: &str, requested: &[String]) -> Result<Template, GenerateError> {
    if requested.is_empty() {
        return Ok(discover(root, role)?.remove(0));
    }
    let candidates = discover(root, role).unwrap_or_default();
    for wanted in requested {
        let path = root.join(wanted);
        if path.is_dir() && path.file_name().and_then(|name| name.to_str()) == Some(role) {
            let topology = path.parent().and_then(|parent| parent.strip_prefix(root).ok()).map(|path| path.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            return Ok(Template { role_dir: path, topology, relative: wanted.trim_matches('/').to_string() });
        }
        if let Some(found) = candidates.iter().find(|candidate| candidate.relative == *wanted) {
            return Ok(found.clone());
        }
    }
    Err(TemplateError::NoTemplate { role: role.to_string(), template_root: root.to_path_buf() }.into())
}

fn certificate_pin(root: &ServerRoot, spec: &Spec) -> Result<String, GenerateError> {
    let cert = onlyne_net::load_or_create(&root.key_path(), &spec.server.name).map_err(|error| GenerateError::Net(error.to_string()))?;
    Ok(cert.spki_pin)
}

fn key_for_target(out: &Path, template: &Template, role: &str, force: bool) -> Result<KeyPair, GenerateError> {
    let key_path = out.join(&template.relative).join(".onlyne/keys/role.key");
    if force && key_path.is_file() {
        return KeyPair::load(&key_path).map_err(|error| GenerateError::Net(error.to_string()));
    }
    if key_path.is_file() && !force {
        return Err(GenerateError::Template(TemplateError::Io { path: key_path, source: io::Error::new(io::ErrorKind::AlreadyExists, format!("onlyne: refusing to overwrite {}; pass --force", out.display())) }));
    }
    let _ = role;
    Ok(KeyPair::generate())
}

fn client_config_value(role: &ClientEntry, spec: &Spec, pin: &str) -> toml::Value {
    let (host, port) = spec.server.listen.rsplit_once(':').map(|(host, port)| (host.trim_matches(['[', ']']), port.parse::<u16>().unwrap_or(0))).unwrap_or((spec.server.listen.as_str(), 0));
    toml::toml! {
        role = role.role.clone()
        cert_pin = pin
        key_path = "keys/role.key"
        plugins = []
        [server]
        host = host
        port = port
    }
}

fn render_fragment(role: &ClientEntry) -> String {
    let mut value = toml::Value::try_from(role).unwrap_or_else(|_| toml::Value::Table(Default::default()));
    if let toml::Value::Table(table) = &mut value {
        table.remove("aggregate");
    }
    format!("[[client]]\n{}", toml::to_string(&value).unwrap_or_default())
}

fn package_name(path: &str) -> Option<String> {
    if path.is_empty() { None } else { Path::new(path).file_name().and_then(|name| name.to_str()).map(ToString::to_string) }
}

fn copy_package(source: &Path, target: &Path) -> io::Result<()> {
    if !source.is_dir() { return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not a directory", source.display()))); }
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if text == ".onlyne" || text == "target" || text == ".git" { continue; }
        let destination = target.join(&name);
        if entry.file_type()?.is_dir() { copy_package(&entry.path(), &destination)?; } else { fs::copy(entry.path(), destination)?; }
    }
    Ok(())
}

fn replace_bytes(source: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() { return source.to_vec(); }
    let mut out = Vec::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].windows(needle.len()).position(|window| window == needle) {
        let start = cursor + offset;
        out.extend_from_slice(&source[cursor..start]);
        out.extend_from_slice(replacement);
        cursor = start + needle.len();
    }
    out.extend_from_slice(&source[cursor..]);
    out
}

fn ensure_dir(path: &Path, created: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.exists() { return Ok(()); }
    fs::create_dir_all(path)?;
    created.push(path.to_path_buf());
    Ok(())
}
fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    fs::write(path, bytes)
}
fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() { Ok(path.to_path_buf()) } else { std::env::current_dir().map(|cwd| cwd.join(path)) }
}
fn path_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

pub fn cli_generate(args: GenerateArgs) -> i32 {
    let spec_path = ServerRoot::resolve(&args.root).spec_path();
    let spec = match Spec::load(&spec_path) {
        Ok(spec) => spec,
        Err(error) => { eprintln!("{error}"); return 4; }
    };
    match generate(&args, &spec) {
        Ok(report) => { print!("{}", report.fragment); 0 }
        Err(error) => { eprintln!("{error}"); error.exit_code() }
    }
}
