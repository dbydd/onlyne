use std::path::{Path, PathBuf};

pub const SOCKET_RELATIVE: &str = ".onlyne/run/s";
pub const NO_SOCKET_MESSAGE: &str = onlyne_proto::NO_SOCKET_MESSAGE;

#[derive(Debug, Clone, Default)]
pub struct SocketArgs {
    pub socket: Option<PathBuf>,
    pub server_root: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoSocket;

pub fn resolve_socket(args: &SocketArgs) -> Result<PathBuf, NoSocket> {
    if let Some(path) = &args.socket {
        return Ok(path.clone());
    }
    if let Some(root) = &args.server_root {
        return Ok(root.join(SOCKET_RELATIVE));
    }
    let start = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    discover_upwards(&start).ok_or(NoSocket)
}

fn discover_upwards(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(SOCKET_RELATIVE);
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_flag_wins() {
        let args = SocketArgs {
            socket: Some(PathBuf::from("/tmp/s")),
            server_root: Some(PathBuf::from("/tmp/root")),
            workspace: None,
        };
        assert_eq!(resolve_socket(&args).unwrap(), PathBuf::from("/tmp/s"));
    }

    #[test]
    fn server_root_maps_to_admin_socket() {
        let args = SocketArgs {
            socket: None,
            server_root: Some(PathBuf::from("srv")),
            workspace: None,
        };
        assert_eq!(
            resolve_socket(&args).unwrap(),
            PathBuf::from("srv/.onlyne/run/s")
        );
    }

    #[test]
    fn discovers_workspace_upwards() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("a");
        let nested = root.join("b/c");
        std::fs::create_dir_all(root.join(".onlyne/run")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join(".onlyne/run/s"), b"not-a-socket").unwrap();
        let args = SocketArgs {
            socket: None,
            server_root: None,
            workspace: Some(nested),
        };
        assert_eq!(resolve_socket(&args).unwrap(), root.join(".onlyne/run/s"));
    }
}
