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

use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions, ToFsName,
};
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;

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

/// NPFS leaf `onlyne-<32hex>` derived from `path` without touching the filesystem.
///
/// `std::path::absolute` is lexical (Rust 1.79+). Separators become `/` and the
/// whole string is lowercased before the digest so `C:\Work` and `c:/work` agree.
pub fn pipe_name_for(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let normalized = absolute
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let digest = Sha256::digest(normalized.as_bytes());
    let mut hex = String::with_capacity(32);
    for byte in &digest[..16] {
        hex.push(HEX[(*byte >> 4) as usize] as char);
        hex.push(HEX[(*byte & 0x0f) as usize] as char);
    }
    format!("onlyne-{hex}")
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
