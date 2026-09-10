use chrono::Utc;
use onlyne_config::template::{
    Placeholders, Template, TemplateError, discover, load_tree, local_override, merge_fragment,
    scan_for_prefixes, substitute_at,
};
use onlyne_config::{ClientEntry, Spec};
use onlyne_layout::{ServerRoot, apply_private_mode};
use onlyne_net::KeyPair;
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    WorkspaceExists(PathBuf),
    Io { path: PathBuf, source: io::Error },
    Net(String),
    Config(String),
}

impl GenerateError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Template(error) => error.exit_code(),
            Self::WorkspaceExists(_) => 4,
            Self::Io { .. } | Self::Net(_) | Self::Config(_) => 4,
        }
    }
}

impl std::fmt::Display for GenerateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Template(error) => error.fmt(f),
            Self::WorkspaceExists(path) => write!(
                f,
                "onlyne: workspace exists at {}; pass --force to overwrite",
                path.display()
            ),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Net(message) | Self::Config(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for GenerateError {}
impl From<TemplateError> for GenerateError {
    fn from(value: TemplateError) -> Self {
        Self::Template(value)
    }
}

pub struct PendingRole {
    role: ClientEntry,
    target: PathBuf,
    template: Template,
    files: Vec<(String, Vec<u8>)>,
    config: Vec<u8>,
    public_key: String,
    key: KeyPair,
    key_exists: bool,
}

pub fn generate(args: &GenerateArgs, spec: &Spec) -> Result<GenerateReport, GenerateError> {
    let root = absolute(&args.root).map_err(|source| GenerateError::Io {
        path: args.root.clone(),
        source,
    })?;
    let server_root = ServerRoot::resolve(&root);
    server_root
        .bootstrap()
        .map_err(|source| GenerateError::Io {
            path: server_root.dir().to_path_buf(),
            source,
        })?;
    let template_root = server_root.templates_root().to_path_buf();
    let out =
        absolute(args.out.as_deref().unwrap_or(&server_root.ws_root())).map_err(|source| {
            GenerateError::Io {
                path: args.out.clone().unwrap_or_else(|| server_root.ws_root()),
                source,
            }
        })?;

    let cert_pin = certificate_pin(&server_root, spec)?;
    let agent = agent_name(spec.server.agent_package.as_str())?;

    let role_filter = |entry: &ClientEntry| {
        args.roles.is_empty() || args.roles.iter().any(|wanted| wanted == &entry.role)
    };
    let mut pending = Vec::new();
    for entry in spec.client.iter().filter(|entry| role_filter(entry)) {
        let template = choose_template(&template_root, &entry.role, &args.templates)?;
        let target = out.join(&template.relative);
        let key_path = target.join(".onlyne/keys/role.key");
        let key_exists = key_path.is_file();
        let key = if key_exists {
            KeyPair::load(&key_path).map_err(|error| GenerateError::Net(error.to_string()))?
        } else {
            KeyPair::generate()
        };
        let public_key = key.public_str();
        let mut role = entry.clone();
        role.key = public_key.clone();
        let placeholders = Placeholders {
            role: role.role.clone(),
            cluster: spec.server.name.clone(),
            server_name: spec.server.name.clone(),
            listen: spec.server.listen.clone(),
            cert_pin: cert_pin.clone(),
            admin: role.admin.to_string(),
            max_sessions: role.max_sessions.to_string(),
            agent_package: agent.as_ref().map(|name| format!(".onlyne/agent/{name}")),
        };
        let mut source_files = load_tree(&template)?;
        source_files.extend(load_dot_pi(&template.role_dir).map_err(|source| {
            GenerateError::Io {
                path: template.role_dir.join(".pi"),
                source,
            }
        })?);
        let mut files = Vec::with_capacity(source_files.len());
        for (relative, bytes) in source_files {
            let path = template.role_dir.join(&relative);
            let substituted = substitute_at(&bytes, &placeholders, path.display().to_string())?;
            if relative == ".pi/settings.json" {
                if let Some(source) =
                    Some(spec.server.agent_package.as_str()).filter(|v| !v.is_empty())
                {
                    if let Some(name) = agent_name(source)? {
                        files.push((
                            relative,
                            replace_bytes(
                                &substituted,
                                source.as_bytes(),
                                format!(".onlyne/agent/{name}").as_bytes(),
                            ),
                        ));
                    } else {
                        files.push((relative, substituted.into_owned()));
                    }
                } else {
                    files.push((relative, substituted.into_owned()));
                }
            } else {
                files.push((relative, substituted.into_owned()));
            }
        }
        let derived = client_config_value(&role, spec, &cert_pin);
        let config_value = local_override(&template)
            .map(|override_value| merge_fragment(&derived, &override_value))
            .unwrap_or(derived);
        let config = toml::to_string(&config_value)
            .map_err(|error| GenerateError::Config(error.to_string()))?
            .into_bytes();
        pending.push(PendingRole {
            target,
            role,
            template,
            files,
            config,
            public_key,
            key,
            key_exists,
        });
    }
    if pending.is_empty() {
        return Err(TemplateError::NoRoleMatches.into());
    }

    for item in &pending {
        if item.target.exists() && !args.force {
            return Err(GenerateError::WorkspaceExists(item.target.clone()));
        }
    }

    let mut created_files = Vec::new();
    let mut created_dirs = Vec::new();
    for item in &pending {
        ensure_dir(&item.target, &mut created_dirs).map_err(|source| GenerateError::Io {
            path: item.target.clone(),
            source,
        })?;
        let onlyne = item.target.join(".onlyne");
        ensure_dir(&onlyne, &mut created_dirs).map_err(|source| GenerateError::Io {
            path: onlyne.clone(),
            source,
        })?;
        let keys = onlyne.join("keys");
        ensure_dir(&keys, &mut created_dirs).map_err(|source| GenerateError::Io {
            path: keys.clone(),
            source,
        })?;
        let key_path = keys.join("role.key");
        if !item.key_exists {
            item.key
                .save(&key_path)
                .map_err(|error| GenerateError::Net(error.to_string()))?;
            apply_private_mode(&key_path).map_err(|error| GenerateError::Io {
                path: key_path.clone(),
                source: io::Error::other(error.to_string()),
            })?;
            created_files.push(key_path);
        }
        let config_path = onlyne.join("config.toml");
        fs::write(&config_path, &item.config).map_err(|source| GenerateError::Io {
            path: config_path.clone(),
            source,
        })?;
        created_files.push(config_path);
        for (relative, bytes) in &item.files {
            let path = item.target.join(relative);
            if let Some(parent) = path.parent() {
                ensure_dir(parent, &mut created_dirs).map_err(|source| GenerateError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            fs::write(&path, bytes).map_err(|source| GenerateError::Io {
                path: path.clone(),
                source,
            })?;
            created_files.push(path.clone());
        }
        if let Some(name) = &agent {
            if item.files.iter().any(|(_, bytes)| {
                bytes
                    .windows(b".onlyne/agent/".len())
                    .any(|window| window == b".onlyne/agent/")
            }) {
                let source = Path::new(&spec.server.agent_package);
                let package_target = item.target.join(".onlyne/agent").join(name);
                copy_package(
                    source,
                    &package_target,
                    &mut created_files,
                    &mut created_dirs,
                )
                .map_err(|source_error| GenerateError::Io {
                    path: package_target,
                    source: source_error,
                })?;
            }
        }
    }

    let mut scan = Vec::new();
    for item in &pending {
        scan.push((
            item.target
                .join(".onlyne/config.toml")
                .display()
                .to_string(),
            item.config.clone(),
        ));
        for (relative, bytes) in &item.files {
            scan.push((
                item.target.join(relative).display().to_string(),
                bytes.clone(),
            ));
        }
    }
    for path in &created_files {
        if let Ok(bytes) = fs::read(path) {
            scan.push((path.display().to_string(), bytes));
        }
    }
    if let Err(error) = scan_for_prefixes(&scan, &scan_prefixes(&root, &out)) {
        for path in created_files.iter().rev() {
            let _ = fs::remove_file(path);
        }
        for path in created_dirs.iter().rev() {
            let _ = fs::remove_dir(path);
        }
        return Err(error.into());
    }

    let roles = pending
        .iter()
        .map(|item| GeneratedRole {
            role: item.role.role.clone(),
            dir: path_relative(&out, &item.target),
            key: item.public_key.clone(),
            template: item.template.relative.clone(),
        })
        .collect::<Vec<_>>();
    let manifest_value = serde_json::json!({
        "generated_at": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "server_root": root.display().to_string(),
        "roles": roles,
    });
    let manifest = serde_json::to_string(&manifest_value)
        .map_err(|error| GenerateError::Config(error.to_string()))?;
    let manifest_path = out.join(".onlyne-generation.json");
    fs::write(&manifest_path, manifest.as_bytes()).map_err(|source| GenerateError::Io {
        path: manifest_path,
        source,
    })?;
    let fragment = pending
        .iter()
        .map(|item| render_fragment(&item.role))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(GenerateReport {
        fragment,
        manifest,
        roles: serde_json::from_value(manifest_value["roles"].clone()).unwrap_or_default(),
        out,
    })
}

fn choose_template(
    root: &Path,
    role: &str,
    requested: &[String],
) -> Result<Template, GenerateError> {
    if requested.is_empty() {
        let mut matches = discover(root, role)?;
        if matches.is_empty() {
            return Err(TemplateError::NoTemplate {
                role: role.to_string(),
                template_root: root.to_path_buf(),
            }
            .into());
        }
        return Ok(matches.remove(0));
    }
    for wanted in requested {
        let path = root.join(wanted);
        if path.is_dir() {
            let relative = wanted.trim_matches('/').to_string();
            let topology = path
                .parent()
                .and_then(|parent| parent.strip_prefix(root).ok())
                .map(|parent| parent.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            return Ok(Template {
                role_dir: path,
                topology,
                relative,
            });
        }
        if let Ok(matches) = discover(root, role) {
            if let Some(found) = matches
                .iter()
                .find(|candidate| candidate.relative == *wanted)
            {
                return Ok(found.clone());
            }
        }
    }
    Err(TemplateError::NoRoleMatches.into())
}

fn certificate_pin(root: &ServerRoot, spec: &Spec) -> Result<String, GenerateError> {
    let cert = onlyne_net::load_or_create(&root.key_path(), &spec.server.name)
        .map_err(|error| GenerateError::Net(error.to_string()))?;
    Ok(cert.spki_pin)
}

fn client_config_value(role: &ClientEntry, spec: &Spec, pin: &str) -> toml::Value {
    let (host, port) = spec
        .server
        .listen
        .rsplit_once(':')
        .map(|(h, p)| {
            (
                h.trim_matches(['[', ']']).to_string(),
                p.parse::<u16>().unwrap_or(0),
            )
        })
        .unwrap_or((spec.server.listen.clone(), 0));
    let mut server = toml::map::Map::new();
    server.insert("host".to_string(), toml::Value::String(host));
    server.insert("port".to_string(), toml::Value::Integer(i64::from(port)));
    let mut table = toml::map::Map::new();
    table.insert("role".to_string(), toml::Value::String(role.role.clone()));
    table.insert("cert_pin".to_string(), toml::Value::String(pin.to_string()));
    table.insert(
        "key_path".to_string(),
        toml::Value::String("keys/role.key".to_string()),
    );
    table.insert("plugins".to_string(), toml::Value::Array(Vec::new()));
    table.insert("server".to_string(), toml::Value::Table(server));
    toml::Value::Table(table)
}

fn render_fragment(role: &ClientEntry) -> String {
    let mut value =
        toml::Value::try_from(role).unwrap_or_else(|_| toml::Value::Table(Default::default()));
    if let toml::Value::Table(table) = &mut value {
        table.remove("aggregate");
    }
    let mut root = toml::map::Map::new();
    root.insert("client".to_string(), toml::Value::Array(vec![value]));
    let text = toml::to_string(&toml::Value::Table(root)).unwrap_or_default();
    text.trim_start_matches('\n').to_string()
}

fn agent_name(package: &str) -> Result<Option<String>, GenerateError> {
    if package.trim().is_empty() {
        Ok(None)
    } else {
        Path::new(package)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .map(Some)
            .ok_or_else(|| {
                GenerateError::Config("onlyne: agent_package has no file name".to_string())
            })
    }
}

fn copy_package(
    source: &Path,
    target: &Path,
    files: &mut Vec<PathBuf>,
    dirs: &mut Vec<PathBuf>,
) -> io::Result<()> {
    if !source.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", source.display()),
        ));
    }
    ensure_dir(target, dirs)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if text == ".onlyne" || text == "target" || text == ".git" {
            continue;
        }
        let destination = target.join(&name);
        if entry.file_type()?.is_dir() {
            copy_package(&entry.path(), &destination, files, dirs)?;
        } else {
            fs::copy(entry.path(), &destination)?;
            files.push(destination);
        }
    }
    Ok(())
}

fn load_dot_pi(role_dir: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
    let pi = role_dir.join(".pi");
    if !pi.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    collect_pi_files(&pi, Path::new(".pi"), &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn collect_pi_files(
    root: &Path,
    relative: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, io::Error>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let file_type = entry.file_type()?;
        let nested = relative.join(&name);
        if file_type.is_dir() {
            collect_pi_files(&entry.path(), &nested, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let relative_string = nested.to_string_lossy().replace('\\', "/");
        out.push((relative_string, fs::read(entry.path())?));
    }
    Ok(())
}

/// The scan set from `docs/v1-PLAN.md` §11 line 391: `out.canonicalize()` and the
/// server root, plus each given path so a leak of either spelling is caught.
fn scan_prefixes(root: &Path, out: &Path) -> Vec<String> {
    let mut prefixes = Vec::new();
    for path in [root, out] {
        prefixes.push(path.display().to_string());
        if let Ok(canonical) = path.canonicalize() {
            prefixes.push(canonical.display().to_string());
        }
    }
    prefixes.retain(|prefix| !prefix.is_empty() && prefix != "/");
    prefixes.sort();
    prefixes.dedup();
    prefixes
}

fn replace_bytes(source: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return source.to_vec();
    }
    let mut out = Vec::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(offset) = source[cursor..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let start = cursor + offset;
        out.extend_from_slice(&source[cursor..start]);
        out.extend_from_slice(replacement);
        cursor = start + needle.len();
    }
    out.extend_from_slice(&source[cursor..]);
    out
}

fn ensure_dir(path: &Path, created: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let mut missing = Vec::new();
    let mut current = Some(path);
    while let Some(dir) = current {
        if dir.exists() {
            break;
        }
        missing.push(dir.to_path_buf());
        current = dir.parent();
    }
    fs::create_dir_all(path)?;
    for dir in missing.into_iter().rev() {
        created.push(dir);
    }
    Ok(())
}

fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir().map(|cwd| cwd.join(path))
    }
}

fn path_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn cli_generate(args: GenerateArgs) -> i32 {
    let spec_path = ServerRoot::resolve(&args.root).spec_path();
    let spec = match Spec::load(&spec_path) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("{error}");
            return 4;
        }
    };
    match generate(&args, &spec) {
        Ok(report) => {
            print!("{}", report.fragment);
            0
        }
        Err(error) => {
            eprintln!("{error}");
            error.exit_code()
        }
    }
}
