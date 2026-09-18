//! Cross-platform local socket seam.
//!
//! Unix keeps a filesystem UDS at `path` (`GenericFilePath`, mode `0o600`,
//! `reclaim_name(false)` so unlink stays with onlyne). Windows cannot bind a
//! UDS on stable 1.85, so `path` is a regular marker file whose contents name
//! an NPFS pipe (`v1:onlyne-<32hex>`). The pipe leaf is
//! `sha256(absolute-lexical)[..16]` hex; canonicalize is forbidden because a
//! missing path fails it and junctions/drive-letter case would drift.
//!
//! `--socket` values that already start with `\\.\pipe\` skip derivation and
//! travel as `GenericFilePath`. Frame I/O stays generic `AsyncRead`/`AsyncWrite`.
//!
//! [`bind_socket`] adds the owner-tree seam: it resolves a
//! [`SocketEndpoint`], binds the path that fits
//! `sun_path`, and publishes the choice in `run/socket` so every reader reaches
//! one socket through one file.

use crate::SocketEndpoint;
use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions, ToFsName,
};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

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

#[cfg(any(windows, test))]
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
pub(crate) fn lexical_absolute(path: &Path) -> PathBuf {
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

/// `onlyne-<16hex>`-ready identity of one owner tree: the first 8 bytes of
/// `sha256` over the same normalized spelling [`pipe_name_for`] uses.
///
/// The derived socket directory name has to stay short enough that
/// `<temp>/onlyne-<hex>/s` fits [`UNIX_SOCKET_PATH_MAX`](crate::UNIX_SOCKET_PATH_MAX),
/// which is why the leaf is 16 hex characters and the pipe leaf above carries 32.
#[cfg(unix)]
pub(crate) fn short_digest(absolute_root: &Path) -> String {
    digest_hex(absolute_root, 8)
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

/// Bind the owner tree's local socket, choosing the short path when the canonical
/// spelling exceeds [`UNIX_SOCKET_PATH_MAX`](crate::UNIX_SOCKET_PATH_MAX), and
/// publishing the choice in the marker.
///
/// `root` is the owner tree's root; `run_dir` is `<root>/.onlyne/run`. The
/// derived directory under [`std::env::temp_dir`] is created `0700` and adopted
/// only after it proves itself writable by this process alone, because a shared
/// temporary root can already hold a foreign `onlyne-<digest>` directory, and
/// binding a second socket inside it would put two daemons behind one name. A
/// refusal names the directory, which is the answer an operator needs: the fix
/// is to move or clear that directory.
///
/// The marker is written last, so a failed bind leaves the previously published
/// path in place for a client that reads it.
pub fn bind_socket(root: &Path, run_dir: &Path) -> io::Result<(LocalListener, SocketEndpoint)> {
    let endpoint = SocketEndpoint::resolve(root, run_dir);
    crate::create_dir(run_dir, Some(0o700))?;
    #[cfg(unix)]
    if endpoint.short() {
        let fallback = endpoint.actual().to_path_buf();
        let derived = endpoint.actual().parent().unwrap_or(fallback.as_path());
        ensure_private_dir(derived)?;
    }
    #[cfg(unix)]
    remove_stale_socket(endpoint.actual());
    let listener = bind_tokio(endpoint.actual()).map_err(|error| bind_failure(&endpoint, error))?;
    endpoint.publish()?;
    Ok((listener, endpoint))
}

/// Unix: drop the name so a restarted daemon can take it again.
///
/// `reclaim_name(false)` leaves unlink to the owner, and a name left in place
/// makes the bind below fail with both paths and both lengths in the message.
#[cfg(unix)]
fn remove_stale_socket(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// A bind failure the operator can act on: the path that was tried, the
/// canonical spelling it stands for, and each length.
fn bind_failure(endpoint: &SocketEndpoint, error: io::Error) -> io::Error {
    let natural = endpoint.natural();
    let actual = endpoint.actual();
    let message = format!(
        "bind {} ({} bytes) for natural {} ({} bytes): {error}",
        actual.display(),
        actual.as_os_str().len(),
        natural.display(),
        natural.as_os_str().len(),
    );
    io::Error::new(error.kind(), message)
}

/// Adopt a directory this process owns outright.
///
/// An existing directory keeps its mode: chmod'ing a foreign directory to `0700`
/// would make the checks below pass on a path another owner is already using.
#[cfg(unix)]
fn ensure_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path).map_err(|source| dir_failure(path, &source))?;
            crate::set_dir_mode(path, 0o700).map_err(|source| dir_failure(path, &source))?;
        }
        Err(source) => return Err(dir_failure(path, &source)),
    }
    let metadata = std::fs::metadata(path).map_err(|source| dir_failure(path, &source))?;
    if !metadata.is_dir() {
        return Err(dir_refusal(path, "already held by a non-directory"));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(dir_refusal(path, "open to group or other access"));
    }
    // Mode `0700` is open to exactly one uid, and the file system owner can
    // write anywhere, so the create-new probe is the decision this process
    // actually makes.
    let probe = path.join(format!("onlyne-probe-{}", std::process::id()));
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

#[cfg(any(windows, test))]
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

#[cfg(any(windows, test))]
fn read_marker_name(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_marker(&text)
}

#[cfg(any(windows, test))]
fn write_marker(path: &Path, pipe_name: &str) -> io::Result<()> {
    std::fs::write(path, format!("{MARKER_PREFIX}{pipe_name}"))
}

#[cfg(any(windows, test))]
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
                crate::apply_private_mode(path).map_err(|e| io::Error::other(e.to_string()))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_socket_on_a_deep_root_echoes_a_frame() {
        use crate::{RoleWorkspace, ServerRoot, UNIX_SOCKET_PATH_MAX};
        use prelude::*;
        use std::fs;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r".repeat(120));
        let layout = RoleWorkspace::resolve(&root);
        let run_dir = layout.run_dir();
        let natural = layout.socket_path_natural();
        let natural_len = natural.as_os_str().len();
        assert!(
            natural_len > UNIX_SOCKET_PATH_MAX,
            "{} is {natural_len} bytes",
            natural.display()
        );
        // The defect this seam exists for: the canonical spelling is a legal
        // path the kernel refuses, so every client retrying it is stuck. The
        // directory is created first, so the refusal below is the length.
        fs::create_dir_all(&run_dir).unwrap();
        let failure = bind_tokio(&natural).expect_err("the kernel refuses a path past `sun_path`");
        assert_eq!(failure.kind(), io::ErrorKind::InvalidInput, "{failure}");

        let (listener, endpoint) = bind_socket(&root, &run_dir).unwrap();
        let derived_parent = endpoint.actual().parent().unwrap();
        let derived = derived_parent.to_path_buf();
        assert!(endpoint.short());
        assert_eq!(endpoint.natural(), natural.as_path());
        assert_eq!(endpoint.marker(), run_dir.join("socket"));
        let marker_bytes = fs::read_to_string(endpoint.marker()).unwrap();
        assert_eq!(marker_bytes, format!("{}\n", endpoint.actual().display()));
        assert_eq!(mode(endpoint.marker()), 0o600);
        assert_eq!(mode(endpoint.actual()), 0o600);
        assert_eq!(mode(&run_dir), 0o700);
        assert_eq!(mode(&derived), 0o700);
        // Binder and reader reach one path: a freshly resolved layout, standing
        // for another process, agrees with what was just bound, and a server
        // view of the same root agrees too.
        assert_eq!(
            RoleWorkspace::resolve(&root).socket_path(),
            endpoint.actual()
        );
        assert_eq!(ServerRoot::resolve(&root).socket_path(), endpoint.actual());

        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let mut buf = [0u8; 14];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(&buf).await.unwrap();
        });
        let mut client = connect_local(endpoint.actual()).await.unwrap();
        client.write_all(b"onlyne frame!!").await.unwrap();
        let mut buf = [0u8; 14];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"onlyne frame!!");
        server.await.unwrap();

        // A restart takes the same endpoint: the stale name is unlinked, the
        // marker republished, and the derived directory reused.
        let (second, restarted) = bind_socket(&root, &run_dir).unwrap();
        assert_eq!(restarted, endpoint);
        let client = connect_local(restarted.actual()).await.unwrap();
        let accepted = tokio::time::timeout(std::time::Duration::from_secs(5), second.accept())
            .await
            .expect("the restarted listener accepts")
            .unwrap();
        drop((accepted, client));
        fs::remove_dir_all(&derived).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bind_socket_refuses_a_foreign_derived_directory() {
        use crate::RoleWorkspace;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r".repeat(120));
        let layout = RoleWorkspace::resolve(&root);
        let run_dir = layout.run_dir();
        // The tree is created before anything resolves, as `bootstrap` does, so
        // the test and `bind_socket` digest the same canonical root.
        fs::create_dir_all(&run_dir).unwrap();
        let endpoint = layout.socket_endpoint();
        assert!(endpoint.short());
        let derived = endpoint.actual().parent().unwrap().to_path_buf();
        let derived_label = derived.to_string_lossy().into_owned();

        // A name in the way that is a non-directory is refused, by name.
        fs::write(&derived, b"foreign").unwrap();
        let error = bind_socket(&root, &run_dir).unwrap_err();
        assert!(error.to_string().contains(&derived_label), "{error}");
        assert!(!endpoint.marker().exists());
        assert!(!endpoint.actual().exists());
        fs::remove_file(&derived).unwrap();

        // A mode `0500` directory is refused too: the create-new probe is the
        // decision, and a process that ignores directory permissions has no
        // such directory for the probe to fail in.
        if write_probe_is_denied_in_a_private_directory() {
            fs::create_dir_all(&derived).unwrap();
            fs::set_permissions(&derived, fs::Permissions::from_mode(0o500)).unwrap();
            let error = bind_socket(&root, &run_dir).unwrap_err();
            assert!(error.to_string().contains(&derived_label), "{error}");
            fs::set_permissions(&derived, fs::Permissions::from_mode(0o700)).unwrap();
            fs::remove_dir_all(&derived).unwrap();
        }

        // An open mode on an existing directory is refused, and the refusal
        // leaves the foreign mode alone: tightening it in place would be a
        // second owner claiming a directory another owner made.
        fs::create_dir_all(&derived).unwrap();
        fs::set_permissions(&derived, fs::Permissions::from_mode(0o777)).unwrap();
        let error = bind_socket(&root, &run_dir).unwrap_err();
        assert!(error.to_string().contains(&derived_label), "{error}");
        assert_eq!(mode(&derived), 0o777);
        fs::set_permissions(&derived, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(&derived).unwrap();
    }

    /// `true` when this process is denied writes inside a `0500` directory,
    /// which is the condition the create-new probe decides on.
    #[cfg(unix)]
    fn write_probe_is_denied_in_a_private_directory() -> bool {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("private");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let probe = dir.join("probe");
        let denied = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .is_err();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        denied
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn pipe_name_is_stable_across_separators_and_case() {
        let a = pipe_name_for(Path::new(r"C:\Work\App\.onlyne\run\s"));
        let b = pipe_name_for(Path::new("C:/Work/App/.onlyne/run/s"));
        let c = pipe_name_for(Path::new(r"c:\work\app\.onlyne\run\s"));
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert!(a.starts_with("onlyne-"), "{a}");
        assert_eq!(a.len(), "onlyne-".len() + 32, "{a}");
        assert!(
            a[7..]
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{a}"
        );
    }

    #[test]
    fn pipe_name_does_not_follow_canonicalize_rules() {
        // canonicalize would fail: the path does not exist. absolute is lexical.
        let missing = PathBuf::from("/no/such/onlyne-layout-socket-path/s");
        let name = pipe_name_for(&missing);
        assert!(name.starts_with("onlyne-"));
        assert_eq!(name, pipe_name_for(&missing));
    }

    #[test]
    fn verbatim_pipe_paths_are_detected() {
        assert!(is_verbatim_pipe_path(Path::new(
            r"\\.\pipe\onlyne-deadbeef"
        )));
        assert!(is_verbatim_pipe_path(Path::new(r"\\.\PIPE\foo")));
        assert!(!is_verbatim_pipe_path(Path::new("/tmp/.onlyne/run/s")));
        assert!(!is_verbatim_pipe_path(Path::new(r"C:\work\.onlyne\run\s")));
        assert!(!is_verbatim_pipe_path(Path::new(r"\\.\pipe")));
    }

    #[test]
    fn marker_roundtrip_and_corrupt_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s");
        write_marker(&path, "onlyne-abcd0123abcd0123abcd0123abcd0123").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "v1:onlyne-abcd0123abcd0123abcd0123abcd0123"
        );
        assert_eq!(
            read_marker_name(&path).as_deref(),
            Some("onlyne-abcd0123abcd0123abcd0123abcd0123")
        );
        assert!(parse_marker("garbage").is_none());
        assert!(parse_marker("v1:").is_none());
        assert!(parse_marker("v1:has/slash").is_none());
        std::fs::write(&path, "not a marker").unwrap();
        assert!(read_marker_name(&path).is_none());
        assert_eq!(windows_pipe_leaf(&path), pipe_name_for(&path));
    }

    #[tokio::test]
    async fn bind_connect_echoes_bytes() {
        use prelude::*;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s");
        let listener = bind_local(&path).await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(&buf).await.unwrap();
        });
        let mut client = connect_local(&path).await.unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
        server.await.unwrap();
    }
}
