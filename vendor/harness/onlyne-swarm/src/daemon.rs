use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Child;
use std::time::Duration;

/// A managed `onlyne run` child for one workspace.
pub struct Managed {
    #[allow(dead_code)]
    pub workspace: String,
    pub child: Child,
}

fn onlyne_bin() -> String {
    std::env::var("ONLYNE_BIN").unwrap_or_else(|_| "onlyne".into())
}

fn ping(sock: &Path) -> bool {
    let Ok(s) = UnixStream::connect(sock) else {
        return false;
    };
    let mut s = s;
    if s.write_all(b"{\"id\":\"ping\",\"op\":\"ping\"}\n").is_err() {
        return false;
    }
    s.set_read_timeout(Some(Duration::from_millis(150))).ok();
    let mut r = BufReader::new(&s);
    let mut line = String::new();
    r.read_line(&mut line).is_ok() && line.contains("\"ok\":true")
}

/// Probe a workspace daemon without taking ownership of its lifecycle.
pub fn is_alive(sock: &Path) -> bool {
    ping(sock)
}

/// Ensure a daemon per workspace. Reuses already-running daemons (ping check);
/// spawns `onlyne --workspace <dir> run` otherwise. Returns managed children
/// (only the ones we spawned, for shutdown).
pub fn ensure_all(root: &Path) -> anyhow::Result<Vec<Managed>> {
    let tree = crate::template::load_tree(root)?;
    let mut out = vec![];
    for e in &tree {
        let ws = crate::root::resolve_instance(root, &e.path);
        let sock = crate::root::onlyne_sock(&ws);
        if ping(&sock) {
            continue;
        }
        let child = std::process::Command::new(onlyne_bin())
            .args(["--workspace"])
            .arg(&ws)
            .arg("run")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        out.push(Managed {
            workspace: if e.path.is_empty() {
                ".".into()
            } else {
                e.path.clone()
            },
            child,
        });
    }
    // Wait briefly for sockets.
    for _ in 0..50 {
        let mut ready = true;
        for e in &tree {
            let ws = crate::root::resolve_instance(root, &e.path);
            if !ping(&crate::root::onlyne_sock(&ws)) {
                ready = false;
                break;
            }
        }
        if ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(out)
}

pub async fn stop_all(children: &mut Vec<Managed>) {
    for m in children.iter_mut() {
        let _ = m.child.kill();
        let _ = m.child.wait();
    }
}
