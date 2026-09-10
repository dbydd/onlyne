//! Resolution and execution of the three sibling daemon binaries.

use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The daemon binary that serves a forwarding group.
pub fn group_binary(group: &str) -> Option<&'static str> {
    match group {
        "server" => Some("onlyne-server"),
        "client" => Some("onlyne-client"),
        "gateway" => Some("onlyne-gateway"),
        _ => None,
    }
}

/// All three sibling names, in report order.
pub const SIBLINGS: [&str; 3] = ["onlyne-server", "onlyne-client", "onlyne-gateway"];

/// A file that exists and carries an executable bit.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    meta.is_file() && meta.mode() & 0o111 != 0
}

fn candidates_in(dir: &Path, name: &str) -> [PathBuf; 2] {
    [dir.join(name), dir.join(format!("{name}.exe"))]
}

/// Look for `name` next to this binary, then along `PATH`.
pub fn resolve_sibling(name: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in candidates_in(dir, name) {
                if is_executable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    let Ok(var) = std::env::var("PATH") else {
        return None;
    };
    for dir in std::env::split_paths(&var) {
        if dir.as_os_str().is_empty() || !dir.is_dir() {
            continue;
        }
        for candidate in candidates_in(&dir, name) {
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Run the daemon in place of this process, inheriting stdio.
pub fn exec(bin_name: &str, args: &[String]) -> i32 {
    let Some(path) = resolve_sibling(bin_name) else {
        eprintln!("onlyne: binary not found: {bin_name}");
        return 127;
    };
    let mut command = Command::new(&path);
    command.args(args);
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());
    unsafe {
        if let Err(error) = command.exec() {
            eprintln!("onlyne: cannot exec {bin_name}: {error}");
        }
    }
    127
}

/// The directory this binary lives in, for sibling lookup and version reports.
pub fn self_dir() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from))
}
