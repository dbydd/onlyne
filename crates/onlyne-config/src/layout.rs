//! The workspace trees: where a server root and a role workspace keep their
//! files, how they are created, and what makes a tree too old to serve.
//!
//! [`ServerRoot`] is a server's `<root>/.onlyne/`; [`RoleWorkspace`] is a role's
//! `<workspace>/.onlyne/`. Both resolve by walking upward for their own marker
//! file, both bootstrap the documented directories with their modes, and both
//! refuse a pre-v1 tree before creating anything.
//!
//! The socket a tree serves is not decided here: [`SocketEndpoint`] owns the
//! machine-level runtime path (`/tmp/onlyne-<uid>/<digest>.sock`), the canonical
//! `run/s` spelling kept for operators, and the `<digest>.json` registration
//! that names the surface serving it.

use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use onlyne_wire::socket::{SOCKET_FILE_NAME, SocketEndpoint, create_dir};

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
    // Rejection-path marker only: this name identifies the pre-v1 `io_cursors` table that v1 refuses (plan §2 line 108).
    if tables.iter().any(|name| name == "io_cursors") {
        return Some(LegacyReason::IoCursorsTable);
    }
    // Rejection-path marker only: this name identifies the pre-v1 `loopback_idempotency` table that v1 refuses (plan §2 line 108).
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

/// The role-wide content index beside the session logs, named in plan §5.
pub const CONTENT_INDEX_FILE_NAME: &str = "content.index.jsonl";

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

    /// The socket to bind and connect, per [`ServerRoot::socket_endpoint`].
    pub fn socket_path(&self) -> PathBuf {
        self.socket_endpoint().actual().to_path_buf()
    }

    /// The canonical socket spelling, `<root>/.onlyne/run/s`.
    ///
    /// Print-only: v2 binds in the machine-level runtime directory, and nothing
    /// is created under `run/`.
    pub fn socket_path_natural(&self) -> PathBuf {
        self.run_dir().join(SOCKET_FILE_NAME)
    }

    /// Canonical spelling, runtime path, and the registration naming it.
    pub fn socket_endpoint(&self) -> SocketEndpoint {
        SocketEndpoint::resolve(&self.root, &self.run_dir())
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
    /// `run/` and `keys/` are `0700`. The socket lives in the machine-level
    /// runtime directory, which [`bind_socket`] creates and verifies; call
    /// [`apply_private_mode`](onlyne_wire::socket::apply_private_mode) after writing `key_path()`.
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

    /// The socket to bind and connect, per [`RoleWorkspace::socket_endpoint`].
    pub fn socket_path(&self) -> PathBuf {
        self.socket_endpoint().actual().to_path_buf()
    }

    /// The canonical socket spelling, `<workspace>/.onlyne/run/s`.
    ///
    /// Print-only: v2 binds in the machine-level runtime directory, and nothing
    /// is created under `run/`.
    pub fn socket_path_natural(&self) -> PathBuf {
        self.run_dir().join(SOCKET_FILE_NAME)
    }

    /// Canonical spelling, runtime path, and the registration naming it. A
    /// generated role workspace sits three levels below its server root, which
    /// is why v2 keeps the socket out of the tree altogether.
    pub fn socket_endpoint(&self) -> SocketEndpoint {
        SocketEndpoint::resolve(&self.root, &self.run_dir())
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.onlyne.join("logs")
    }

    pub fn log_path(&self) -> PathBuf {
        self.logs_dir().join("client.log")
    }

    /// The rendered transcript of one session, the file an operator tails.
    pub fn session_log_path(&self, task_id: &str) -> PathBuf {
        self.logs_dir().join(format!("session-{task_id}.log"))
    }

    /// The raw update journal of one session, one JSON line per backend event.
    pub fn session_events_path(&self, task_id: &str) -> PathBuf {
        self.logs_dir()
            .join(format!("session-{task_id}.events.jsonl"))
    }

    /// The role-wide content index named in plan §5.
    pub fn content_index_path(&self) -> PathBuf {
        self.logs_dir().join(CONTENT_INDEX_FILE_NAME)
    }

    /// The report file one session closes its task through.
    pub fn report_path(&self, task_id: &str) -> PathBuf {
        self.out_dir().join(format!("{task_id}.md"))
    }

    /// Directory holding the closing reports of this role's sessions.
    pub fn out_dir(&self) -> PathBuf {
        self.onlyne.join("out")
    }

    pub fn keys_dir(&self) -> PathBuf {
        self.onlyne.join("keys")
    }

    pub fn key_path(&self) -> PathBuf {
        self.keys_dir().join("role.key")
    }

    /// The role key a workspace config names.
    ///
    /// `generate` writes `keys/role.key`, which is relative to the `.onlyne`
    /// directory the config itself sits in, so a workspace stays valid after it
    /// moves; `init` writes the absolute path, which resolves to the same file.
    pub fn resolve_key_path(&self, configured: &str) -> PathBuf {
        let path = Path::new(configured);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.dir().join(path)
        }
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
    /// `run/` and `keys/` are `0700`. The socket lives in the machine-level
    /// runtime directory, which [`bind_socket`] creates and verifies; call
    /// [`apply_private_mode`](onlyne_wire::socket::apply_private_mode) after writing `key_path()`.
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
    // Rejection-path markers only: these names identify pre-v1 tables that this text scan refuses (plan §2 line 108).
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

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_wire::socket::{
        SOCKET_SUFFIX, UNIX_SOCKET_PATH_MAX, absolute_path, apply_private_mode, runtime_dir,
        socket_path, workspace_digest,
    };
    use std::collections::BTreeSet;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
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
        assert_eq!(layout.socket_path_natural(), root.join(".onlyne/run/s"));
        assert_eq!(layout.socket_path(), socket_path(&root).unwrap());
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
        assert_eq!(layout.socket_path_natural(), root.join(".onlyne/run/s"));
        assert_eq!(layout.socket_path(), socket_path(&root).unwrap());
        assert_eq!(layout.log_path(), root.join(".onlyne/logs/client.log"));
        assert_eq!(layout.key_path(), root.join(".onlyne/keys/role.key"));
        // Both spellings a config can carry reach the one file `generate` and
        // `init` write, which is what makes a generated tree movable.
        assert_eq!(
            layout.resolve_key_path("keys/role.key"),
            root.join(".onlyne/keys/role.key")
        );
        assert_eq!(
            layout.resolve_key_path("/elsewhere/keys/role.key"),
            PathBuf::from("/elsewhere/keys/role.key")
        );
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

    /// A root long enough that `<root>/.onlyne/run/s` passes the unix bound.
    #[cfg(unix)]
    fn deep_root(tmp: &Path) -> PathBuf {
        tmp.join("r".repeat(120))
    }

    #[cfg(unix)]
    #[test]
    fn a_short_root_answers_the_runtime_path() {
        let tmp = tempfile::tempdir().unwrap();
        let role = RoleWorkspace::resolve(tmp.path());
        role.bootstrap().unwrap();
        assert_eq!(role.socket_path_natural(), tmp.path().join(".onlyne/run/s"));
        assert!(role.socket_path_natural().as_os_str().len() <= UNIX_SOCKET_PATH_MAX);
        let endpoint = role.socket_endpoint();
        assert!(endpoint.short(), "v2 binds off the canonical spelling");
        assert_eq!(endpoint.natural(), role.socket_path_natural().as_path());
        assert_eq!(endpoint.actual(), socket_path(&role.root()).unwrap());
        assert_eq!(role.socket_path(), endpoint.actual().to_path_buf());
        // A bootstrap writes nothing into the tree, and a bind publishes no
        // registration on its own: the serving side owns the surface.
        assert!(!role.run_dir().join("socket").exists());
        assert!(!endpoint.registration().exists());
        assert_eq!(
            ServerRoot::resolve(tmp.path()).socket_path(),
            role.socket_path()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_deep_root_answers_the_same_runtime_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = deep_root(tmp.path());
        let layout = RoleWorkspace::resolve(&root);
        let natural = layout.socket_path_natural();
        assert!(
            natural.as_os_str().len() > UNIX_SOCKET_PATH_MAX,
            "{} is {} bytes",
            natural.display(),
            natural.as_os_str().len()
        );
        let endpoint = layout.socket_endpoint();
        assert!(endpoint.short());
        assert_eq!(endpoint.natural(), natural.as_path());
        assert_eq!(endpoint.actual().parent().unwrap(), runtime_dir().unwrap());
        assert_eq!(
            endpoint.actual().file_name().unwrap().to_str().unwrap(),
            format!("{}{SOCKET_SUFFIX}", workspace_digest(&root))
        );
        assert!(
            endpoint.actual().as_os_str().len() <= UNIX_SOCKET_PATH_MAX,
            "{} is {} bytes",
            endpoint.actual().display(),
            endpoint.actual().as_os_str().len()
        );
        // Length is a property of the runtime directory, not of the root: the
        // rule that moved deep trees off `run/s` is the rule v2 deleted.
        let short = ServerRoot::resolve(tmp.path()).socket_endpoint();
        assert_eq!(short.actual().parent(), endpoint.actual().parent());
        assert_eq!(
            short.actual().as_os_str().len(),
            endpoint.actual().as_os_str().len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_is_deterministic_and_one_tree_yields_one_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = deep_root(tmp.path());
        fs::create_dir_all(root.join("child")).unwrap();
        let layout = ServerRoot::resolve(&root);
        assert_eq!(
            layout.socket_endpoint(),
            ServerRoot::resolve(&root).socket_endpoint()
        );
        let endpoint = layout.socket_endpoint();
        assert!(endpoint.short());
        // A spelling that walks back out of a child names the same tree, so it
        // reaches the same derived directory.
        let through_child = RoleWorkspace::resolve(root.join("child/..")).socket_endpoint();
        assert_eq!(through_child.actual(), endpoint.actual());
        assert_eq!(
            workspace_digest(&root),
            workspace_digest(&root.join("child/.."))
        );
        // An absent root still resolves, twice alike, off the lexical
        // spelling, and a `..` in that spelling cancels before any directory
        // exists to canonicalize.
        let missing = Path::new("/onlyne-layout-absent-root-9a7f")
            .join("a")
            .join("z".repeat(120));
        let through_child = missing.join("child/..");
        assert_eq!(absolute_path(&missing), absolute_path(&through_child));
        let first = RoleWorkspace::resolve(&missing).socket_endpoint();
        let second = RoleWorkspace::resolve(&through_child).socket_endpoint();
        assert_eq!(first.actual(), second.actual());
        assert_eq!(RoleWorkspace::resolve(&missing).socket_endpoint(), first);
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
