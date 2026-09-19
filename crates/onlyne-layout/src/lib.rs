use std::{
    fmt, io,
    path::{Path, PathBuf},
};

pub mod local_socket;
pub use local_socket::{
    LocalListener, LocalListenerSync, LocalStream, LocalStreamSync, bind_local, bind_local_sync,
    bind_local_sync_poll, bind_socket, bind_tokio, connect_local, connect_local_sync,
    is_verbatim_pipe_path, pipe_name_for,
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

/// Apply `0600` to an existing private file.
///
/// Bootstrap creates `run/` and `keys/` with `0700`. Daemons call this helper
/// after writing key files. Socket privacy is applied at bind time by
/// [`bind_local`] (unix `mode(0o600)`, windows owner-only SDDL), which removes
/// the chmod TOCTOU a post-bind call here would have.
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

/// Bytes available in `sun_path` on unix, including the trailing NUL. macOS
/// allows 104.
///
/// The kernel refuses a longer string, so a bind over the bound fails and
/// `connect()` fails the same way for every client that keeps retrying the
/// spelling it derived — the reported symptoms were a client logging
/// `adapter socket restarting error=bind <path>` on a half-second loop while
/// `onlyne status` kept reporting the role as connected, because the TLS link to
/// the server was healthy and the local half was dead. A generated role
/// workspace nests three levels below its server root
/// (`<root>/.onlyne/ws/<topology>/<role>/.onlyne/run/s`), so a root that is
/// already long carries the canonical socket spelling past 103 bytes.
/// [`SocketEndpoint`] keeps the canonical spelling discoverable through the
/// `run/socket` marker and moves the bound socket to a short derived path once
/// the bound is reached.
pub const UNIX_SOCKET_PATH_MAX: usize = 103;

/// Socket file name inside `run/`, the leaf every layout path has always used.
const SOCKET_FILE_NAME: &str = "s";
/// The role-wide content index beside the session logs, named in plan §5.
pub const CONTENT_INDEX_FILE_NAME: &str = "content.index.jsonl";
/// Marker file name inside `run/` naming the path actually bound.
const SOCKET_MARKER_FILE_NAME: &str = "socket";

/// The owner tree's local socket: the canonical spelling and the one actually
/// bound, kept together so one call presents one answer to both sides.
///
/// `run/s` is the canonical spelling and stays that way; `run/socket` records
/// the choice, so an operator or a client that starts before the daemon finds
/// the live path with one read of a file inside the tree it is already looking
/// at. [`bind_socket`] writes the marker at every
/// start, which makes the marker the truth about a live or last-started daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketEndpoint {
    natural: PathBuf,
    actual: PathBuf,
    marker: PathBuf,
}

impl SocketEndpoint {
    /// Resolve from the owner root and its runtime directory (`<root>/.onlyne/run`).
    ///
    /// One read of the marker is the whole filesystem access, and a failed read
    /// simply means no marker, so resolution is total: a client that starts
    /// before the daemon binds derives the path the daemon will choose.
    ///
    /// A caller that needs the derived spelling to match across the moment the
    /// tree is created resolves after `bootstrap`. The fallback digest covers the
    /// canonical root, and a tree that has just appeared resolves symlinks it
    /// could not resolve before, so `<temp>` reached through `/var` becomes
    /// `/private/var` at creation. The marker written at bind covers the moment
    /// after it.
    pub fn resolve(root: &Path, run_dir: &Path) -> Self {
        let natural = run_dir.join(SOCKET_FILE_NAME);
        let marker = run_dir.join(SOCKET_MARKER_FILE_NAME);
        let actual = resolve_actual(root, &natural, &marker);
        Self {
            natural,
            actual,
            marker,
        }
    }

    /// The canonical spelling, `<run_dir>/s`.
    pub fn natural(&self) -> &Path {
        &self.natural
    }

    /// The path `bind` and `connect` use.
    pub fn actual(&self) -> &Path {
        &self.actual
    }

    /// `<run_dir>/socket`, the file naming [`actual`](Self::actual).
    pub fn marker(&self) -> &Path {
        &self.marker
    }

    /// `true` when the bound path moved off the canonical spelling.
    pub fn short(&self) -> bool {
        self.actual != self.natural
    }

    /// Write `natural`, holding the chosen path inside it, with mode `0600`.
    ///
    /// The file lives beside the socket, so a tree that is group-readable
    /// elsewhere still keeps the endpoint list private to the owner. Windows
    /// derives the NPFS pipe name from `run/s` itself, so publishing there
    /// would add a second file describing one endpoint.
    pub fn publish(&self) -> io::Result<()> {
        write_endpoint_marker(&self.marker, &self.actual)
    }
}

/// Absolute, symlink-free spelling of a path when it exists, its lexical
/// absolute otherwise.
///
/// `std::path::absolute` is purely lexical: it follows no symlink and keeps every
/// `..` it is handed. macOS reaches one temporary tree through `/var` and
/// `/private/var`, so a digest taken over a lexical spelling would split that
/// owner tree across two derived socket directories. Canonicalizing first gives
/// every live tree one spelling. A missing path has nothing to canonicalize, so
/// the fallback collapses `..` against the components in front of them, which
/// keeps `/srv/a/../b` and `/srv/b` one answer before either directory exists.
pub fn absolute_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    collapse_parents(&local_socket::lexical_absolute(path))
}

/// Lexically cancel `..` against a preceding normal component.
///
/// A `..` with nothing to cancel keeps its place, so a path that climbs past its
/// own root still spells one thing every time.
fn collapse_parents(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => match out.components().next_back() {
                Some(std::path::Component::Normal(_)) => {
                    out.pop();
                }
                _ => out.push(component.as_os_str()),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(unix)]
fn resolve_actual(root: &Path, natural: &Path, marker: &Path) -> PathBuf {
    if let Some(published) = marker_endpoint(marker) {
        return published;
    }
    if natural.as_os_str().len() <= UNIX_SOCKET_PATH_MAX {
        return natural.to_path_buf();
    }
    derived_endpoint(root)
}

/// Windows `run/s` is already the marker naming an NPFS pipe, so the canonical
/// spelling is the bound spelling.
#[cfg(not(unix))]
fn resolve_actual(_root: &Path, natural: &Path, _marker: &Path) -> PathBuf {
    natural.to_path_buf()
}

/// The path a previous [`bind_socket`](local_socket::bind_socket) published.
///
/// The content is the absolute chosen path plus a newline. An unreadable or
/// malformed marker falls through to the length rule, which keeps a tree that
/// never held a short endpoint resolving to `run/s`.
#[cfg(unix)]
fn marker_endpoint(marker: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(marker).ok()?;
    let path = Path::new(text.trim());
    path.is_absolute().then(|| path.to_path_buf())
}

/// The short home for a socket whose canonical spelling is over the bound.
///
/// The directory name carries [`short_digest`](local_socket::short_digest) of the
/// canonical root, so one live tree owns one directory, and a root that is
/// still absent falls back to a collapsed lexical spelling, which keeps two
/// callers that resolve before `bootstrap` in agreement. Once the daemon binds,
/// the published marker is what every reader uses, so the guess below only has
/// to be stable for the moment before a first bind.
#[cfg(unix)]
fn derived_endpoint(root: &Path) -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "onlyne-{}",
            local_socket::short_digest(&absolute_path(root))
        ))
        .join(SOCKET_FILE_NAME)
}

#[cfg(unix)]
fn write_endpoint_marker(marker: &Path, actual: &Path) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(marker)?;
    // `mode(0o600)` applies at creation and passes through umask, so the open
    // handle is chmod'd directly: a marker that already exists gets the same
    // privacy, and the file is never briefly group-readable.
    let mut permissions = file.metadata()?.permissions();
    permissions.set_mode(0o600);
    file.set_permissions(permissions)?;
    file.write_all(actual.as_os_str().as_encoded_bytes())?;
    file.write_all(b"\n")
}

#[cfg(not(unix))]
fn write_endpoint_marker(_marker: &Path, _actual: &Path) -> io::Result<()> {
    Ok(())
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

    /// The socket to bind and connect, per [`ServerRoot::socket_endpoint`].
    pub fn socket_path(&self) -> PathBuf {
        self.socket_endpoint().actual().to_path_buf()
    }

    /// The canonical socket spelling, `<root>/.onlyne/run/s`.
    pub fn socket_path_natural(&self) -> PathBuf {
        self.run_dir().join(SOCKET_FILE_NAME)
    }

    /// Canonical spelling, bound path, and the marker naming it. See
    /// [`UNIX_SOCKET_PATH_MAX`] for the bound that moves the socket off
    /// `run/s`.
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
    /// `run/` and `keys/` are `0700`. [`bind_socket`] applies `0600` to the
    /// socket it creates, the derived short endpoint included; call
    /// [`apply_private_mode`] after writing `key_path()`.
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
    pub fn socket_path_natural(&self) -> PathBuf {
        self.run_dir().join(SOCKET_FILE_NAME)
    }

    /// Canonical spelling, bound path, and the marker naming it. A generated
    /// role workspace sits three levels below its server root, which is the
    /// shape that reaches [`UNIX_SOCKET_PATH_MAX`].
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
    /// `run/` and `keys/` are `0700`. [`bind_socket`] applies `0600` to the
    /// socket it creates, the derived short endpoint included; call
    /// [`apply_private_mode`] after writing `key_path()`.
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

/// Create a directory, optionally setting its mode.
///
/// `pub(crate)` so [`bind_socket`](local_socket::bind_socket) creates `run/`
/// with the same `0700` a `bootstrap` creates, keeping one convention for the
/// directories that hold private endpoints.
pub(crate) fn create_dir(path: &Path, mode: Option<u32>) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    if let Some(mode) = mode {
        set_dir_mode(path, mode)?;
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn set_dir_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
pub(crate) fn set_dir_mode(_path: &Path, _mode: u32) -> io::Result<()> {
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

    /// A root long enough that `<root>/.onlyne/run/s` passes the unix bound.
    #[cfg(unix)]
    fn deep_root(tmp: &Path) -> PathBuf {
        tmp.join("r".repeat(120))
    }

    #[cfg(unix)]
    #[test]
    fn short_root_binds_the_canonical_spelling_without_a_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let role = RoleWorkspace::resolve(tmp.path());
        role.bootstrap().unwrap();
        assert_eq!(role.socket_path_natural(), tmp.path().join(".onlyne/run/s"));
        assert!(role.socket_path_natural().as_os_str().len() <= UNIX_SOCKET_PATH_MAX);
        let endpoint = role.socket_endpoint();
        assert!(!endpoint.short());
        assert_eq!(endpoint.actual(), endpoint.natural());
        assert_eq!(role.socket_path(), role.socket_path_natural());
        assert_eq!(endpoint.marker(), tmp.path().join(".onlyne/run/socket"));
        // A bootstrap writes no marker, so the length rule is the whole answer
        // for a short tree, and server and role derive the same path for one
        // root.
        assert!(!endpoint.marker().exists());
        assert_eq!(
            ServerRoot::resolve(tmp.path()).socket_path(),
            role.socket_path()
        );
    }

    #[cfg(unix)]
    #[test]
    fn deep_root_binds_a_short_derived_endpoint() {
        let root = deep_root(tempfile::tempdir().unwrap().path());
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
        assert!(
            endpoint.actual().starts_with(std::env::temp_dir()),
            "{}",
            endpoint.actual().display()
        );
        assert!(
            endpoint.actual().as_os_str().len() <= UNIX_SOCKET_PATH_MAX,
            "{} is {} bytes",
            endpoint.actual().display(),
            endpoint.actual().as_os_str().len()
        );
        let leaf = endpoint
            .actual()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(leaf, "s");
        let dir = endpoint
            .actual()
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(dir.starts_with("onlyne-"), "{dir}");
        assert_eq!(dir.len(), "onlyne-".len() + 16, "{dir}");
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
            local_socket::short_digest(&absolute_path(&root)),
            local_socket::short_digest(&absolute_path(&root.join("child/..")))
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

    #[cfg(unix)]
    #[test]
    fn published_marker_wins_over_the_length_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let root = deep_root(tmp.path());
        let layout = RoleWorkspace::resolve(&root);
        fs::create_dir_all(layout.run_dir()).unwrap();
        let endpoint = layout.socket_endpoint();
        assert!(endpoint.short());
        endpoint.publish().unwrap();
        assert_eq!(
            fs::read_to_string(endpoint.marker()).unwrap(),
            format!("{}\n", endpoint.actual().display())
        );
        assert_eq!(
            fs::metadata(endpoint.marker())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            RoleWorkspace::resolve(&root).socket_endpoint().actual(),
            endpoint.actual()
        );
        // A marker naming a path past the bound still wins: the daemon that
        // published it is the authority about its own endpoint, and a client
        // that re-derived a shorter guess would reach a socket nobody holds.
        let overlong = tmp.path().join("x".repeat(120)).join("s");
        fs::write(endpoint.marker(), format!("{}\n", overlong.display())).unwrap();
        let reread = RoleWorkspace::resolve(&root).socket_endpoint();
        assert_eq!(reread.actual(), overlong.as_path());
        assert!(reread.actual().as_os_str().len() > UNIX_SOCKET_PATH_MAX);
        assert!(reread.short());
        // The same precedence holds for a short tree, where the length rule on
        // its own would have answered `run/s`.
        let short_root = tmp.path().join("short");
        let short = ServerRoot::resolve(&short_root);
        fs::create_dir_all(short.run_dir()).unwrap();
        assert_eq!(short.socket_path(), short.socket_path_natural());
        fs::write(
            short.socket_endpoint().marker(),
            format!("{}\n", overlong.display()),
        )
        .unwrap();
        assert_eq!(short.socket_path(), overlong.as_path());
    }

    #[cfg(unix)]
    #[test]
    fn publish_replaces_every_byte_of_the_marker_it_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ServerRoot::resolve(tmp.path().join("short"));
        fs::create_dir_all(layout.run_dir()).unwrap();
        let endpoint = layout.socket_endpoint();
        assert!(!endpoint.short());
        // A tree can hold a marker naming a longer path than the one its daemon
        // settles on: a `--socket` override, or a root that moved deeper and
        // came back. The republished path has to arrive with the old bytes gone,
        // since `trim` would keep a tail presenting a path no socket holds.
        let longer = tmp.path().join("w".repeat(140)).join("s");
        fs::write(endpoint.marker(), format!("{}\n", longer.display())).unwrap();
        endpoint.publish().unwrap();
        assert_eq!(
            fs::read_to_string(endpoint.marker()).unwrap(),
            format!("{}\n", endpoint.actual().display())
        );
        assert_eq!(layout.socket_path(), layout.socket_path_natural());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_or_relative_marker_falls_through_to_the_length_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ServerRoot::resolve(tmp.path());
        fs::create_dir_all(layout.run_dir()).unwrap();
        let binding = layout.socket_endpoint();
        let marker = binding.marker();
        for content in ["", "   \n", "relative/run/s\n", "\n"] {
            fs::write(marker, content).unwrap();
            assert_eq!(
                layout.socket_path(),
                layout.socket_path_natural(),
                "content {content:?}"
            );
        }
    }

    #[test]
    fn absolute_path_canonicalizes_an_existing_tree_and_stays_lexical_without_one() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir_all(&target).unwrap();
        #[cfg(unix)]
        {
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert_eq!(absolute_path(&link), fs::canonicalize(&target).unwrap());
        }
        let missing = absolute_path(Path::new("onlyne-layout-missing/deep/s"));
        assert!(missing.is_absolute());
        assert!(missing.ends_with("onlyne-layout-missing/deep/s"));
        assert_eq!(
            missing,
            local_socket::lexical_absolute(Path::new("onlyne-layout-missing/deep/s"))
        );
        // Nothing has to exist for one tree to get one spelling.
        assert_eq!(
            absolute_path(Path::new("a/../b")),
            absolute_path(Path::new("b"))
        );
    }

    /// The reported failure straddles the bound, so one byte is the difference
    /// between a socket in place and a client that retries forever.
    #[cfg(unix)]
    #[test]
    fn one_byte_past_the_bound_moves_the_socket() {
        let cases = [
            (UNIX_SOCKET_PATH_MAX, false),
            (UNIX_SOCKET_PATH_MAX + 1, true),
        ];
        for (target, expect_short) in cases {
            let (root, run_dir) = run_dir_with_natural_length(target);
            let endpoint = SocketEndpoint::resolve(&root, &run_dir);
            let natural = endpoint.natural();
            assert_eq!(natural.as_os_str().len(), target, "{}", natural.display());
            assert_eq!(endpoint.short(), expect_short, "{}", natural.display());
            assert!(
                endpoint.actual().as_os_str().len() <= UNIX_SOCKET_PATH_MAX,
                "{}",
                endpoint.actual().display()
            );
        }
    }

    /// A root plus a `run` directory whose natural socket path is `target` bytes.
    #[cfg(unix)]
    fn run_dir_with_natural_length(target: usize) -> (PathBuf, PathBuf) {
        let root = PathBuf::from("/onlyne-layout-bound");
        let head = root.join(".onlyne/run");
        let pad = target - head.as_os_str().len() - 3;
        (root, head.join("a".repeat(pad)))
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
