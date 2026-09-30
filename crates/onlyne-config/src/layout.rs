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

/// Exit code for binaries that refuse a workspace from an older layout.
///
/// The same code the schema refusal takes, because it is the same situation
/// read from the other end: this build will not start on a tree it did not
/// write, and no command converts one.
pub const LEGACY_WORKSPACE_EXIT_CODE: i32 = onlyne_proto::EXIT_NEEDS_MIGRATION;

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
///
/// The sentence names the marker that decided it, so an operator who has two
/// candidate directories can tell which one was refused without reading the
/// source.
pub fn exit_legacy_workspace(reason: LegacyReason) -> ! {
    eprintln!(
        "{}",
        onlyne_proto::legacy_workspace_message(&[reason.to_string()])
    );
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
        if let Some(reason) = detect_legacy(&self.root) {
            exit_legacy_workspace(reason);
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
        if let Some(reason) = detect_legacy(&self.root) {
            exit_legacy_workspace(reason);
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
