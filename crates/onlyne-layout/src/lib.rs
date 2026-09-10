use std::{
    fmt, io,
    path::{Path, PathBuf},
};

/// Exit code for binaries that refuse a legacy workspace layout.
pub const LEGACY_WORKSPACE_EXIT_CODE: i32 = 2;

/// Byte-exact legacy refusal text printed by binaries before exit 2.
pub const LEGACY_WORKSPACE_MESSAGE: &str =
    "onlyne: legacy workspace layout; v1.0.0 does not migrate";

/// Legacy marker that makes v1 bootstrap abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyReason {
    /// `.onlyne/channels/` belongs to the pre-v1 layout.
    ChannelsDirectory,
    /// `state.db` carries the pre-v1 `io_cursors` table marker.
    IoCursorsTable,
    /// `state.db` carries the pre-v1 `loopback_idempotency` table marker.
    LoopbackIdempotencyTable,
}

impl fmt::Display for LegacyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelsDirectory => f.write_str("channels directory"),
            Self::IoCursorsTable => f.write_str("io_cursors table"),
            Self::LoopbackIdempotencyTable => f.write_str("loopback_idempotency table"),
        }
    }
}

/// Errors produced by layout helpers.
#[derive(Debug)]
pub enum LayoutError {
    /// Filesystem operation failed for a concrete path.
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Apply `0600` to an existing private file or bound socket path.
///
/// Bootstrap creates `run/` and `keys/` with `0700`. Daemons call this helper
/// immediately after `UnixListener::bind` and after writing key files. The
/// directory mode plus this call protects sockets and keys at creation time.
pub fn apply_private_mode(path: &Path) -> Result<(), LayoutError> {
    set_file_mode(path, 0o600).map_err(|source| LayoutError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Detect a pre-v1 workspace with the default SQLite marker scanner.
pub fn detect_legacy(dir: &Path) -> Option<LegacyReason> {
    detect_legacy_with_probe(dir, &sqlite_master_text_scan)
}

/// Detect a pre-v1 workspace with an injected table-name opener.
///
/// The default probe reads the SQLite file bytes and searches for the table-name
/// strings that SQLite stores in `sqlite_master`. This avoids bringing SQLite
/// into a leaf layout crate. It is a conservative marker scan for v1 bootstrap;
/// callers that already have SQLite available can inject a real
/// `select name from sqlite_master` opener.
pub fn detect_legacy_with_probe(
    dir: &Path,
    probe: &dyn Fn(&Path) -> io::Result<Vec<String>>,
) -> Option<LegacyReason> {
    let onlyne = dir.join(".onlyne");
    if onlyne.join("channels").is_dir() {
        return Some(LegacyReason::ChannelsDirectory);
    }
    let db_path = onlyne.join("state.db");
    let tables = probe(&db_path).unwrap_or_default();
    if tables.iter().any(|name| name == "io_cursors") {
        return Some(LegacyReason::IoCursorsTable);
    }
    if tables.iter().any(|name| name == "loopback_idempotency") {
        return Some(LegacyReason::LoopbackIdempotencyTable);
    }
    None
}

/// Print the legacy refusal message and terminate the process.
pub fn exit_legacy_workspace() -> ! {
    eprintln!("{LEGACY_WORKSPACE_MESSAGE}");
    std::process::exit(LEGACY_WORKSPACE_EXIT_CODE);
}

/// Server-side root at `<root>/.onlyne/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRoot {
    root: PathBuf,
    onlyne: PathBuf,
    template_root: PathBuf,
    ws_name: PathBuf,
}

/// Role-side workspace at `<workspace>/.onlyne/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleWorkspace {
    root: PathBuf,
    onlyne: PathBuf,
}

/// Plain strings accepted from a config-shaped server section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerLayoutSpec {
    /// Directory containing templates, relative to root unless absolute.
    pub template_root: String,
    /// Directory containing generated workspaces, relative to `.onlyne` unless absolute.
    pub ws_dir: String,
}

impl Default for ServerLayoutSpec {
    fn default() -> Self {
        Self {
            template_root: ".onlyne/templates".to_string(),
            ws_dir: "ws".to_string(),
        }
    }
}

impl ServerRoot {
    /// Resolve a server root directly.
    pub fn resolve(root: impl Into<PathBuf>) -> Self {
        Self::resolve_with_spec(root, &ServerLayoutSpec::default())
    }

    /// Resolve a server root using plain strings from a config-shaped section.
    pub fn resolve_with_spec(root: impl Into<PathBuf>, spec: &ServerLayoutSpec) -> Self {
        let root = root.into();
        let onlyne = root.join(".onlyne");
        Self {
            template_root: resolve_under_root(&root, &spec.template_root),
            ws_name: PathBuf::from(&spec.ws_dir),
            onlyne,
            root,
        }
    }

    /// Discover a server root by walking upward for `.onlyne/spec.toml`.
    pub fn discover(start: impl AsRef<Path>) -> io::Result<Self> {
        let start = start.as_ref().to_path_buf();
        for dir in start.ancestors() {
            if dir.join(".onlyne/spec.toml").is_file() {
                return Ok(Self::resolve(dir));
            }
        }
        Ok(Self::resolve(start))
    }

    /// Root directory containing `.onlyne`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The `.onlyne` directory.
    pub fn dir(&self) -> &Path {
        &self.onlyne
    }

    pub fn spec_path(&self) -> PathBuf {
        self.onlyne.join("spec.toml")
    }

    pub fn state_db_path(&self) -> PathBuf {
        self.onlyne.join("state.db")
    }

    pub fn db_path(&self) -> PathBuf {
        self.state_db_path()
    }

    pub fn run_dir(&self) -> PathBuf {
        self.onlyne.join("run")
    }

    pub fn socket_path(&self) -> PathBuf {
        self.run_dir().join("s")
    }

    pub fn pid_path(&self) -> PathBuf {
        self.run_dir().join("server.pid")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.onlyne.join("logs")
    }

    pub fn log_path(&self) -> PathBuf {
        self.logs_dir().join("server.log")
    }

    pub fn keys_dir(&self) -> PathBuf {
        self.onlyne.join("keys")
    }

    pub fn key_path(&self) -> PathBuf {
        self.keys_dir().join("server.key")
    }

    pub fn templates_dir(&self, topology: &str, role: &str) -> PathBuf {
        self.template_root.join(topology).join(role)
    }

    pub fn templates_root(&self) -> &Path {
        &self.template_root
    }

    pub fn ws_dir(&self, topology: &str, role: &str) -> PathBuf {
        resolve_under_onlyne(&self.onlyne, &self.ws_name)
            .join(topology)
            .join(role)
    }

    pub fn ws_root(&self) -> PathBuf {
        resolve_under_onlyne(&self.onlyne, &self.ws_name)
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.onlyne.join("cache")
    }

    pub fn provision_dir(&self) -> PathBuf {
        self.ws_root()
    }

    /// Create documented directories after refusing legacy markers.
    ///
    /// `run/` and `keys/` are `0700`. Call [`apply_private_mode`] immediately
    /// after binding `socket_path()` and after writing `key_path()`.
    pub fn bootstrap(&self) -> io::Result<()> {
        if detect_legacy(&self.root).is_some() {
            exit_legacy_workspace();
        }
        create_dir(&self.onlyne, None)?;
        create_dir(&self.run_dir(), Some(0o700))?;
        create_dir(&self.logs_dir(), None)?;
        create_dir(&self.keys_dir(), Some(0o700))?;
        create_dir(self.templates_root(), None)?;
        create_dir(&self.ws_root(), None)?;
        create_dir(&self.cache_dir(), None)?;
        Ok(())
    }
}

impl RoleWorkspace {
    /// Resolve a role workspace directly.
    pub fn resolve(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            onlyne: root.join(".onlyne"),
            root,
        }
    }

    /// Discover a role workspace by walking upward for `.onlyne/config.toml`.
    pub fn discover(start: impl AsRef<Path>) -> io::Result<Self> {
        let start = start.as_ref().to_path_buf();
        for dir in start.ancestors() {
            if dir.join(".onlyne/config.toml").is_file() {
                return Ok(Self::resolve(dir));
            }
        }
        Ok(Self::resolve(start))
    }

    /// Workspace root containing `.onlyne`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The `.onlyne` directory.
    pub fn dir(&self) -> &Path {
        &self.onlyne
    }

    pub fn config_path(&self) -> PathBuf {
        self.onlyne.join("config.toml")
    }

    pub fn client_db_path(&self) -> PathBuf {
        self.onlyne.join("client.db")
    }

    pub fn db_path(&self) -> PathBuf {
        self.client_db_path()
    }

    pub fn run_dir(&self) -> PathBuf {
        self.onlyne.join("run")
    }

    pub fn socket_path(&self) -> PathBuf {
        self.run_dir().join("s")
    }

    pub fn pid_path(&self) -> PathBuf {
        self.run_dir().join("client.pid")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.onlyne.join("logs")
    }

    pub fn log_path(&self) -> PathBuf {
        self.logs_dir().join("client.log")
    }

    pub fn keys_dir(&self) -> PathBuf {
        self.onlyne.join("keys")
    }

    pub fn key_path(&self) -> PathBuf {
        self.keys_dir().join("role.key")
    }

    pub fn agent_dir(&self, package: &str) -> PathBuf {
        self.onlyne.join("agent").join(package)
    }

    pub fn agent_root(&self) -> PathBuf {
        self.onlyne.join("agent")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.onlyne.join("cache")
    }

    pub fn templates_dir(&self, topology: &str, role: &str) -> PathBuf {
        self.onlyne.join("templates").join(topology).join(role)
    }

    pub fn ws_dir(&self, topology: &str, role: &str) -> PathBuf {
        self.onlyne.join("ws").join(topology).join(role)
    }

    pub fn provision_dir(&self) -> PathBuf {
        self.agent_root()
    }

    /// Create documented directories after refusing legacy markers.
    ///
    /// `run/` and `keys/` are `0700`. Call [`apply_private_mode`] immediately
    /// after binding `socket_path()` and after writing `key_path()`.
    pub fn bootstrap(&self) -> io::Result<()> {
        if detect_legacy(&self.root).is_some() {
            exit_legacy_workspace();
        }
        create_dir(&self.onlyne, None)?;
        create_dir(&self.run_dir(), Some(0o700))?;
        create_dir(&self.logs_dir(), None)?;
        create_dir(&self.keys_dir(), Some(0o700))?;
        create_dir(&self.agent_root(), None)?;
        Ok(())
    }
}

fn sqlite_master_text_scan(path: &Path) -> io::Result<Vec<String>> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut names = Vec::new();
    for marker in ["io_cursors", "loopback_idempotency"] {
        if text.contains(marker) {
            names.push(marker.to_string());
        }
    }
    Ok(names)
}

fn resolve_under_root(root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn resolve_under_onlyne(onlyne: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        onlyne.join(path)
    }
}

fn create_dir(path: &Path, mode: Option<u32>) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    if let Some(mode) = mode {
        set_dir_mode(path, mode)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_dir_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_dir_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

fn set_file_mode(path: &Path, mode: u32) -> io::Result<()> {
    let _ = std::fs::metadata(path)?;
    set_file_mode_impl(path, mode)
}

#[cfg(unix)]
fn set_file_mode_impl(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_file_mode_impl(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    use std::os::unix::net::UnixListener;
    #[test]
    fn legacy_refusal_message_is_byte_exact_without_trailing_newline() {
        let expected = "onlyne: legacy workspace layout; v1.0.0 does not migrate";
        assert_eq!(LEGACY_WORKSPACE_MESSAGE, expected);
        assert!(!LEGACY_WORKSPACE_MESSAGE.ends_with('\n'));
        assert_eq!(
            LEGACY_WORKSPACE_MESSAGE.as_bytes(),
            b"onlyne: legacy workspace layout; v1.0.0 does not migrate"
        );
        assert_eq!(
            format!("{LEGACY_WORKSPACE_MESSAGE}\n").len(),
            expected.len() + 1
        );
    }

    #[test]
    fn server_paths_are_exact() {
        let root = PathBuf::from("/tmp/cluster-a");
        let layout = ServerRoot::resolve(&root);
        assert_eq!(layout.spec_path(), root.join(".onlyne/spec.toml"));
        assert_eq!(layout.state_db_path(), root.join(".onlyne/state.db"));
        assert_eq!(layout.socket_path(), root.join(".onlyne/run/s"));
        assert_eq!(layout.pid_path(), root.join(".onlyne/run/server.pid"));
        assert_eq!(layout.log_path(), root.join(".onlyne/logs/server.log"));
        assert_eq!(layout.key_path(), root.join(".onlyne/keys/server.key"));
        assert_eq!(
            layout.templates_dir("topology-a", "planner"),
            root.join(".onlyne/templates/topology-a/planner")
        );
        assert_eq!(
            layout.ws_dir("topology-a", "planner"),
            root.join(".onlyne/ws/topology-a/planner")
        );
        assert_eq!(layout.cache_dir(), root.join(".onlyne/cache"));
    }

    #[test]
    fn server_paths_honor_spec_shaped_strings() {
        let root = PathBuf::from("/tmp/cluster-a");
        let layout = ServerRoot::resolve_with_spec(
            &root,
            &ServerLayoutSpec {
                template_root: "templates-custom".to_string(),
                ws_dir: "workspaces-custom".to_string(),
            },
        );
        assert_eq!(
            layout.templates_dir("topology-a", "planner"),
            root.join("templates-custom/topology-a/planner")
        );
        assert_eq!(
            layout.ws_dir("topology-a", "planner"),
            root.join(".onlyne/workspaces-custom/topology-a/planner")
        );
        assert_eq!(
            layout.provision_dir(),
            root.join(".onlyne/workspaces-custom")
        );
    }

    #[test]
    fn role_paths_are_exact() {
        let root = PathBuf::from("/tmp/workspace-a");
        let layout = RoleWorkspace::resolve(&root);
        assert_eq!(layout.config_path(), root.join(".onlyne/config.toml"));
        assert_eq!(layout.client_db_path(), root.join(".onlyne/client.db"));
        assert_eq!(layout.socket_path(), root.join(".onlyne/run/s"));
        assert_eq!(layout.pid_path(), root.join(".onlyne/run/client.pid"));
        assert_eq!(layout.log_path(), root.join(".onlyne/logs/client.log"));
        assert_eq!(layout.key_path(), root.join(".onlyne/keys/role.key"));
        assert_eq!(layout.agent_dir("pi"), root.join(".onlyne/agent/pi"));
    }

    #[test]
    fn discover_walks_up_for_server_and_role_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("a");
        let child = root.join("b/c");
        fs::create_dir_all(root.join(".onlyne")).unwrap();
        fs::create_dir_all(&child).unwrap();
        fs::write(root.join(".onlyne/spec.toml"), "").unwrap();
        fs::write(root.join(".onlyne/config.toml"), "").unwrap();
        assert_eq!(ServerRoot::discover(&child).unwrap().root(), root.as_path());
        assert_eq!(
            RoleWorkspace::discover(&child).unwrap().root(),
            root.as_path()
        );
    }

    #[test]
    fn detect_legacy_channels_directory() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".onlyne/channels")).unwrap();
        assert_eq!(
            detect_legacy(tmp.path()),
            Some(LegacyReason::ChannelsDirectory)
        );
    }

    #[test]
    fn detect_legacy_io_cursors_string_in_db_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".onlyne")).unwrap();
        fs::write(
            tmp.path().join(".onlyne/state.db"),
            b"SQLite format 3\0io_cursors",
        )
        .unwrap();
        assert_eq!(
            detect_legacy(tmp.path()),
            Some(LegacyReason::IoCursorsTable)
        );
    }

    #[test]
    fn detect_legacy_loopback_string_in_db_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".onlyne")).unwrap();
        fs::write(
            tmp.path().join(".onlyne/state.db"),
            b"SQLite format 3\0loopback_idempotency",
        )
        .unwrap();
        assert_eq!(
            detect_legacy(tmp.path()),
            Some(LegacyReason::LoopbackIdempotencyTable)
        );
    }

    #[test]
    fn injected_probe_detects_table_names() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".onlyne")).unwrap();
        fs::write(tmp.path().join(".onlyne/state.db"), b"sample legacy db").unwrap();
        let reason = detect_legacy_with_probe(tmp.path(), &|_| Ok(vec!["io_cursors".to_string()]));
        assert_eq!(reason, Some(LegacyReason::IoCursorsTable));
    }

    #[test]
    fn fresh_directory_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(detect_legacy(tmp.path()), None);
    }

    #[test]
    fn fresh_server_bootstrap_creates_exact_documented_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ServerRoot::resolve(tmp.path());
        layout.bootstrap().unwrap();
        assert_eq!(
            entries(&tmp.path().join(".onlyne")),
            set(["cache", "keys", "logs", "run", "templates", "ws"])
        );
        assert_eq!(entries(&layout.run_dir()), BTreeSet::new());
        assert_eq!(entries(&layout.keys_dir()), BTreeSet::new());
        assert!(!tmp.path().join(".onlyne/channels").exists());
        assert!(!tmp.path().join(".onlyne/adapters").exists());
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(layout.run_dir()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(layout.keys_dir())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn fresh_role_bootstrap_creates_exact_documented_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = RoleWorkspace::resolve(tmp.path());
        layout.bootstrap().unwrap();
        assert_eq!(
            entries(&tmp.path().join(".onlyne")),
            set(["agent", "keys", "logs", "run"])
        );
        assert_eq!(entries(&layout.run_dir()), BTreeSet::new());
        assert_eq!(entries(&layout.keys_dir()), BTreeSet::new());
        assert!(!tmp.path().join(".onlyne/channels").exists());
        assert!(!tmp.path().join(".onlyne/adapters").exists());
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(layout.run_dir()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(layout.keys_dir())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn apply_private_mode_sets_bound_socket_to_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("socket");
        let _listener = UnixListener::bind(&path).unwrap();
        apply_private_mode(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn apply_private_mode_missing_path_names_path() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("missing");
        let error = apply_private_mode(&path).unwrap_err();
        assert!(error.to_string().contains(path.to_string_lossy().as_ref()));
    }

    #[cfg(unix)]
    #[test]
    fn key_directory_is_0700_and_key_file_can_be_made_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = RoleWorkspace::resolve(tmp.path());
        layout.bootstrap().unwrap();
        assert_eq!(
            fs::metadata(layout.keys_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        fs::write(layout.key_path(), b"private key").unwrap();
        apply_private_mode(&layout.key_path()).unwrap();
        assert_eq!(
            fs::metadata(layout.key_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    fn entries(path: &Path) -> BTreeSet<String> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn set(items: impl IntoIterator<Item = &'static str>) -> BTreeSet<String> {
        items.into_iter().map(str::to_string).collect()
    }
}
