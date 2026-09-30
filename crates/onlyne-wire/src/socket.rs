//! Cross-platform local socket seam, the owner-tree endpoint over it, and the
//! per-user runtime directory that names both.
//!
//! v2 keeps every local socket in one machine-level runtime directory:
//! `$ONLYNE_RUNTIME_DIR` when the operator sets it, `/tmp/onlyne-<uid>/`
//! otherwise, created `0700` and adopted only when this process owns it alone.
//! One workspace owns two files there, both named by [`workspace_digest`] — the
//! first 16 hex characters of `sha256` over the canonical workspace root:
//!
//! - `<digest>.sock`: the socket [`bind_socket_v2`] binds and [`connect_local`]
//!   reaches, `0600`.
//! - `<digest>.json`: the [`RegistrationFile`] naming who serves it — kind,
//!   role, root, pid, version, runtime. [`read_registration`],
//!   [`registration_path`], and [`list_registrations`] are the discovery seam
//!   the CLI, the test harness, and external-runtime plugins read instead of
//!   walking workspace trees.
//!
//! v1 kept the socket at `<run_dir>/s` while that spelling fit `sun_path`, moved
//! it to a short derived path when it did not, and published the choice in
//! `run/socket`. A generated role nests deep enough
//! (`<root>/.onlyne/ws/<topology>/<role>/.onlyne/run/s`) that the canonical
//! spelling passes [`UNIX_SOCKET_PATH_MAX`], so the two-rule answer split one
//! tree across two directories depending on who resolved when. The runtime path
//! is about 40 bytes for any root, so one tree owns one spelling, and
//! `<run_dir>/s` survives only as [`SocketEndpoint::natural`], the spelling
//! operators and older tooling print.
//!
//! The unix base is `/tmp` rather than [`std::env::temp_dir`] because a
//! launchd-started daemon and an interactive shell see different `TMPDIR` and
//! `XDG_RUNTIME_DIR` values; one fixed base makes every context compute the same
//! path for the same root.
//!
//! Windows cannot bind a UDS on stable 1.85, so `<digest>.sock` there is a
//! regular marker file whose contents name an NPFS pipe (`v1:onlyne-<32hex>`)
//! and the pipe carries the traffic. `--socket` values that already start with a
//! verbatim pipe path skip derivation and travel as `GenericFilePath`. Frame I/O
//! stays generic `AsyncRead`/`AsyncWrite`.
//!
//! [`create_dir`], [`set_dir_mode`], and [`apply_private_mode`] are the mode half
//! of the same seam: one spelling of "owner-only `run/`, `keys/`, and runtime
//! directory" for the daemon that binds and the layout that bootstraps.

use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions, ToFsName,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Errors produced by the owner-only mode helpers below.
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
/// [`bind_local`] (unix `mode(0o600)`, windows owner-only SDDL), and
/// registration privacy by [`write_registration`], which removes the chmod
/// TOCTOU a post-write call here would have.
pub fn apply_private_mode(path: &Path) -> Result<(), LayoutError> {
    set_file_mode(path, 0o600).map_err(|source| LayoutError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Bytes available in `sun_path` on unix, including the trailing NUL. macOS
/// allows 104.
///
/// Every v2 runtime path is about 40 bytes, so no caller has to respect this
/// bound any more; the constant stays because it is what v1 worked around. A
/// canonical spelling over it is not a legal name for the kernel: the bind fails
/// and `connect()` fails the same way for every client that keeps retrying the
/// spelling it derived — the reported v1 symptoms were a client logging
/// `adapter socket restarting error=bind <path>` on a half-second loop while
/// `onlyne status` kept reporting the role as connected, because the TLS link to
/// the server was healthy and the local half was dead. A generated role
/// workspace nests three levels below its server root, so a root that was
/// already long carried the canonical socket spelling past 103 bytes.
pub const UNIX_SOCKET_PATH_MAX: usize = 103;

/// Socket file name inside `run/`, the canonical spelling every v1 layout path
/// used. v2 binds elsewhere and keeps this only as
/// [`SocketEndpoint::natural`].
pub const SOCKET_FILE_NAME: &str = "s";

/// Socket leaf inside a runtime directory: `<digest>.sock`.
pub const SOCKET_SUFFIX: &str = ".sock";

/// Registration leaf inside a runtime directory: `<digest>.json`.
pub const REGISTRATION_SUFFIX: &str = ".json";

/// Environment variable that replaces the default runtime directory, used
/// verbatim when set and non-empty.
pub const RUNTIME_DIR_ENV: &str = "ONLYNE_RUNTIME_DIR";

/// The version of the wire protocol this crate speaks, the `version` every
/// [`RegistrationFile`] written here carries, so a reader can tell which
/// protocol a live endpoint expects.
pub const WIRE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The runtime directory, created and owner-verified.
///
/// `$ONLYNE_RUNTIME_DIR` when set and non-empty, `/tmp/onlyne-<uid>/`
/// otherwise. Adoption follows [`ensure_private_dir`]: a directory this user
/// owns that is closed to group and other access, which this process can still
/// write. `/tmp` is shared and world-executable, so another user can create
/// `/tmp/onlyne-<uid>` first; an endpoint inside a directory that user owns is
/// one that user could swap from under a live daemon, which is the refusal
/// [`ensure_private_dir`] makes.
pub fn runtime_dir() -> io::Result<PathBuf> {
    let dir = runtime_dir_path();
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// The runtime directory's spelling without creating or checking anything, so a
/// caller can name a path before any daemon exists (`/tmp` is one component).
pub fn runtime_dir_path() -> PathBuf {
    match std::env::var_os(RUNTIME_DIR_ENV) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => runtime_base().join(format!("onlyne-{}", current_user_id())),
    }
}

/// `/tmp` on unix: fixed so launchd and an interactive shell agree, and short
/// enough that every derived path stays well under [`UNIX_SOCKET_PATH_MAX`].
#[cfg(unix)]
fn runtime_base() -> PathBuf {
    PathBuf::from("/tmp")
}

/// Off unix the per-user temporary root already scopes the directory.
#[cfg(not(unix))]
fn runtime_base() -> PathBuf {
    std::env::temp_dir()
}

/// The effective uid, the runtime directory's name component.
#[cfg(unix)]
fn current_user_id() -> String {
    // SAFETY: `geteuid` takes no arguments, touches no memory, and cannot fail.
    format!("{}", unsafe { libc::geteuid() })
}

/// Off unix the per-user temporary root is the scope: `%TEMP%` sits inside the
/// user profile, and the pipe's owner-only security descriptor guards the
/// endpoint, so no uid is spelled out.
#[cfg(not(unix))]
fn current_user_id() -> String {
    "user".to_string()
}

/// The socket one workspace owns: `<runtime_dir>/<workspace_digest>.sock`.
///
/// The runtime directory is created and verified first, so a caller that gets a
/// path has a private place to bind it in.
pub fn socket_path(root: &Path) -> io::Result<PathBuf> {
    runtime_dir()?;
    Ok(runtime_file_path(root, SOCKET_SUFFIX))
}

/// The registration belonging to [`socket_path`]'s socket,
/// `<runtime_dir>/<workspace_digest>.json`.
///
/// Pure: [`write_registration`] and [`socket_path`] create the directory, so a
/// reader can name the file without claiming the machine has a runtime.
pub fn registration_path(root: &Path) -> PathBuf {
    runtime_file_path(root, REGISTRATION_SUFFIX)
}

/// `<runtime_dir_path>/<workspace_digest(root)><suffix>`, filesystem untouched.
fn runtime_file_path(root: &Path, suffix: &str) -> PathBuf {
    let digest = workspace_digest(root);
    runtime_dir_path().join(format!("{digest}{suffix}"))
}

/// Which side of a socket one [`RegistrationFile`] describes.
///
/// The serving side owns the answer: a server root's daemon publishes
/// [`Server`](Self::Server), and a role workspace's client daemon publishes
/// [`Client`](Self::Client) with its role. No reader infers the surface from
/// what happens to sit beside a tree — two v1 readers did, and disagreed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationKind {
    /// The admin surface of a server root.
    Server,
    /// A role workspace's client daemon.
    Client,
}

/// The one file that names a workspace's endpoint: `<runtime>/<digest>.json`.
///
/// It answers without touching the workspace what `run/socket` answered for a
/// path and what a `state.db`/`client.db` guess answered for a surface: who
/// serves this tree (`kind`, `role`), which tree it is (`root`), which process
/// (`pid`), which wire version, and which runtime hosts the role's sessions
/// (`runtime`, the field external-runtime plugins match to find their clients).
///
/// The writer's facts are the file's facts. A bind does not publish one on its
/// own, because a bind does not know whether it belongs to an admin root or a
/// role workspace: the serving side calls [`write_registration`] or
/// [`SocketEndpoint::publish`] with the surface it knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationFile {
    /// Which side of the socket this process serves.
    pub kind: RegistrationKind,
    /// The role this client serves; `None` for a server root. `null` in JSON.
    #[serde(default)]
    pub role: Option<String>,
    /// The canonical spelling of the tree the socket belongs to.
    pub root: PathBuf,
    /// The pid of the process serving it, restated at every bind.
    pub pid: u32,
    /// The writer's version, [`WIRE_VERSION`] for this crate.
    #[serde(default)]
    pub version: String,
    /// The runtime hosting the role's sessions (`pi`, `acp`, …); `None` for a
    /// server root. `null` in JSON.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Where this machine displays the role's runtime process (`orca`, `zellij`,
    /// `headless`, `external`); `None` for a server root, and for a client that
    /// published its registration before it resolved one.
    ///
    /// Placement is a property of the machine, which is why it is recorded
    /// here: an external runtime's plugin reads the registrations in this
    /// directory and dials the clients whose placement says the runtime is
    /// already resident (`external`).
    #[serde(default)]
    pub placement: Option<String>,
}

impl RegistrationFile {
    /// This process serving `root` as a server root.
    pub fn server(root: &Path) -> Self {
        Self::new(RegistrationKind::Server, root)
    }

    /// This process serving `root` as a role workspace's client daemon.
    pub fn client(root: &Path) -> Self {
        Self::new(RegistrationKind::Client, root)
    }

    /// This process, `WIRE_VERSION`, canonical `root`, no role, no runtime.
    pub fn new(kind: RegistrationKind, root: &Path) -> Self {
        Self {
            kind,
            role: None,
            root: absolute_path(root),
            pid: std::process::id(),
            version: WIRE_VERSION.to_string(),
            runtime: None,
            placement: None,
        }
    }

    /// Name the role this registration serves.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    /// Name the runtime hosting the role's sessions.
    pub fn with_runtime(mut self, runtime: impl Into<String>) -> Self {
        self.runtime = Some(runtime.into());
        self
    }

    /// Name the placement this machine displays the role's sessions in.
    pub fn with_placement(mut self, placement: impl Into<String>) -> Self {
        self.placement = Some(placement.into());
        self
    }
}

/// Publish `reg` as the registration for `root`, creating the runtime directory.
///
/// The digest is taken now, so a caller that has just bound should publish
/// through [`SocketEndpoint::publish`] or bind with [`bind_socket_registered`],
/// which reuse the digest the bind resolved.
///
/// The file is replaced atomically, so a reader listing the directory never sees
/// half a registration.
pub fn write_registration(root: &Path, reg: &RegistrationFile) -> io::Result<()> {
    let path = runtime_dir()?.join(format!("{}{REGISTRATION_SUFFIX}", workspace_digest(root)));
    write_registration_at(&path, reg)
}

/// The registration for `root`, or `None` when nothing published one.
///
/// Only absence means "nothing is published". A file that exists at this tree's
/// own digest but is not a registration is [`io::ErrorKind::InvalidData`] with
/// the path in the message, because a reader that got this far has to know it
/// could not be read rather than treat a broken endpoint as an absent one.
pub fn read_registration(root: &Path) -> io::Result<Option<RegistrationFile>> {
    read_registration_at(&registration_path(root))
}

/// Remove the registration for `root`. Removing nothing is not an error.
pub fn remove_registration(root: &Path) -> io::Result<()> {
    match std::fs::remove_file(registration_path(root)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Every registration this machine's runtime directory holds, sorted by file
/// path.
///
/// A missing runtime directory is no registrations rather than an error: a
/// reader that starts before any daemon should see an empty machine. Files that
/// are not `.json`, and files that do not parse as a [`RegistrationFile`], are
/// skipped — a publisher writes through a temporary name and holds the socket in
/// a private directory, and one stray file must not blind a reader to every
/// other tree. The socket each entry describes is `socket_path(&reg.root)`, or
/// the entry's own path with its `.json` leaf replaced by `.sock`.
pub fn list_registrations() -> io::Result<Vec<(PathBuf, RegistrationFile)>> {
    let entries = match std::fs::read_dir(runtime_dir_path()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !path
            .to_str()
            .is_some_and(|name| name.ends_with(REGISTRATION_SUFFIX))
        {
            continue;
        }
        if let Ok(Some(reg)) = read_registration_at(&path) {
            found.push((path, reg));
        }
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(found)
}

fn write_registration_at(path: &Path, reg: &RegistrationFile) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(reg)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_private_file(path, &json)
}

fn read_registration_at(path: &Path) -> io::Result<Option<RegistrationFile>> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(reg) => Ok(Some(reg)),
            Err(error) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {error}", path.display()),
            )),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Replace `path` with `bytes`, `0600`, through a temporary name in the same
/// directory so a reader never sees half a registration.
fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("onlyne-registration");
    let temp = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));
    let outcome = open_private(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        drop(file);
        std::fs::rename(&temp, path)
    });
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    outcome
}

/// Open `path` for replacement with mode `0600`.
#[cfg(unix)]
fn open_private(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` applies at creation and passes through umask, and an existing file
    // keeps its own mode, so the handle is chmod'd directly: a replacement is
    // never briefly readable by group or other.
    let mut permissions = file.metadata()?.permissions();
    permissions.set_mode(0o600);
    file.set_permissions(permissions)?;
    Ok(file)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// The owner tree's local socket: the path v2 binds, the canonical spelling v1
/// bound, the registration that names it, and the root all three derive from.
///
/// [`resolve`](Self::resolve) touches no filesystem, so a client that starts
/// before its daemon still resolves the one path the daemon will bind, and an
/// absent runtime directory is not an error until a caller binds or publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketEndpoint {
    root: PathBuf,
    natural: PathBuf,
    actual: PathBuf,
    registration: PathBuf,
}

impl SocketEndpoint {
    /// Resolve from the owner root and its runtime directory
    /// (`<root>/.onlyne/run`).
    ///
    /// The digest covers [`absolute_path`] of `root`, so the spelling has to
    /// stop moving before a daemon and its clients can agree. A tree that exists
    /// resolves its canonical spelling; one that does not yet own a directory
    /// (a caller resolving before `bootstrap` creates it) resolves a collapsed
    /// lexical spelling, and macOS reaches one tree through `/var` and
    /// `/private/var`. Resolve after `bootstrap` when the derived name has to
    /// match a running daemon's.
    ///
    /// `run_dir` names only [`natural`](Self::natural).
    pub fn resolve(root: &Path, run_dir: &Path) -> Self {
        let root = absolute_path(root);
        let dir = runtime_dir_path();
        let digest = workspace_digest(&root);
        Self {
            natural: run_dir.join(SOCKET_FILE_NAME),
            actual: dir.join(format!("{digest}{SOCKET_SUFFIX}")),
            registration: dir.join(format!("{digest}{REGISTRATION_SUFFIX}")),
            root,
        }
    }

    /// The canonical owner root the other three paths belong to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The canonical v1 spelling, `<run_dir>/s`, kept for operators and older
    /// tooling that print it. Nothing binds or connects here under v2.
    pub fn natural(&self) -> &Path {
        &self.natural
    }

    /// The path `bind` and `connect` use:
    /// `<runtime_dir>/<digest>.sock`.
    pub fn actual(&self) -> &Path {
        &self.actual
    }

    /// `<runtime_dir>/<digest>.json`, the file naming [`actual`](Self::actual).
    pub fn registration(&self) -> &Path {
        &self.registration
    }

    /// `true` when the bound path is not the canonical `<run_dir>/s` spelling,
    /// which under v2 is every tree unless a runtime path happens to equal it.
    /// Callers log one line about the move.
    pub fn short(&self) -> bool {
        self.actual != self.natural
    }

    /// Write `reg` at [`registration`](Self::registration).
    ///
    /// The caller owns the file: a role workspace writes its own kind, role, and
    /// runtime here, and the next bind overwrites whatever was there.
    pub fn publish(&self, reg: &RegistrationFile) -> io::Result<()> {
        runtime_dir()?;
        write_registration_at(&self.registration, reg)
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
    collapse_parents(&lexical_absolute(path))
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

/// Create a directory, optionally setting its mode.
///
/// Shared with the layout module of `onlyne-config`, so a caller creates `run/`
/// with the same `0700` a `bootstrap` creates, keeping one convention for the
/// directories that hold private endpoints.
pub fn create_dir(path: &Path, mode: Option<u32>) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    if let Some(mode) = mode {
        set_dir_mode(path, mode)?;
    }
    Ok(())
}

#[cfg(unix)]
pub fn set_dir_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
pub fn set_dir_mode(_path: &Path, _mode: u32) -> io::Result<()> {
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

/// Tokio listener produced by [`bind_local`].
pub type LocalListener = interprocess::local_socket::tokio::Listener;
/// Tokio stream produced by [`connect_local`] or [`LocalListener::accept`](interprocess::local_socket::tokio::prelude::Listener::accept).
pub type LocalStream = interprocess::local_socket::tokio::Stream;
/// Blocking listener for tests that cannot run inside a tokio runtime.
pub type LocalListenerSync = interprocess::local_socket::Listener;
/// Blocking stream matching [`LocalListenerSync`].
pub type LocalStreamSync = interprocess::local_socket::Stream;

/// Traits needed to `.accept()` / `.connect()` the interprocess types.
pub mod prelude {
    pub use interprocess::local_socket::traits::tokio::{
        Listener as TokioListener, Stream as TokioStream,
    };
    pub use interprocess::local_socket::traits::{Listener as SyncListener, Stream as SyncStream};
}

#[cfg(windows)]
const MARKER_PREFIX: &str = "v1:";
const PIPE_BUSY: i32 = 231;
const VERBATIM_PIPE_PREFIX: &str = r"\\.\pipe\";

/// `true` when `--socket` is already an NPFS path and must not be hashed.
pub fn is_verbatim_pipe_path(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|s| starts_with_ignore_ascii_case(s, VERBATIM_PIPE_PREFIX))
}

/// Lexical absolute spelling of `path`.
///
/// `std::path::absolute` (Rust 1.79+) never touches the filesystem, so a missing
/// path still gets an answer.
fn lexical_absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Lowercase hex of the first `bytes` of `sha256` over `path`.
///
/// Separators become `/` and the whole string is lowercased, so `C:\Work` and
/// `c:/work` digest alike.
fn digest_hex(path: &Path, bytes: usize) -> String {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let digest = Sha256::digest(normalized.as_bytes());
    let mut hex = String::with_capacity(bytes * 2);
    for byte in &digest[..bytes] {
        hex.push(HEX[(*byte >> 4) as usize] as char);
        hex.push(HEX[(*byte & 0x0f) as usize] as char);
    }
    hex
}

/// NPFS leaf `onlyne-<32hex>` derived from `path` without touching the filesystem.
///
/// `std::path::absolute` is lexical. Separators become `/` and the whole string
/// is lowercased before the digest so `C:\Work` and `c:/work` agree.
pub fn pipe_name_for(path: &Path) -> String {
    format!("onlyne-{}", digest_hex(&lexical_absolute(path), 16))
}

/// Identity of one owner tree: `sha256` of its canonical spelling, the first 16
/// lowercase hex characters.
///
/// The spelling is [`absolute_path`], so macOS's `/var` and `/private/var`,
/// `a/../b` and `b`, and `C:\Work` and `c:/work` each digest alike — the whole
/// reason a daemon and every client can derive one file name without talking to
/// each other. Both runtime files of a tree share this digest, one leaf apart.
pub fn workspace_digest(root: &Path) -> String {
    digest_hex(&absolute_path(root), 8)
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Bind a tokio listener at `path`.
///
/// Callers still remove a stale `path` first. Unix `mode(0o600)` is set on the
/// bind options (fchmod before bind, no umask TOCTOU). Windows writes the
/// marker, then binds the derived pipe with an owner-only SDDL; a live listener
/// holding that NPFS name makes the second bind fail, which is the EADDRINUSE
/// equivalent (`try_overwrite` is a no-op on Windows).
#[allow(clippy::unused_async)]
pub async fn bind_local(path: &Path) -> io::Result<LocalListener> {
    bind_tokio(path)
}

/// Synchronous counterpart of [`bind_local`] for the tokio listener type.
pub fn bind_tokio(path: &Path) -> io::Result<LocalListener> {
    create_with_privacy(
        path,
        ListenerNonblockingMode::Neither,
        ListenerOptions::create_tokio,
    )
}

/// Bind the owner tree's socket in the runtime directory.
///
/// The runtime directory is created and owner-verified, a stale name at the
/// bound path is dropped, and nothing else is written: the registration naming
/// this socket is the serving side's to publish, because only the serving side
/// knows whether the tree is an admin root or a role workspace
/// ([`write_registration`], [`SocketEndpoint::publish`]).
pub fn bind_socket_v2(root: &Path) -> io::Result<LocalListener> {
    let path = socket_path(root)?;
    bind_runtime_socket(&path)
}

/// Bind the owner tree's local socket at [`socket_path`] in the runtime
/// directory and return it with the endpoint that names it.
///
/// `root` is the owner tree's root and `run_dir` is `<root>/.onlyne/run`, which
/// is created `0700` `bootstrap`-style for a caller that binds before
/// `bootstrap` has, and which survives only as [`SocketEndpoint::natural`].
/// Resolution happens before that creation, so a caller that needs binding and a
/// client to agree on the digest resolves after `bootstrap`, which guarantees a
/// canonical root.
pub fn bind_socket(root: &Path, run_dir: &Path) -> io::Result<(LocalListener, SocketEndpoint)> {
    let endpoint = SocketEndpoint::resolve(root, run_dir);
    runtime_dir()?;
    create_dir(run_dir, Some(0o700))?;
    let listener = bind_runtime_socket(endpoint.actual())?;
    Ok((listener, endpoint))
}

/// Bind the owner tree's socket and publish `reg` for it in one call, for a
/// daemon that knows its surface at start.
///
/// A refused registration fails the call: a socket no reader can find is not the
/// outcome the serving side asked for.
pub fn bind_socket_registered(
    root: &Path,
    run_dir: &Path,
    reg: &RegistrationFile,
) -> io::Result<(LocalListener, SocketEndpoint)> {
    let (listener, endpoint) = bind_socket(root, run_dir)?;
    endpoint.publish(reg)?;
    Ok((listener, endpoint))
}

/// Drop a stale name at `path` and bind it, naming the path and its runtime
/// directory in any failure.
fn bind_runtime_socket(path: &Path) -> io::Result<LocalListener> {
    #[cfg(unix)]
    remove_stale_socket(path);
    bind_tokio(path).map_err(|error| bind_failure(path, error))
}

/// Unix: drop the name so a restarted daemon can take it again.
///
/// `reclaim_name(false)` leaves unlink to the owner, and a name left in place
/// makes the bind below fail with the path and its length in the message.
#[cfg(unix)]
fn remove_stale_socket(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// A bind failure the operator can act on: the path that was tried, its length,
/// and the runtime directory holding it.
fn bind_failure(path: &Path, error: io::Error) -> io::Error {
    let dir = path.parent().unwrap_or(Path::new(""));
    io::Error::new(
        error.kind(),
        format!(
            "bind {} ({} bytes) in {}: {error}",
            path.display(),
            path.as_os_str().len(),
            dir.display(),
        ),
    )
}

/// Adopt a directory this process owns outright.
///
/// Ownership is the check that decides. `/tmp` is world-executable, so a
/// pre-created `/tmp/onlyne-<uid>` owned by another user is refused by uid:
/// a socket inside a directory that user can rewrite is one that user could
/// replace under a live daemon.
///
/// A directory this user owns that is open to group or other is tightened to
/// `0700` rather than refused, because the endpoints inside it are private only
/// while the directory is, and its mode is whatever the first writer chose —
/// an installer, a `tmpfiles.d` rule, or a client that made the directory
/// before its daemon did. Refusing would brick every later daemon over a mode
/// that carries no ownership information, which is why the uid check above and
/// not the mode is the decision.
#[cfg(unix)]
fn ensure_private_dir(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => verify_private_dir(path, &metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // `create_dir_all` because an operator-named runtime directory can
            // nest under paths that do not exist yet, which the default
            // `/tmp/onlyne-<uid>` never does. A process that loses the race to
            // create it gets `Ok` and the chmod below, which fails with EPERM
            // when the winner was another user and no-ops when it was this one.
            std::fs::create_dir_all(path).map_err(|source| dir_failure(path, &source))?;
            set_dir_mode(path, 0o700).map_err(|source| dir_failure(path, &source))?;
        }
        Err(source) => return Err(dir_failure(path, &source)),
    }
    // Mode `0700` is open to exactly one uid, and the file system owner can
    // write anywhere (and macOS ACLs can deny that owner), so the create-new
    // probe is the decision this process actually makes. The name carries a
    // sequence number as well as the pid, so two threads of one process
    // adopting the same directory do not collide on the probe itself.
    static PROBE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = PROBE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let probe = path.join(format!("onlyne-probe-{}-{sequence}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(source) => Err(dir_failure(path, &source)),
    }
}

/// Refuse anything but a directory this user owns and no one else can enter.
#[cfg(unix)]
fn verify_private_dir(path: &Path, metadata: &std::fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if metadata.file_type().is_symlink() {
        return Err(dir_refusal(path, "a symlink"));
    }
    if !metadata.is_dir() {
        return Err(dir_refusal(path, "already held by a non-directory"));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(dir_refusal(path, "owned by another user"));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        set_dir_mode(path, 0o700).map_err(|source| dir_failure(path, &source))?;
    }
    Ok(())
}

/// Off unix the temporary root's ACLs are the guard, so the runtime directory is
/// created if absent and otherwise left alone.
#[cfg(not(unix))]
fn ensure_private_dir(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(path)
}

#[cfg(unix)]
fn dir_refusal(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("refusing to use directory {}: {reason}", path.display()),
    )
}

#[cfg(unix)]
fn dir_failure(path: &Path, source: &io::Error) -> io::Error {
    io::Error::new(
        source.kind(),
        format!("directory {}: {source}", path.display()),
    )
}

/// Blocking bind, used by CLI tests that serve one frame on a helper thread.
pub fn bind_local_sync(path: &Path) -> io::Result<LocalListenerSync> {
    create_with_privacy(
        path,
        ListenerNonblockingMode::Neither,
        ListenerOptions::create_sync,
    )
}

/// Blocking bind whose `accept` returns `WouldBlock` when no client is waiting.
pub fn bind_local_sync_poll(path: &Path) -> io::Result<LocalListenerSync> {
    create_with_privacy(
        path,
        ListenerNonblockingMode::Accept,
        ListenerOptions::create_sync,
    )
}

/// Connect to the listener that [`bind_local`] created for `path`.
///
/// Windows `ERROR_PIPE_BUSY` (231) is remapped to [`io::ErrorKind::WouldBlock`]
/// so a caller can retry inside its existing timeout budget.
pub async fn connect_local(path: &Path) -> io::Result<LocalStream> {
    use interprocess::local_socket::tokio::Stream;
    use interprocess::local_socket::traits::tokio::Stream as _;
    Stream::connect(connect_name(path)?)
        .await
        .map_err(map_pipe_busy)
}

/// Blocking connect matching [`bind_local_sync`].
pub fn connect_local_sync(path: &Path) -> io::Result<LocalStreamSync> {
    use interprocess::local_socket::Stream;
    use interprocess::local_socket::traits::Stream as _;
    Stream::connect(connect_name(path)?).map_err(map_pipe_busy)
}

fn map_pipe_busy(err: io::Error) -> io::Error {
    if err.raw_os_error() == Some(PIPE_BUSY) {
        io::Error::new(io::ErrorKind::WouldBlock, err)
    } else {
        err
    }
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len()
        && value
            .as_bytes()
            .iter()
            .zip(prefix.as_bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

#[cfg(windows)]
fn parse_marker(text: &str) -> Option<String> {
    let rest = text.trim().strip_prefix(MARKER_PREFIX)?;
    if rest.is_empty()
        || !rest
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'/' && b != b'\\')
    {
        return None;
    }
    Some(rest.to_string())
}

#[cfg(windows)]
fn read_marker_name(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_marker(&text)
}

#[cfg(windows)]
fn write_marker(path: &Path, pipe_name: &str) -> io::Result<()> {
    std::fs::write(path, format!("{MARKER_PREFIX}{pipe_name}"))
}

#[cfg(windows)]
fn windows_pipe_leaf(path: &Path) -> String {
    read_marker_name(path).unwrap_or_else(|| pipe_name_for(path))
}

fn create_with_privacy<T>(
    path: &Path,
    nonblocking: ListenerNonblockingMode,
    create: fn(ListenerOptions<'static>) -> io::Result<T>,
) -> io::Result<T> {
    #[cfg(unix)]
    {
        use interprocess::os::unix::local_socket::ListenerOptionsExt;
        match create(unix_options(path, nonblocking)?.mode(0o600)) {
            Ok(listener) => Ok(listener),
            Err(err) if err.kind() == io::ErrorKind::Unsupported => {
                // macOS: fchmod on an unbound socket is Unsupported. chmod
                // after bind is the 1.0.x path and still yields 0600.
                let listener = create(unix_options(path, nonblocking)?)?;
                apply_private_mode(path).map_err(|e| io::Error::other(e.to_string()))?;
                Ok(listener)
            }
            Err(err) => Err(err),
        }
    }
    #[cfg(windows)]
    {
        create(listener_options_windows(path, nonblocking)?)
    }
}

#[cfg(unix)]
fn unix_options(
    path: &Path,
    nonblocking: ListenerNonblockingMode,
) -> io::Result<ListenerOptions<'static>> {
    let name = path.to_fs_name::<GenericFilePath>()?.into_owned();
    Ok(ListenerOptions::new()
        .name(name)
        .reclaim_name(false)
        .nonblocking(nonblocking))
}

#[cfg(windows)]
fn listener_options_windows(
    path: &Path,
    nonblocking: ListenerNonblockingMode,
) -> io::Result<ListenerOptions<'static>> {
    use interprocess::local_socket::{GenericNamespaced, ToNsName};
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use widestring::u16cstr;

    let name = if is_verbatim_pipe_path(path) {
        path.to_fs_name::<GenericFilePath>()?.into_owned()
    } else {
        let leaf = windows_pipe_leaf(path);
        write_marker(path, &leaf)?;
        leaf.to_ns_name::<GenericNamespaced>()?.into_owned()
    };
    let sd = SecurityDescriptor::deserialize(u16cstr!("D:P(A;;GA;;;OW)(A;;GA;;;SY)"))?;
    Ok(ListenerOptions::new()
        .name(name)
        .reclaim_name(false)
        .nonblocking(nonblocking)
        .security_descriptor(sd))
}

fn connect_name(path: &Path) -> io::Result<interprocess::local_socket::Name<'static>> {
    #[cfg(unix)]
    {
        Ok(path.to_fs_name::<GenericFilePath>()?.into_owned())
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        if is_verbatim_pipe_path(path) {
            Ok(path.to_fs_name::<GenericFilePath>()?.into_owned())
        } else {
            Ok(windows_pipe_leaf(path)
                .to_ns_name::<GenericNamespaced>()?
                .into_owned())
        }
    }
}
