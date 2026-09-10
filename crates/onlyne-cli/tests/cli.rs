//! End-to-end contracts of the `onlyne` binary, exercised through real sockets
//! and a real `exec`, so the messages, exit codes and stream split stay pinned.

use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

const EXIT_OK: i32 = 0;
const EXIT_ANSWER_FAILED: i32 = 1;
const EXIT_VALIDATION: i32 = 2;
const EXIT_NO_SOCKET: i32 = 3;
const EXIT_NO_SIBLING: i32 = 127;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_onlyne"))
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// With no `--socket`, `--server-root` or `--workspace`, resolution fails with
/// the byte-exact hint on stderr and exit 3.
#[test]
fn no_socket_prints_the_exact_hint_and_exits_three() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_NO_SOCKET));
    assert_eq!(
        stderr_of(&output),
        "onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace\n"
    );
    assert!(
        output.stdout.is_empty(),
        "a local resolution failure must not print an answer"
    );
}

/// `--from` is rejected on the client surface and required on the admin one;
/// both are local validation failures, so they print a hint and exit 2.
#[test]
fn from_is_rejected_on_client_and_required_on_admin() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("s");

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "--as",
            "client",
            "send",
            "--from",
            "ops",
            "--to",
            "worker",
            "--text",
            "hi",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert_eq!(
        stderr_of(&output),
        "onlyne: --from is only valid on the admin surface\n"
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "--as",
            "admin",
            "send",
            "--to",
            "worker",
            "--text",
            "hi",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert_eq!(stderr_of(&output), "onlyne: --from is required on the admin surface\n");
}

/// An image over the protocol ceiling is refused locally, before any socket is
/// opened, so a missing socket cannot mask the message.
#[test]
fn image_over_the_ceiling_is_refused_before_the_socket_is_opened() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("s");
    let image = dir.path().join("big.png");
    let ceiling = onlyne_proto::IMAGE_DATA_MAX_BYTES;
    std::fs::write(&image, vec![0u8; ceiling + 1]).unwrap();

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "--as",
            "admin",
            "send",
            "--from",
            "ops",
            "--to",
            "worker",
            "--text",
            "hi",
            "--image",
            image.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert_eq!(
        stderr_of(&output),
        format!("onlyne: image exceeds {ceiling} bytes\n")
    );
}

/// `generate` is a forwarder: the spec fragment reaches stdout, the progress
/// lines reach stderr, and the child's exit code is ours.
#[test]
fn generate_forwards_the_argv_to_onlyne_server() {
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let argv_out = dir.path().join("argv.txt");
    let script = "#!/bin/sh\n\
                  printf '%s\n' \"$@\" > \"$ONLYNE_ARGV\"\n\
                  echo 'rendering spec' >&2\n\
                  printf '%s\n' '[[client]] roles: worker'\n";
    let server = bin_dir.join("onlyne-server");
    std::fs::write(&server, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&server).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&server, perms).unwrap();
    }

    let root = dir.path().join("srv");
    let out = dir.path().join("ws");
    let mut path_value = std::ffi::OsString::from(bin_dir.as_os_str());
    path_value.push(":");
    path_value.push(std::env::var_os("PATH").unwrap_or_default());

    let output = Command::new(bin())
        .current_dir(dir.path())
        .env("ONLYNE_ARGV", &argv_out)
        .env("PATH", path_value)
        .args([
            "generate",
            "--server-root",
            root.to_str().unwrap(),
            "--template",
            "spec/roles.yaml",
            "--role",
            "worker",
            "--out",
            out.to_str().unwrap(),
            "--force",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(EXIT_OK));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[[client]]"),
        "the spec fragment must reach stdout, not stderr:\n{stdout}"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("rendering spec"),
        "progress must stay on stderr, not on stdout:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&argv_out).unwrap(),
        format!("generate\n--root\n{}\n--template\nspec/roles.yaml\n--role\nworker\n--out\n{}\n--force\n", root.display(), out.display())
    );
}

/// `wait-ready` never spins past its bound: it prints the elapsed bound and
/// exits 1 when the server stays silent.
#[test]
fn wait_ready_reports_the_bound_when_the_server_never_answers() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("s");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let probe = stop.clone();
    let thread = thread::spawn(move || {
        let _ = listener.set_nonblocking(true);
        loop {
            if probe.load(Ordering::Relaxed) {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buffer = [0u8; 4096];
                    let _ = stream.read(&mut buffer);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "--as",
            "admin",
            "--timeout",
            "300",
            "wait-ready",
            "--interval-ms",
            "50",
        ])
        .output()
        .unwrap();
    stop.store(true, Ordering::Relaxed);
    thread.join().unwrap();

    assert_eq!(output.status.code(), Some(EXIT_ANSWER_FAILED));
    assert_eq!(stderr_of(&output), "onlyne: server not ready after 300ms\n");
    assert!(output.stdout.is_empty(), "a local timeout prints a hint, not json");
}

/// With a `PATH` that carries no daemon sibling, `onlyne server <verb>` reports
/// the missing binary verbatim and exits 127. The binary is copied to a scratch
/// dir first, so the "next to the exe" lookup cannot find one either.
#[test]
fn missing_sibling_binary_reports_the_exact_line_and_exit_127() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("server-root");
    let empty_bin = dir.path().join("bin");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&empty_bin).unwrap();
    let copy = dir.path().join("onlyne");
    std::fs::copy(env!("CARGO_BIN_EXE_onlyne"), &copy).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&copy).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&copy, perms).unwrap();
    }

    let output = Command::new(&copy)
        .current_dir(dir.path())
        .env("PATH", empty_bin.to_str().unwrap())
        .args(["--server-root", root.to_str().unwrap(), "server", "status"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_NO_SIBLING));
    assert_eq!(stderr_of(&output), "onlyne: binary not found: onlyne-server\n");
    assert!(output.stdout.is_empty(), "a missing sibling prints a hint, not an answer");
}
