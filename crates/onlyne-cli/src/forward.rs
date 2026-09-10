//! Resolution and execution of the three sibling daemon binaries.

use crate::runtime::EXIT_NO_SIBLING;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        // The canonical line is owned by `onlyne_proto::text`, so the emitter
        // and the assertions in `tests/cli.rs` share one literal.
        eprintln!("{}", onlyne_proto::binary_not_found(bin_name));
        return EXIT_NO_SIBLING;
    };
    let mut command = Command::new(&path);
    command.args(args);
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());
    // `exec` replaces this process on success; any value back means it failed.
    let error = command.exec();
    eprintln!("onlyne: cannot exec {bin_name}: {error}");
    EXIT_NO_SIBLING
}
