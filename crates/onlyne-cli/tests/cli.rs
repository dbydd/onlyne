//! End-to-end contracts of the `onlyne` binary, exercised through real sockets
//! and a real `exec`, so the messages, exit codes and stream split stay pinned.

use onlyne_proto::{NO_SOCKET_MESSAGE, binary_not_found};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    assert_eq!(stderr_of(&output), format!("{NO_SOCKET_MESSAGE}\n"));
    assert!(
        output.stdout.is_empty(),
        "a local resolution failure must not print an answer"
    );
}

/// A socket path that resolves and then proves absent reads as the same operator
/// problem, so it answers with the canonical hint and exit 3.
#[test]
fn a_resolved_but_absent_socket_prints_the_hint_and_exits_three() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("s");
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "--as",
            "admin",
            "status",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_NO_SOCKET));
    assert_eq!(stderr_of(&output), format!("{NO_SOCKET_MESSAGE}\n"));
    assert!(
        output.stdout.is_empty(),
        "a missing socket must not print an answer body"
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
    assert_eq!(
        stderr_of(&output),
        "onlyne: --from is required on the admin surface\n"
    );
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
    let argv_out = dir.path().join("argv.txt");
    let script = "#!/bin/sh\n\
                  printf '%s\n' \"$@\" > \"$ONLYNE_ARGV\"\n\
                  echo 'rendering spec' >&2\n\
                  printf '%s\n' '[[client]] roles: worker'\n";
    stub_binary(&bin_dir, "onlyne-server", script);
    // The stub shares the directory of the CLI copy, which `resolve_sibling`
    // probes before `PATH`, so the real daemons the suite just built cannot win.
    let cli = onlyne_in(&bin_dir);

    let root = dir.path().join("srv");
    let out = dir.path().join("ws");

    let output = Command::new(&cli)
        .current_dir(dir.path())
        .env("ONLYNE_ARGV", &argv_out)
        .env("PATH", path_with(&bin_dir))
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
        format!(
            "generate\n--root\n{}\n--template\nspec/roles.yaml\n--role\nworker\n--out\n{}\n--force\n",
            root.display(),
            out.display()
        )
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
    assert!(
        output.stdout.is_empty(),
        "a local timeout prints a hint, not json"
    );
}

/// With a `PATH` that carries no daemon sibling, an exec verb reports the
/// canonical line byte for byte, including the `onlyne: ` prefix, and exits 127.
/// The binary is copied to a scratch dir first, so the "next to the exe" lookup
/// cannot find one either.
#[test]
fn missing_sibling_binary_reports_the_exact_line_and_exit_127() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("server-root");
    let empty_bin = dir.path().join("bin");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&empty_bin).unwrap();
    let copy = onlyne_in(dir.path());

    let output = Command::new(&copy)
        .current_dir(dir.path())
        .env("PATH", empty_bin.to_str().unwrap())
        .args(["--server-root", root.to_str().unwrap(), "server", "run"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_NO_SIBLING));
    assert_eq!(
        stderr_of(&output),
        format!("{}\n", binary_not_found("onlyne-server")).as_str(),
        "the whole line, prefix and newline included, is the canonical text"
    );
    assert!(
        output.stdout.is_empty(),
        "a missing sibling prints a hint, not an answer"
    );
}

/// The two status meanings stay distinct, and the admin-socket query wins:
/// `onlyne status`, `onlyne reload`, and `onlyne server status` reach the
/// socket with no `onlyne-server` binary on `PATH` anywhere.
#[test]
fn status_and_reload_never_exec() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let empty_bin = dir.path().join("bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let answer = serde_json::json!({"f": "res", "id": "r1", "ok": true, "data": {"cluster": "c1"}});

    let cases = [
        (vec!["status"], "status"),
        (vec!["reload"], "reload"),
        (vec!["server", "status"], "status"),
    ];
    for (args, op) in cases {
        let listener = admin_listener(&root);
        let server = serve_once(listener, answer.clone());
        let label = args.join(" ");
        let output = Command::new(bin())
            .current_dir(dir.path())
            .env("PATH", empty_bin.as_os_str())
            .arg("--server-root")
            .arg(root.as_os_str())
            .args(&args)
            .output()
            .unwrap();
        let request = server.join().unwrap().unwrap_or_else(|| {
            panic!("`onlyne {label}` must query the socket with no sibling present")
        });

        assert_eq!(
            output.status.code(),
            Some(EXIT_OK),
            "`onlyne {label}` needs no binary"
        );
        assert_eq!(request["f"], "req");
        assert_eq!(
            request["op"], op,
            "`onlyne {label}` round-trips AdminOp::{op}"
        );
        let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            stdout["ok"], true,
            "`onlyne {label}` answers well-formed json"
        );
        assert_eq!(stdout["data"]["cluster"], "c1");
    }
}

/// `status` resolved to a client-surface socket is a local refusal that names
/// the flag the operator must add.
#[test]
fn status_without_a_role_names_the_missing_flag() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("ws");
    // A role workspace with its client socket in place: the path a supervisor
    // agent's shell resolves, where `status` has nothing to answer from.
    let client_socket = workspace.join(".onlyne").join("run").join("s");
    std::fs::create_dir_all(client_socket.parent().unwrap()).unwrap();
    std::fs::write(&client_socket, "").unwrap();

    let output = Command::new(bin())
        .current_dir(&workspace)
        .arg("status")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert_eq!(
        stderr_of(&output),
        "onlyne: status needs the admin surface; pass --server-root <dir>, or --socket <path> with --as admin\n"
    );
    assert!(
        output.stdout.is_empty(),
        "a refusal that reached the socket would print a json answer here"
    );
}

/// The `server` group resolves its admin nouns in this process: `server roles`
/// writes a `roles` frame to the resolved admin socket, and the sibling binary
/// on `PATH` never runs.
#[test]
fn server_roles_queries_the_admin_socket_and_never_execs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let bin_dir = dir.path().join("bin");
    let marker = dir.path().join("executed");
    stub_binary(
        &bin_dir,
        "onlyne-server",
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    let listener = admin_listener(&root);
    let server = serve_once(
        listener,
        serde_json::json!({"f": "res", "id": "r1", "ok": true, "data": {"roles": []}}),
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .env("PATH", path_with(&bin_dir))
        .args(["--server-root", root.to_str().unwrap(), "server", "roles"])
        .output()
        .unwrap();
    let request = server
        .join()
        .unwrap()
        .expect("the CLI must reach the socket");

    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert_eq!(request["f"], "req");
    assert_eq!(request["op"], "roles");
    assert!(
        !marker.exists(),
        "`server roles` resolves in process, so onlyne-server must never run"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "{\"ok\":true,\"data\":{\"roles\":[]}}\n"
    );
}

/// A verb outside the server vocabulary is refused locally, naming the verb,
/// with exit 2 and no answer on stdout.
#[test]
fn unknown_server_verb_is_refused_with_exit_two() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(["server", "frobnicate"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert_eq!(
        stderr_of(&output),
        "onlyne: unknown server verb frobnicate\n"
    );
    assert!(
        output.stdout.is_empty(),
        "a refused verb prints a hint, not an answer"
    );
}

/// `generate` execs the sibling, so the child's exit code becomes the CLI's.
#[test]
fn generate_propagates_the_child_exit_four() {
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    stub_binary(&bin_dir, "onlyne-server", "#!/bin/sh\nexit 4\n");
    let cli = onlyne_in(&bin_dir);
    let root = dir.path().join("srv");

    let output = Command::new(&cli)
        .current_dir(dir.path())
        .env("PATH", path_with(&bin_dir))
        .args(["generate", "--server-root", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
}

/// `--file -` reads the message body from stdin.
#[test]
fn file_dash_reads_the_body_from_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let listener = admin_listener(&root);
    let server = serve_once(
        listener,
        serde_json::json!({"f": "res", "id": "r1", "ok": true, "data": {"state": "in_flight"}}),
    );

    let mut child = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--server-root",
            root.to_str().unwrap(),
            "send",
            "--from",
            "planner",
            "--to",
            "planner",
            "--file",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"body from stdin")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let request = server
        .join()
        .unwrap()
        .expect("the CLI must reach the socket");

    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert_eq!(request["args"]["from"], "planner");
    assert_eq!(
        request["args"]["envelope"]["body"]["text"],
        "body from stdin"
    );
}

/// `--pretty` indents the whole answer; `--quiet` keeps only its payload.
#[test]
fn pretty_and_quiet_shape_the_answer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let answer = || {
        serde_json::json!({
            "f": "res",
            "id": "r1",
            "ok": true,
            "data": {"a": 1, "b": 2},
        })
    };

    let listener = admin_listener(&root);
    let server = serve_once(listener, answer());
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--server-root",
            root.to_str().unwrap(),
            "--pretty",
            "status",
        ])
        .output()
        .unwrap();
    server
        .join()
        .unwrap()
        .expect("the CLI must reach the socket");
    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "{\n  \"ok\": true,\n  \"data\": {\n    \"a\": 1,\n    \"b\": 2\n  }\n}\n"
    );

    let listener = admin_listener(&root);
    let server = serve_once(listener, answer());
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(["--server-root", root.to_str().unwrap(), "--quiet", "status"])
        .output()
        .unwrap();
    server
        .join()
        .unwrap()
        .expect("the CLI must reach the socket");
    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "{\"a\":1,\"b\":2}\n"
    );
}

/// `completions zsh` prints one zsh script on stdout, and nothing on stderr.
#[test]
fn completions_zsh_writes_a_script_to_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(["completions", "zsh"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_OK));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("#compdef onlyne"),
        "the zsh script opens with its compdef line:\n{stdout}"
    );
    assert!(
        stdout.contains("_onlyne"),
        "the script defines the onlyne completion function:\n{stdout}"
    );
    assert!(
        stderr_of(&output).is_empty(),
        "a generated script writes nothing to stderr"
    );
}

/// `--request` replaces the constructed args wholesale: the pinned object
/// passes the protocol validator, reaches the wire verbatim, and the flags that
/// built the discarded one leave no trace.
#[test]
fn request_replaces_the_constructed_args_wholesale() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let listener = admin_listener(&root);
    let server = serve_once(
        listener,
        serde_json::json!({"f": "res", "id": "r1", "ok": true, "data": {}}),
    );

    let given = serde_json::to_value(pinned_send(Some(VALID_OP_ID))).unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--server-root",
            root.to_str().unwrap(),
            "--request",
            &given.to_string(),
            "send",
            "--from",
            "planner",
            "--to",
            "planner",
            "--text",
            "discarded",
        ])
        .output()
        .unwrap();
    let request = server
        .join()
        .unwrap()
        .expect("the CLI must reach the socket");

    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert_eq!(request["op"], "send");
    assert_eq!(
        request["args"], given,
        "the pinned object reaches the wire as given"
    );
    assert_eq!(request["args"]["envelope"]["body"]["text"], "pinned body");
}

/// `--request` is validated before the socket opens: a pinned `Task` with no
/// `op_id` is refused locally, and the protocol error names the field. The
/// server root below holds no socket, so a remote round trip cannot be what
/// answers.
#[test]
fn request_without_op_id_is_refused_locally() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let given = serde_json::to_value(pinned_send(None)).unwrap();
    assert!(!given["envelope"].as_object().unwrap().contains_key("op_id"));

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--server-root",
            root.to_str().unwrap(),
            "--request",
            &given.to_string(),
            "send",
            "--from",
            "planner",
            "--to",
            "planner",
            "--text",
            "pinned body",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert_eq!(
        stderr_of(&output),
        "onlyne: op_id: op_id is required for kind task\n"
    );
    assert!(
        output.stdout.is_empty(),
        "a local refusal prints a hint, not an answer"
    );
}

/// `control` carries no envelope, so `--request` is refused outright instead of
/// being silently ignored. No socket is reachable here, which pins the refusal
/// ahead of socket resolution.
#[test]
fn request_is_refused_on_control() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--request",
            "{}",
            "control",
            "--task",
            "22222222-2222-4222-8222-222222222222",
            "--reason",
            "rotate",
            "recycle",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert_eq!(
        stderr_of(&output),
        "onlyne: --request is not supported by control\n"
    );
    assert!(
        output.stdout.is_empty(),
        "a refused flag prints a hint, not an answer"
    );
}

/// A pinned `op_id` in the protocol's own spelling, `o-` plus uuid v4.
const VALID_OP_ID: &str = "o-33333333-3333-4333-8333-333333333333";

/// An admin `send` with pinned envelope identity, the shape `--request` takes.
fn pinned_send(op_id: Option<&str>) -> onlyne_proto::AdminSend {
    onlyne_proto::AdminSend {
        from: "planner".to_string(),
        envelope: Box::new(onlyne_proto::Envelope {
            protocol: onlyne_proto::PROTOCOL_VERSION,
            id: "11111111-1111-4111-8111-111111111111".to_string(),
            op_id: op_id.map(str::to_string),
            kind: onlyne_proto::MsgKind::Task,
            from: onlyne_proto::Principal::role("planner"),
            to: onlyne_proto::Principal::role("planner"),
            control: None,
            causality: Some(onlyne_proto::Causality::root(
                "22222222-2222-4222-8222-222222222222",
            )),
            body: onlyne_proto::Body::text("pinned body"),
            ts: "2026-09-10T00:00:00Z".parse().unwrap(),
            ttl_ms: None,
            admin: false,
        }),
    }
}

/// The admin socket path `--server-root` resolves, bound and ready to answer.
fn admin_listener(root: &Path) -> UnixListener {
    let socket = root.join(".onlyne").join("run").join("s");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&socket);
    UnixListener::bind(&socket).unwrap()
}

/// Answer one request frame on `listener` and hand back the frame it carried.
///
/// The accept loop gives up after ten seconds, so a CLI that never connects
/// fails an assertion instead of hanging the suite.
fn serve_once(
    listener: UnixListener,
    answer: serde_json::Value,
) -> thread::JoinHandle<Option<serde_json::Value>> {
    thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // The listener polls, and macOS hands its non-blocking flag
                    // to the accepted stream, so the frame read clears it.
                    stream.set_nonblocking(false).unwrap();
                    let mut stream = stream;
                    let request = read_frame(&mut stream);
                    let mut reply = answer.clone();
                    reply["id"] = request["id"].clone();
                    write_frame(&mut stream, &reply);
                    return Some(request);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() > deadline {
                        return None;
                    }
                    thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => return None,
            }
        }
    })
}

/// Read one length-prefixed JSON frame from a blocking stream.
fn read_frame(stream: &mut std::os::unix::net::UnixStream) -> serde_json::Value {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).unwrap();
    let mut payload = vec![0u8; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut payload).unwrap();
    serde_json::from_slice(&payload).unwrap()
}

/// Write one length-prefixed JSON frame to a blocking stream.
fn write_frame(stream: &mut std::os::unix::net::UnixStream, value: &serde_json::Value) {
    let payload = serde_json::to_vec(value).unwrap();
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&payload).unwrap();
    stream.flush().unwrap();
}

/// Copy the built CLI into `dir`, so `resolve_sibling`'s exe-adjacent probe
/// reads that directory and the real daemons in the cargo target directory
/// cannot shadow the stubs beside it.
fn onlyne_in(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let copy = dir.join("onlyne");
    std::fs::copy(env!("CARGO_BIN_EXE_onlyne"), &copy).unwrap();
    let mut perms = std::fs::metadata(&copy).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&copy, perms).unwrap();
    copy
}

/// Write an executable stub binary named `name` into `dir`.
fn stub_binary(dir: &Path, name: &str, script: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// `PATH` with `dir` in front, so a stub shadows any real sibling.
fn path_with(dir: &Path) -> OsString {
    let mut value = OsString::from(dir.as_os_str());
    value.push(":");
    value.push(std::env::var_os("PATH").unwrap_or_default());
    value
}

/// The lifecycle verbs exec `onlyne-server` with their flags intact, so the
/// daemon sees the same argv a direct invocation carries.
#[test]
fn server_lifecycle_verbs_forward_the_argv_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    let argv_out = dir.path().join("argv.txt");
    stub_binary(
        &bin_dir,
        "onlyne-server",
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ONLYNE_ARGV\"\n",
    );
    let cli = onlyne_in(&bin_dir);
    let root = dir.path().join("srv");

    let output = Command::new(&cli)
        .current_dir(dir.path())
        .env("ONLYNE_ARGV", &argv_out)
        .env("PATH", path_with(&bin_dir))
        .args([
            "server",
            "init",
            "--root",
            root.to_str().unwrap(),
            "--listen",
            "127.0.0.1:7899",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert_eq!(
        std::fs::read_to_string(&argv_out).unwrap(),
        format!(
            "init\n--root\n{}\n--listen\n127.0.0.1:7899\n",
            root.display()
        )
    );
}

/// `gateway status` reads the admin status op, so the registered gateway ids
/// and their capabilities reach stdout.
#[test]
fn gateway_status_queries_the_admin_socket() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let listener = admin_listener(&root);
    let server = serve_once(
        listener,
        serde_json::json!({
            "f": "res",
            "id": "r1",
            "ok": true,
            "data": {
                "gateways": [{"id": "fake", "capabilities": ["register", "deliver"]}],
                "connected_gateways": 1,
            },
        }),
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(["--server-root", root.to_str().unwrap(), "gateway", "status"])
        .output()
        .unwrap();
    let request = server
        .join()
        .unwrap()
        .expect("the CLI must reach the socket");

    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert_eq!(request["op"], "status");
    let answer: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let gateway = &answer["data"]["gateways"][0];
    assert_eq!(gateway["id"], "fake", "the gateway id must reach stdout");
    assert_eq!(
        gateway["capabilities"],
        serde_json::json!(["register", "deliver"]),
        "the capabilities must reach stdout"
    );
}

/// `gateway run` execs `onlyne-gateway`, and the platform plus its flags reach
/// the sibling verbatim.
#[test]
fn gateway_run_forwards_to_onlyne_gateway() {
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = dir.path().join("bin");
    let argv_out = dir.path().join("argv.txt");
    stub_binary(
        &bin_dir,
        "onlyne-gateway",
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ONLYNE_ARGV\"\n",
    );
    let cli = onlyne_in(&bin_dir);
    let root = dir.path().join("srv");

    let output = Command::new(&cli)
        .current_dir(dir.path())
        .env("ONLYNE_ARGV", &argv_out)
        .env("PATH", path_with(&bin_dir))
        .args([
            "gateway",
            "run",
            "telegram",
            "--server-root",
            root.to_str().unwrap(),
            "--token",
            "t0ken",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert_eq!(
        std::fs::read_to_string(&argv_out).unwrap(),
        format!(
            "run\ntelegram\n--server-root\n{}\n--token\nt0ken\n",
            root.display()
        )
    );
}
