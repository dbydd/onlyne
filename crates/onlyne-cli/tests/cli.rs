//! End-to-end contracts of the `onlyne` binary, exercised through real sockets
//! and a real `exec`, so the messages, exit codes and stream split stay pinned.

use onlyne_layout::local_socket::prelude::SyncListener;
use onlyne_layout::{LocalListenerSync, LocalStreamSync, bind_local_sync_poll};
use onlyne_proto::{NO_SOCKET_MESSAGE, binary_not_found};
use std::ffi::OsString;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const EXIT_OK: i32 = 0;
const EXIT_ANSWER_FAILED: i32 = 1;
const EXIT_VALIDATION: i32 = 2;
const EXIT_NO_SOCKET: i32 = 3;
const EXIT_REFUSAL: i32 = 4;
const EXIT_NO_SIBLING: i32 = 127;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_onlyne"))
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `fs::copy` can leave the destination busy on Linux (ETXTBSY / code 26).
/// Retry a bounded number of times so a just-written binary can exec.
fn spawn_output(cmd: &mut Command) -> Output {
    const ATTEMPTS: u32 = 10;
    let mut last = None;
    for _ in 0..ATTEMPTS {
        match cmd.output() {
            Ok(out) => return out,
            Err(err) if err.kind() == ErrorKind::ExecutableFileBusy => {
                last = Some(err);
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => panic!("failed to spawn CLI: {err}"),
        }
    }
    panic!(
        "failed to spawn CLI after {ATTEMPTS} ETXTBSY retries: {}",
        last.expect("ExecutableFileBusy")
    );
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
            "--force",
            SUPERVISOR_FLAG,
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
            "--force",
            SUPERVISOR_FLAG,
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
            "--force",
            SUPERVISOR_FLAG,
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

    let output = spawn_output(
        Command::new(&cli)
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
            ]),
    );

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
        read_stub_argv(&argv_out),
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
    let listener = bind_local_sync_poll(&socket).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let probe = stop.clone();
    let thread = thread::spawn(move || {
        loop {
            if probe.load(Ordering::Relaxed) {
                break;
            }
            match listener.accept() {
                Ok(mut stream) => {
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

/// With no socket reachable by any resolution rule, `wait-ready` answers with
/// the canonical hint on stderr and exit 3.
#[test]
fn wait_ready_exits_three_when_no_socket_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .env_remove("ONLYNE_SOCKET")
        .arg("wait-ready")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_NO_SOCKET));
    assert_eq!(stderr_of(&output), format!("{NO_SOCKET_MESSAGE}\n"));
    assert!(
        output.stdout.is_empty(),
        "a local resolution failure must not print an answer"
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

    let output = spawn_output(
        Command::new(&copy)
            .current_dir(dir.path())
            .env("PATH", empty_bin.to_str().unwrap())
            .args(["--server-root", root.to_str().unwrap(), "server", "run"]),
    );
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
/// `onlyne status`, `onlyne reload`, `onlyne server status`, and
/// `onlyne server reload` reach the socket with no `onlyne-server` binary on
/// `PATH` anywhere.
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
        (vec!["server", "reload"], "reload"),
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

    let output = spawn_output(
        Command::new(&cli)
            .current_dir(dir.path())
            .env("PATH", path_with(&bin_dir))
            .args(["generate", "--server-root", root.to_str().unwrap()]),
    );
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
            "--force",
            SUPERVISOR_FLAG,
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

/// `--head-from` defaults to `local`, so a `complete` that omits it truncates
/// `--text` into the head. Both frames reach the role socket, and the head filed
/// with the report is the completion text.
#[test]
fn complete_without_head_from_files_a_local_head() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("role");
    let listener = role_listener(&workspace);
    let ok = || serde_json::json!({"f": "res", "id": "r1", "ok": true, "data": {}});
    let server = serve_sequence(listener, vec![ok(), ok()]);

    let output = Command::new(bin())
        .current_dir(dir.path())
        .env("ONLYNE_ROLE", "planner")
        .args([
            "--workspace",
            workspace.to_str().unwrap(),
            "complete",
            "--force",
            SUPERVISOR_FLAG,
            "--task",
            TEST_TASK,
            "--text",
            "default head source",
            "--outcome",
            "done",
        ])
        .output()
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "an omitted `--head-from` takes its default: {}",
        stderr_of(&output)
    );
    assert_eq!(
        requests.len(),
        2,
        "the completion send and its report both reach the socket"
    );
    let send = &requests[0];
    assert_eq!(send["op"], "send");
    assert_eq!(send["args"]["kind"], "completion");
    assert_eq!(send["args"]["body"]["text"], "default head source");
    let report = &requests[1];
    assert_eq!(report["op"], "report");
    assert_eq!(report["args"]["kind"], "complete");
    assert_eq!(report["args"]["data"]["task_id"], TEST_TASK);
    assert_eq!(report["args"]["data"]["outcome"], "done");
    assert_eq!(report["args"]["data"]["head"], "default head source");
}

/// `--head-from ledger` takes the head from the row, so `--text` is optional
/// there. The run clears clap and the local head rule, and the ledger query frame
/// is on the wire; the empty completion body then meets the protocol's own rule.
#[test]
fn complete_with_ledger_head_omits_text_without_a_flag_error() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("role");
    let listener = role_listener(&workspace);
    let server = serve_sequence(
        listener,
        vec![serde_json::json!({
            "f": "res",
            "id": "r1",
            "ok": true,
            "data": {"rows": [{"msg_id": ACK_MSG_ID, "out_head": "head filed by the row"}]}
        })],
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .env("ONLYNE_ROLE", "planner")
        .args([
            "--workspace",
            workspace.to_str().unwrap(),
            "complete",
            "--force",
            SUPERVISOR_FLAG,
            "--task",
            TEST_TASK,
            "--head-from",
            "ledger",
            "--outcome",
            "done",
        ])
        .output()
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(
        requests.len(),
        1,
        "a ledger head is read before the payload is built"
    );
    assert_eq!(requests[0]["op"], "query_ledger");
    assert_eq!(requests[0]["args"]["task"], TEST_TASK);
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("body requires text or image"),
        "the protocol's own body rule ends the run: {stderr}"
    );
    assert!(
        !stderr.contains("--text"),
        "`--text` is optional with a ledger head: {stderr}"
    );
}

/// A local head comes from `--text`, so a `complete` naming neither flag is a
/// local validation failure decided before the socket: exit 2, the flag named on
/// stderr, stdout empty.
#[test]
fn complete_local_head_without_text_names_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("s");
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--socket",
            socket.to_str().unwrap(),
            "--as",
            "admin",
            "complete",
            "--force",
            SUPERVISOR_FLAG,
            "--task",
            TEST_TASK,
            "--outcome",
            "done",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert_eq!(
        stderr_of(&output),
        "onlyne: --text is required with --head-from local\n"
    );
    assert!(
        output.stdout.is_empty(),
        "a local refusal must not print an answer body"
    );
}

/// The long half of the supervisor gate, spelled as the operator types it.
const SUPERVISOR_FLAG: &str = "--yes-i-am-supervisor-not-other-role";

/// Every verb a role speaks or acts through, and so every verb behind the gate.
const GATED_VERBS: [&str; 7] = [
    "send", "reply", "complete", "handoff", "ack", "reject", "control",
];

/// The line a gated verb prints when a call arrives without both flags: both
/// flag names, the path a role reads instead, and why the verb exists.
fn supervisor_refusal(verb: &str) -> String {
    let tool = match verb {
        "send" => {
            "sends with its plugin's own tool, onlyne_send (to, text, kind, image), where \
             kind=\"task\" starts a new task family at hop 0, kind=\"note\" leaves free text, and \
             onlyne_handoff continues the family this session was handed"
        }
        "handoff" => {
            "hands work on with its plugin's own tool, onlyne_handoff (task_id, to, text, image), \
             which names this task as the child's parent_task and carries the family's hop \
             budget, origin, deadline, and labels"
        }
        "complete" => {
            "reports its ending with its plugin's own tool, onlyne_complete (outcome, text, \
             force, reason)"
        }
        "reply" => {
            "replies through its plugin, which answers for its session and offers no reply tool \
             that a role would reach for"
        }
        "ack" => {
            "settles a delivered envelope through its plugin, which answers for its session and \
             offers no ack tool that a role would reach for"
        }
        "reject" => {
            "refuses a delivered envelope through its plugin, which answers for its session and \
             offers no reject tool that a role would reach for"
        }
        "control" => {
            "runs a control op through its plugin, which answers for its session and offers no \
             control tool that a role would reach for"
        }
        other => panic!("{other} is not a gated verb"),
    };
    format!(
        "onlyne: {verb} requires --force and {SUPERVISOR_FLAG}: a role inside a session {tool}; \
         this verb is a supervisor maintenance command for an operator or a supervisor driving a \
         role from outside\n"
    )
}

/// The argv one gated verb needs to be otherwise valid, so a flagless call is
/// decided by the gate alone. The socket path is one nothing serves: a call that
/// reached socket work answers the canonical hint and exits 3.
fn gated_argv<'a>(verb: &'a str, socket: &'a str) -> Vec<&'a str> {
    let mut args = vec!["--socket", socket, "--as", "admin", verb];
    let tail: &[&str] = match verb {
        "send" => &["--from", "ops", "--to", "worker", "--text", "gate check"],
        "reply" => &["--from", "ops", "--to", ACK_MSG_ID, "--text", "gate check"],
        "complete" => &[
            "--from",
            "ops",
            "--task",
            TEST_TASK,
            "--text",
            "gate check",
            "--outcome",
            "done",
        ],
        "handoff" => &[
            "--from",
            "ops",
            "--task",
            TEST_TASK,
            "--to",
            "worker",
            "--text",
            "gate check",
        ],
        // `ack` and `reject` carry `AckArgs` alone: they take no sender.
        "ack" | "reject" => &["--msg-id", ACK_MSG_ID, "--reason", "gate check"],
        "control" => &["--from", "ops", "--task", CONTROL_TASK, "probe"],
        other => panic!("{other} is not a gated verb"),
    };
    args.extend_from_slice(tail);
    args
}

/// Every gated verb refuses a call missing either flag, and the refusal is
/// local: exit 2, both flag names on stderr, and nothing on stdout. The socket
/// path is one nothing serves, so a call that reached it would answer the
/// canonical no-socket hint and exit 3 — the message here is the gate's.
#[test]
fn every_gated_verb_refuses_without_both_supervisor_flags() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("s");
    let socket = socket.to_str().unwrap();

    let partial: [&[&str]; 3] = [&[], &["--force"], &[SUPERVISOR_FLAG]];
    for verb in GATED_VERBS {
        for flags in partial {
            let output = Command::new(bin())
                .current_dir(dir.path())
                .args(gated_argv(verb, socket))
                .args(flags)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(EXIT_VALIDATION),
                "an otherwise valid `{verb}` missing {flags:?} is refused locally: {}",
                stderr_of(&output)
            );
            assert_eq!(stderr_of(&output), supervisor_refusal(verb));
            assert!(
                output.stdout.is_empty(),
                "a local refusal must not print an answer body"
            );
        }
    }
}

/// Both flags together open the gate, so each verb runs on to the check it
/// already had. Every one of them reaches socket resolution and meets the absent
/// socket; `ack` and `reject` stop one step earlier, at their own surface rule.
#[test]
fn both_supervisor_flags_open_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("s");
    let socket = socket.to_str().unwrap();

    for verb in GATED_VERBS {
        let output = Command::new(bin())
            .current_dir(dir.path())
            .args(gated_argv(verb, socket))
            .args(["--force", SUPERVISOR_FLAG])
            .output()
            .unwrap();
        let stderr = stderr_of(&output);
        assert_ne!(
            stderr,
            supervisor_refusal(verb),
            "`{verb}` must pass the gate"
        );
        let (code, expected) = match verb {
            "ack" | "reject" => (
                EXIT_VALIDATION,
                format!("onlyne: {verb} requires a role workspace or client socket\n"),
            ),
            _ => (EXIT_NO_SOCKET, format!("{NO_SOCKET_MESSAGE}\n")),
        };
        assert_eq!(output.status.code(), Some(code), "`{verb}`: {stderr}");
        assert_eq!(stderr, expected, "`{verb}` reached its own next check");
    }

    // `complete`'s head rule is one step past the gate, so a call that opens the
    // gate with no `--text` is answered by that rule.
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--socket",
            socket,
            "--as",
            "admin",
            "complete",
            "--force",
            SUPERVISOR_FLAG,
            "--from",
            "ops",
            "--task",
            TEST_TASK,
            "--outcome",
            "done",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert_eq!(
        stderr_of(&output),
        "onlyne: --text is required with --head-from local\n",
        "`complete` reaches its own head rule once the gate is open"
    );
}

/// A task id the handoff tests hand inside a family: the envelope validator
/// requires every task id to be a uuid.
const FAMILY_TASK: &str = "55555555-5555-4555-8555-555555555555";

/// A handoff child continues the family its parent row carries: the child
/// envelope names the row's task as `parent_task`, sits one hop below it, and
/// carries the row's `family`, `hop_budget`, `origin`, `deadline`, and `labels`.
#[test]
fn handoff_continues_the_family_the_parent_row_carries() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("role");
    let listener = role_listener(&workspace);
    let server = serve_sequence(
        listener,
        vec![
            serde_json::json!({
                "f": "res",
                "id": "r1",
                "ok": true,
                "data": {"rows": [{
                    "msg_id": ACK_MSG_ID,
                    "task": TEST_TASK,
                    "state": "acked",
                    "hop": 3,
                    "family": FAMILY_TASK,
                    "hop_budget": 11,
                    "origin": "planner",
                    "deadline": "2030-01-01T00:00:00Z",
                    "labels": {"run": "lights"}
                }]}
            }),
            serde_json::json!({"f": "res", "id": "r2", "ok": true, "data": {}}),
        ],
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .env("ONLYNE_ROLE", "planner")
        .args([
            "--workspace",
            workspace.to_str().unwrap(),
            "handoff",
            "--force",
            SUPERVISOR_FLAG,
            "--task",
            TEST_TASK,
            "--to",
            "worker",
            "--text",
            "carry the token",
        ])
        .output()
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "the handoff must land: {}",
        stderr_of(&output)
    );
    assert_eq!(
        requests.len(),
        2,
        "the parent row is read back before the child is sent"
    );
    assert_eq!(requests[0]["op"], "query_ledger");
    assert_eq!(requests[1]["op"], "send");
    let causality = &requests[1]["args"]["causality"];
    assert_eq!(causality["parent_task"], TEST_TASK);
    assert_eq!(causality["hop"], 4, "the child sits one hop below the row");
    assert_eq!(causality["family"], FAMILY_TASK);
    assert_eq!(causality["hop_budget"], 11);
    assert_eq!(causality["origin"], "planner");
    assert_eq!(causality["deadline"], "2030-01-01T00:00:00Z");
    assert_eq!(causality["labels"]["run"], "lights");
}

/// A row written before the family columns existed names no family, so the child
/// carries the task it hangs under as the family it continues, and no more.
#[test]
fn handoff_roots_the_family_when_the_row_names_none() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("role");
    let listener = role_listener(&workspace);
    let server = serve_sequence(
        listener,
        vec![
            serde_json::json!({
                "f": "res",
                "id": "r1",
                "ok": true,
                "data": {"rows": [{
                    "msg_id": ACK_MSG_ID,
                    "task": TEST_TASK,
                    "state": "acked",
                    "hop": 0
                }]}
            }),
            serde_json::json!({"f": "res", "id": "r2", "ok": true, "data": {}}),
        ],
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .env("ONLYNE_ROLE", "planner")
        .args([
            "--workspace",
            workspace.to_str().unwrap(),
            "handoff",
            "--force",
            SUPERVISOR_FLAG,
            "--task",
            TEST_TASK,
            "--to",
            "worker",
            "--text",
            "carry the token",
        ])
        .output()
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "the handoff must land: {}",
        stderr_of(&output)
    );
    let causality = &requests[1]["args"]["causality"];
    assert_eq!(causality["family"], TEST_TASK);
    assert_eq!(causality["hop"], 1);
    for absent in ["hop_budget", "origin", "deadline", "labels"] {
        assert!(
            causality.get(absent).is_none(),
            "a row that names no {absent} hands none down: {causality}"
        );
    }
}

/// The three family flags reach the envelope a `send` mints, beside the origin
/// role the send speaks as and the family the minted task roots.
#[test]
fn send_starts_a_family_with_the_figures_it_was_given() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let listener = admin_listener(&root);
    let server = serve_once(
        listener,
        serde_json::json!({"f": "res", "id": "r1", "ok": true, "data": {}}),
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--server-root",
            root.to_str().unwrap(),
            "send",
            "--force",
            SUPERVISOR_FLAG,
            "--from",
            "planner",
            "--to",
            "worker",
            "--text",
            "a bounded run",
            "--hop-budget",
            "7",
            "--label",
            "run=lights",
            "--label",
            "lap=2",
            "--deadline",
            "2030-01-01T00:00:00Z",
        ])
        .output()
        .unwrap();
    let request = server
        .join()
        .unwrap()
        .expect("the CLI must reach the socket");

    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "the send must land: {}",
        stderr_of(&output)
    );
    let causality = &request["args"]["envelope"]["causality"];
    assert_eq!(causality["origin"], "planner");
    assert_eq!(causality["hop_budget"], 7);
    assert_eq!(causality["deadline"], "2030-01-01T00:00:00Z");
    assert_eq!(causality["labels"]["run"], "lights");
    assert_eq!(causality["labels"]["lap"], "2");
    assert_eq!(
        causality["family"], causality["task"],
        "a fresh send roots the family it starts"
    );
    assert_eq!(causality["hop"], 0);
}

/// A send on a role workspace speaks as the role that owns it: `--from` is the
/// admin surface's flag, so the origin a family reports home to is the local
/// role here, which is the shape a ring's own hop reads.
#[test]
fn send_roots_the_family_with_the_local_role_as_its_origin() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("role");
    let listener = role_listener(&workspace);
    let server = serve_once(
        listener,
        serde_json::json!({"f": "res", "id": "r1", "ok": true, "data": {}}),
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .env("ONLYNE_ROLE", "light6")
        .args([
            "--workspace",
            workspace.to_str().unwrap(),
            "send",
            "--force",
            SUPERVISOR_FLAG,
            "--to",
            "light1",
            "--text",
            "running-lights token",
        ])
        .output()
        .unwrap();
    let request = server
        .join()
        .unwrap()
        .expect("the CLI must reach the socket");

    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "the send must land: {}",
        stderr_of(&output)
    );
    let causality = &request["args"]["causality"];
    assert_eq!(causality["origin"], "light6");
    assert_eq!(causality["family"], causality["task"]);
    assert_eq!(causality["hop"], 0);
}

/// The family flags are refused locally when they cannot be carried: past the
/// protocol's entry ceiling, without a `key=value`, or as a stamp that is not an
/// RFC 3339 instant. Each one exits 2, names its flag, and opens no socket.
#[test]
fn send_refuses_the_family_flags_it_cannot_carry() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("s");
    let base: Vec<String> = [
        "--socket",
        socket.to_str().unwrap(),
        "--as",
        "admin",
        "send",
        "--force",
        SUPERVISOR_FLAG,
        "--from",
        "ops",
        "--to",
        "worker",
        "--text",
        "a bounded run",
    ]
    .iter()
    .map(|value| value.to_string())
    .collect();

    let ceiling = onlyne_proto::envelope::CAUSALITY_LABEL_MAX_ENTRIES;
    let mut over = base.clone();
    for index in 1..=ceiling + 1 {
        over.push("--label".to_string());
        over.push(format!("k{index}=v"));
    }
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(&over)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert_eq!(
        stderr_of(&output),
        format!(
            "onlyne: --label carries at most {ceiling} entries, got {}\n",
            ceiling + 1
        )
    );
    assert!(
        output.stdout.is_empty(),
        "a local refusal must not print an answer body"
    );

    for (flag, value) in [("--label", "no-equals"), ("--deadline", "next tuesday")] {
        let mut args = base.clone();
        args.push(flag.to_string());
        args.push(value.to_string());
        let output = Command::new(bin())
            .current_dir(dir.path())
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(EXIT_VALIDATION), "{flag}");
        let stderr = stderr_of(&output);
        assert!(stderr.contains(flag), "the refusal names {flag}: {stderr}");
        assert!(
            output.stdout.is_empty(),
            "a refused flag prints a hint, not an answer"
        );
    }
}

/// `onlyne ledger` prints the family keys a row carries, and a row written
/// before those columns existed renders without them.
#[test]
fn ledger_prints_the_family_keys_and_omits_absent_ones() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let listener = admin_listener(&root);
    let server = serve_once(
        listener,
        serde_json::json!({
            "f": "res",
            "id": "r1",
            "ok": true,
            "data": {"rows": [
                {
                    "msg_id": ACK_MSG_ID,
                    "task": TEST_TASK,
                    "state": "acked",
                    "hop": 3,
                    "family": FAMILY_TASK,
                    "hop_budget": 11
                },
                {"msg_id": ACK_MSG_ID, "task": TEST_TASK, "state": "acked", "hop": 3}
            ]}
        }),
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--server-root",
            root.to_str().unwrap(),
            "ledger",
            "--task",
            TEST_TASK,
        ])
        .output()
        .unwrap();
    server
        .join()
        .unwrap()
        .expect("the CLI must reach the socket");

    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "the ledger read must land: {}",
        stderr_of(&output)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = &body["data"]["rows"];
    assert_eq!(rows[0]["family"], FAMILY_TASK);
    assert_eq!(rows[0]["hop_budget"], 11);
    for absent in ["family", "hop_budget"] {
        assert!(
            rows[1].get(absent).is_none(),
            "a row without {absent} renders without the key: {}",
            rows[1]
        );
    }

    let help = Command::new(bin())
        .current_dir(dir.path())
        .args(["ledger", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&help.stdout).into_owned();
    for key in ["family", "hop_budget", "origin", "deadline", "labels"] {
        assert!(
            help.contains(key),
            "`onlyne ledger --help` names the key it prints: {key}"
        );
    }
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

/// `schema <target>` is a public surface: each target answers with one compiled
/// JSON Schema object naming its own type, and the two targets carry the key set
/// of the file they describe.
#[test]
fn schema_prints_the_compiled_document_per_target() {
    let dir = tempfile::tempdir().unwrap();

    for (target, title, key) in [
        ("client", "ClientConfig", "backend"),
        ("spec", "Spec", "server"),
    ] {
        let output = Command::new(bin())
            .current_dir(dir.path())
            .args(["schema", target])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(EXIT_OK),
            "`schema {target}` answers: {}",
            stderr_of(&output)
        );
        assert!(
            stderr_of(&output).is_empty(),
            "a printed schema writes nothing to stderr"
        );
        assert!(
            output.stdout.starts_with(b"{"),
            "`schema {target}` opens its json object"
        );
        let schema: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("`schema {target}` must print json: {error}"));
        assert_eq!(schema["title"], title, "the document names its type");
        let properties = schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("`schema {target}` describes an object surface"));
        assert!(
            properties.contains_key(key),
            "`schema {target}` carries the `{key}` key of its file: {properties:?}"
        );
    }
}

/// `--pretty` re-renders the embedded bytes: the same document, one key per
/// indented line, and the pair agree field by field.
#[test]
fn schema_pretty_reprints_the_same_document() {
    let dir = tempfile::tempdir().unwrap();
    let plain = Command::new(bin())
        .current_dir(dir.path())
        .args(["schema", "client"])
        .output()
        .unwrap();
    let pretty = Command::new(bin())
        .current_dir(dir.path())
        .args(["--pretty", "schema", "client"])
        .output()
        .unwrap();

    assert_eq!(plain.status.code(), Some(EXIT_OK));
    assert_eq!(pretty.status.code(), Some(EXIT_OK));
    let plain_text = String::from_utf8_lossy(&plain.stdout).into_owned();
    let pretty_text = String::from_utf8_lossy(&pretty.stdout).into_owned();
    assert!(
        pretty_text.contains("\n  \""),
        "the pretty document breaks each key onto its own indented line:\n{pretty_text}"
    );
    let plain_value: serde_json::Value = serde_json::from_str(&plain_text).unwrap();
    let pretty_value: serde_json::Value = serde_json::from_str(&pretty_text).unwrap();
    assert_eq!(
        plain_value, pretty_value,
        "`--pretty` re-renders one document"
    );
}

/// A target outside the value enum is refused before a schema is read: exit 2,
/// the value named, and the accepted set printed for the operator to copy.
#[test]
fn schema_rejects_an_unknown_target() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(["schema", "bogus"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("bogus"),
        "the refusal names the value: {stderr}"
    );
    assert!(
        stderr.contains("[possible values: client, spec]"),
        "the refusal lists the accepted targets: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a clap refusal must not print a schema"
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
            "--force",
            SUPERVISOR_FLAG,
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
            "--force",
            SUPERVISOR_FLAG,
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
            "--force",
            SUPERVISOR_FLAG,
            "--task",
            "22222222-2222-4222-8222-222222222222",
            "recycle",
            "--reason",
            "rotate",
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

const ACK_MSG_ID: &str = "11111111-1111-4111-8111-111111111111";

/// `ack` and `reject` require an explicit reason, so a half-written terminal
/// decision is refused by clap before any socket is consulted.
#[test]
fn ack_and_reject_require_reason() {
    let dir = tempfile::tempdir().unwrap();

    for verb in ["ack", "reject"] {
        let output = Command::new(bin())
            .current_dir(dir.path())
            .args([verb, "--msg-id", ACK_MSG_ID])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
        let stderr = stderr_of(&output);
        assert!(
            stderr.contains(verb),
            "missing reason must name {verb}: {stderr}"
        );
        assert!(
            stderr.contains("--reason"),
            "missing reason must name the flag: {stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "a clap refusal prints a hint, not an answer"
        );
    }
}

/// `ack` and `reject` carry `AckArgs` directly, not an envelope, so `--request`
/// is refused like `control` instead of being silently ignored.
#[test]
fn request_is_refused_on_ack_and_reject() {
    let dir = tempfile::tempdir().unwrap();

    for verb in ["ack", "reject"] {
        let output = Command::new(bin())
            .current_dir(dir.path())
            .args([
                "--request",
                "{}",
                verb,
                "--force",
                SUPERVISOR_FLAG,
                "--msg-id",
                ACK_MSG_ID,
                "--reason",
                "operator decision",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
        assert_eq!(
            stderr_of(&output),
            format!("onlyne: --request is not supported by {verb}\n")
        );
        assert!(
            output.stdout.is_empty(),
            "a refused flag prints a hint, not an answer"
        );
    }
}

/// The role-surface verbs both serialize as `ClientOp::Ack`; only the subcommand
/// chooses `accepted`, and the daemon's ledger-event payload is printed intact.
#[test]
fn ack_and_reject_write_client_ack_decisions() {
    let dir = tempfile::tempdir().unwrap();

    for (verb, accepted, state) in [("ack", true, "accepted"), ("reject", false, "rejected")] {
        let workspace = dir.path().join(verb);
        let listener = role_listener(&workspace);
        let server = serve_once(
            listener,
            serde_json::json!({
                "f": "res",
                "id": "r1",
                "ok": true,
                "data": {
                    "ledger_event": {
                        "msg_id": ACK_MSG_ID,
                        "state": state,
                        "reason": "operator decision"
                    }
                }
            }),
        );

        let output = Command::new(bin())
            .current_dir(dir.path())
            .args([
                "--workspace",
                workspace.to_str().unwrap(),
                verb,
                "--force",
                SUPERVISOR_FLAG,
                "--msg-id",
                ACK_MSG_ID,
                "--op-id",
                VALID_OP_ID,
                "--reason",
                "operator decision",
            ])
            .output()
            .unwrap();
        let request = server
            .join()
            .unwrap()
            .expect("the CLI must reach the role socket");

        assert_eq!(output.status.code(), Some(EXIT_OK));
        assert_eq!(request["op"], "ack");
        assert_eq!(request["args"]["msg_id"], ACK_MSG_ID);
        assert_eq!(request["args"]["op_id"], VALID_OP_ID);
        assert_eq!(request["args"]["accepted"], accepted);
        assert_eq!(request["args"]["reason"], "operator decision");

        let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["ledger_event"]["msg_id"], ACK_MSG_ID);
        assert_eq!(body["data"]["ledger_event"]["state"], state);
        assert_eq!(body["data"]["ledger_event"]["reason"], "operator decision");
    }
}

/// A server refusal is printed as the daemon sent it, preserving its code and
/// human reason while returning the answer-failed exit code.
#[test]
fn reject_passes_server_error_through() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("role");
    let listener = role_listener(&workspace);
    let server = serve_once(
        listener,
        serde_json::json!({
            "f": "res",
            "id": "r1",
            "ok": false,
            "error": {
                "code": "forbidden",
                "message": "not this role's delivery",
                "field": "msg_id"
            }
        }),
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--workspace",
            workspace.to_str().unwrap(),
            "reject",
            "--force",
            SUPERVISOR_FLAG,
            "--msg-id",
            ACK_MSG_ID,
            "--reason",
            "not mine",
        ])
        .output()
        .unwrap();
    server
        .join()
        .unwrap()
        .expect("the CLI must reach the role socket");

    assert_eq!(output.status.code(), Some(EXIT_ANSWER_FAILED));
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["code"], "forbidden");
    assert_eq!(body["error"]["message"], "not this role's delivery");
    assert_eq!(body["error"]["field"], "msg_id");
}

const CONTROL_TASK: &str = "22222222-2222-4222-8222-222222222222";

/// `control cancel` without `--reason` is a clap refusal that names the verb.
#[test]
fn control_cancel_without_reason_names_the_verb() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(["control", "--task", CONTROL_TASK, "cancel"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("cancel"),
        "missing --reason must name cancel: {stderr}"
    );
    assert!(
        stderr.contains("--reason"),
        "missing --reason must name the flag: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a clap refusal prints a hint, not an answer"
    );
}

/// `control recycle` without `--reason` is a clap refusal that names the verb.
#[test]
fn control_recycle_without_reason_names_the_verb() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(["control", "--task", CONTROL_TASK, "recycle"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("recycle"),
        "missing --reason must name recycle: {stderr}"
    );
    assert!(
        stderr.contains("--reason"),
        "missing --reason must name the flag: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a clap refusal prints a hint, not an answer"
    );
}

/// `--reason` on `cancel` and `recycle` passes clap and reaches socket resolution.
#[test]
fn control_reason_passes_clap_on_cancel_and_recycle() {
    let dir = tempfile::tempdir().unwrap();
    for verb in ["cancel", "recycle"] {
        let output = Command::new(bin())
            .current_dir(dir.path())
            .args([
                "control",
                "--force",
                SUPERVISOR_FLAG,
                "--task",
                CONTROL_TASK,
                verb,
                "--reason",
                "rotate",
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(EXIT_NO_SOCKET),
            "{verb} with --reason must pass clap: {}",
            stderr_of(&output)
        );
        assert_eq!(stderr_of(&output), format!("{NO_SOCKET_MESSAGE}\n"));
        assert!(
            output.stdout.is_empty(),
            "a local resolution failure must not print an answer"
        );
    }
}

/// `control probe` does not take `--reason` and still passes clap.
#[test]
fn control_probe_does_not_require_reason() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "control",
            "--force",
            SUPERVISOR_FLAG,
            "--task",
            CONTROL_TASK,
            "probe",
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(EXIT_NO_SOCKET),
        "probe without --reason must pass clap: {}",
        stderr_of(&output)
    );
}

/// `control focus` carries no reason and sends a control request that names the
/// op and the task.
#[test]
fn control_focus_sends_the_named_op() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("role");
    let listener = role_listener(&workspace);
    let server = serve_once(
        listener,
        serde_json::json!({"f": "res", "id": "r1", "ok": true, "data": {}}),
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--workspace",
            workspace.to_str().unwrap(),
            "control",
            "--force",
            SUPERVISOR_FLAG,
            "--task",
            CONTROL_TASK,
            "focus",
        ])
        .output()
        .unwrap();
    let request = server
        .join()
        .unwrap()
        .expect("the CLI must reach the role socket");

    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "focus must pass clap: {}",
        stderr_of(&output)
    );
    assert_eq!(request["op"], "control");
    assert_eq!(request["args"]["op"]["op"], "focus");
    assert_eq!(request["args"]["op"]["task_id"], CONTROL_TASK);
}

/// An admin `control` with no `--to` reads the task's session row and addresses
/// the op to the role that row names, which is the role the server itself reads
/// when it resolves a control op's owner.
#[test]
fn control_without_to_addresses_the_role_that_owns_the_task() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let listener = admin_listener(&root);
    let server = serve_sequence(
        listener,
        vec![
            serde_json::json!({
                "f": "res",
                "id": "r1",
                "ok": true,
                "data": {
                    "sessions": [{
                        "task_id": CONTROL_TASK,
                        "role": "scriber",
                    }],
                },
            }),
            serde_json::json!({"f": "res", "id": "r2", "ok": true, "data": {}}),
        ],
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--server-root",
            root.to_str().unwrap(),
            "control",
            "--force",
            SUPERVISOR_FLAG,
            "--from",
            "_supervisor",
            "--task",
            CONTROL_TASK,
            "cancel",
            "--reason",
            "operator close",
        ])
        .output()
        .unwrap();
    let frames = server.join().unwrap();

    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "a control op addressed to the owner must be accepted: {}",
        stderr_of(&output)
    );
    assert_eq!(
        frames.len(),
        2,
        "one session read resolves the owner, then the control goes out"
    );
    assert_eq!(frames[0]["op"], "sessions");
    assert_eq!(frames[0]["args"]["task_id"], CONTROL_TASK);
    assert_eq!(frames[1]["op"], "control");
    assert_eq!(frames[1]["args"]["to"], "scriber");
    assert_eq!(
        frames[1]["args"]["from"], "_supervisor",
        "the sender stays the role the operator named"
    );
    assert_eq!(frames[1]["args"]["op"]["op"], "cancel");
    assert_eq!(frames[1]["args"]["op"]["task_id"], CONTROL_TASK);
}

/// An explicit `--to` is the whole answer: the op is addressed where it says
/// with no session read in front of it.
#[test]
fn control_with_an_explicit_to_wins_over_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let listener = admin_listener(&root);
    let server = serve_once(
        listener,
        serde_json::json!({"f": "res", "id": "r1", "ok": true, "data": {}}),
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--server-root",
            root.to_str().unwrap(),
            "control",
            "--force",
            SUPERVISOR_FLAG,
            "--from",
            "_supervisor",
            "--to",
            "scriber",
            "--task",
            CONTROL_TASK,
            "cancel",
            "--reason",
            "operator close",
        ])
        .output()
        .unwrap();
    let request = server
        .join()
        .unwrap()
        .expect("the CLI must reach the admin socket");

    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "an explicit --to must reach the socket: {}",
        stderr_of(&output)
    );
    assert_eq!(
        request["op"], "control",
        "an explicit --to needs no session read, so the control is the only frame"
    );
    assert_eq!(request["args"]["to"], "scriber");
}

/// A task whose session row names no role has nobody to answer the op: the
/// admin surface refuses after the read and before the control frame, naming
/// the task and the flag that states the destination.
#[test]
fn control_without_a_session_refuses_and_names_to() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("srv");
    let listener = admin_listener(&root);
    let server = serve_once(
        listener,
        serde_json::json!({"f": "res", "id": "r1", "ok": true, "data": {"sessions": []}}),
    );

    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "--server-root",
            root.to_str().unwrap(),
            "control",
            "--force",
            SUPERVISOR_FLAG,
            "--from",
            "_supervisor",
            "--task",
            CONTROL_TASK,
            "cancel",
            "--reason",
            "operator close",
        ])
        .output()
        .unwrap();
    let request = server
        .join()
        .unwrap()
        .expect("the CLI must reach the admin socket");

    assert_eq!(output.status.code(), Some(EXIT_REFUSAL));
    assert_eq!(
        stderr_of(&output),
        format!(
            "onlyne: no session owns task {CONTROL_TASK}; pass --to <role> to say where the \
             control goes\n"
        )
    );
    assert!(
        output.stdout.is_empty(),
        "a refusal prints a hint, not an answer"
    );
    assert_eq!(
        request["op"], "sessions",
        "the refusal leaves the session read as the only frame written"
    );
}

/// Operators type the task on the verb's tail: `control cancel --task X`. clap
/// carries `--task`, `--from`, and the supervisor pair as globals, so both
/// readings parse, and the pair reads on either side of the op token.
#[test]
fn control_flags_read_after_the_verb() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args([
            "control",
            "cancel",
            "--task",
            CONTROL_TASK,
            "--reason",
            "rotate",
            "--from",
            "bench",
            "--force",
            SUPERVISOR_FLAG,
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(EXIT_NO_SOCKET),
        "flags after the verb must pass clap: {}",
        stderr_of(&output)
    );
}

/// A control op names a task (D12). The check moved from clap to the verb, so it
/// still exits 2 and still names the flag.
#[test]
fn control_without_task_names_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(["control", "probe", "--from", "bench"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("--task"),
        "a control op without a task names the flag: {stderr}"
    );
}

/// `control cancel --help` and `control recycle --help` list `--reason`.
/// `control probe --help` does not.
#[test]
fn control_subcommand_help_lists_reason_only_where_required() {
    let dir = tempfile::tempdir().unwrap();
    for verb in ["cancel", "recycle"] {
        let output = Command::new(bin())
            .current_dir(dir.path())
            .args(["control", verb, "--help"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(EXIT_OK));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("--reason"),
            "{verb} --help must list --reason: {stdout}"
        );
    }
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(["control", "probe", "--help"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(EXIT_OK));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("--reason"),
        "probe --help must not list --reason: {stdout}"
    );
}

/// A pinned `op_id` in the protocol's own spelling, `o-` plus uuid v4.
const VALID_OP_ID: &str = "o-33333333-3333-4333-8333-333333333333";

/// A task id a completion may carry: the envelope validator requires a uuid.
const TEST_TASK: &str = "44444444-4444-4444-8444-444444444444";

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
fn admin_listener(root: &Path) -> LocalListenerSync {
    let socket = root.join(".onlyne").join("run").join("s");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&socket);
    bind_local_sync_poll(&socket).unwrap()
}

/// The role socket path `--workspace` resolves, bound and ready to answer.
fn role_listener(workspace: &Path) -> LocalListenerSync {
    let socket = workspace.join(".onlyne").join("run").join("s");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&socket);
    bind_local_sync_poll(&socket).unwrap()
}

/// Answer one request frame on `listener` and hand back the frame it carried.
///
/// The accept loop gives up after ten seconds, so a CLI that never connects
/// fails an assertion instead of hanging the suite.
fn serve_once(
    listener: LocalListenerSync,
    answer: serde_json::Value,
) -> thread::JoinHandle<Option<serde_json::Value>> {
    thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok(mut stream) => {
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

/// Answer a scripted run of request frames on one connection, in order, and hand
/// back every frame the CLI wrote.
///
/// The accept loop gives up after ten seconds, so a CLI that never connects leaves
/// the joining test an empty frame list to assert on. A CLI that writes more frames
/// than the script carries meets a stream with nothing left to answer, and reports
/// that through its own exchange-error path.
fn serve_sequence(
    listener: LocalListenerSync,
    answers: Vec<serde_json::Value>,
) -> thread::JoinHandle<Vec<serde_json::Value>> {
    thread::spawn(move || {
        let frame_count = answers.len();
        let mut answers = answers.into_iter();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match listener.accept() {
                Ok(mut stream) => {
                    let mut requests = Vec::with_capacity(frame_count);
                    for mut answer in answers.by_ref() {
                        let request = read_frame(&mut stream);
                        answer["id"] = request["id"].clone();
                        write_frame(&mut stream, &answer);
                        requests.push(request);
                    }
                    return requests;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() > deadline {
                        return Vec::new();
                    }
                    thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => return Vec::new(),
            }
        }
    })
}

/// Read one length-prefixed JSON frame from a blocking stream.
fn read_frame(stream: &mut LocalStreamSync) -> serde_json::Value {
    let mut header = [0u8; 4];
    read_exact_retry(stream, &mut header);
    let mut payload = vec![0u8; u32::from_be_bytes(header) as usize];
    read_exact_retry(stream, &mut payload);
    serde_json::from_slice(&payload).unwrap()
}

/// Write one length-prefixed JSON frame to a blocking stream.
fn write_frame(stream: &mut LocalStreamSync, value: &serde_json::Value) {
    let payload = serde_json::to_vec(value).unwrap();
    write_all_retry(stream, &(payload.len() as u32).to_be_bytes());
    write_all_retry(stream, &payload);
    let _ = stream.flush();
}

/// `ListenerNonblockingMode::Accept` can leak WouldBlock onto the accepted
/// stream (macOS UDS). Retry until the byte count is in, matching the old
/// `set_nonblocking(false)` on `std::os::unix::net::UnixStream`.
fn read_exact_retry(stream: &mut LocalStreamSync, buf: &mut [u8]) {
    let mut filled = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while filled < buf.len() {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => panic!("socket closed before a full frame arrived"),
            Ok(n) => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    panic!("timed out reading a frame");
                }
                thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

fn write_all_retry(stream: &mut LocalStreamSync, buf: &[u8]) {
    let mut written = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while written < buf.len() {
        match stream.write(&buf[written..]) {
            Ok(0) => panic!("socket closed before the frame was written"),
            Ok(n) => written += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    panic!("timed out writing a frame");
                }
                thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

/// Copy the built CLI into `dir`, so `resolve_sibling`'s exe-adjacent probe
/// reads that directory and the real daemons in the cargo target directory
/// cannot shadow the stubs beside it.
fn onlyne_in(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    #[cfg(unix)]
    let copy = dir.join("onlyne");
    #[cfg(windows)]
    let copy = dir.join("onlyne.exe");
    // Write via a temp name, sync, close, then rename so Linux does not
    // ETXTBSY-exec a file whose write handle is still open.
    let staging = copy.with_extension("copying");
    {
        let mut src = File::open(env!("CARGO_BIN_EXE_onlyne")).unwrap();
        let mut dst = File::create(&staging).unwrap();
        std::io::copy(&mut src, &mut dst).unwrap();
        dst.sync_all().unwrap();
    }
    std::fs::rename(&staging, &copy).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&copy).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&copy, perms).unwrap();
    }
    copy
}

/// Write an executable stub binary named `name` into `dir`.
fn stub_binary(dir: &Path, name: &str, script: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }
    #[cfg(windows)]
    {
        let path = dir.join(format!("{name}.cmd"));
        std::fs::write(&path, unix_script_to_cmd(script)).unwrap();
        path
    }
}

#[cfg(windows)]
fn unix_script_to_cmd(script: &str) -> String {
    if script.contains("exit 4") {
        return "@echo off\r\nexit 4\r\n".into();
    }
    if let Some(marker) = script.lines().find_map(|line| {
        line.trim()
            .strip_prefix("touch ")
            .map(|rest| rest.trim_matches('\'').to_string())
    }) {
        return format!("@echo off\r\ntype nul > \"{marker}\"\r\n");
    }
    let mut cmd = String::from("@echo off\r\n");
    if script.contains("ONLYNE_ARGV") {
        // Labels inside `(...)` are a cmd.exe syntax error (exit 255).
        // Keep goto/for at file scope. `for %%A in (%*)` matches unix
        // `printf '%s\n' "$@"` for the space-free paths these tests use.
        cmd.push_str(
            "if not defined ONLYNE_ARGV goto onlyne_after_argv\r\n\
             >\"%ONLYNE_ARGV%\" (\r\n\
             for %%A in (%*) do @echo %%A\r\n\
             )\r\n\
             :onlyne_after_argv\r\n",
        );
    }
    if script.contains("rendering spec") {
        cmd.push_str("echo rendering spec 1>&2\r\n");
    }
    if script.contains("[[client]]") {
        // `echo(` prints `[` literally; plain `echo [` can be parsed as a
        // command grouping.
        cmd.push_str("echo([[client]] roles: worker\r\n");
    }
    cmd.push_str("exit /b 0\r\n");
    cmd
}

/// cmd.exe `%*` / `echo %%A` is the stub's transport, not the argv the CLI
/// forwarded: lines are CRLF, and tokens that contain `\` come back wrapped
/// in double quotes. Unix `printf '%s\n'` is already LF + bare tokens.
/// Only a fully quoted line is unwrapped (cmd's own quoting), never interior
/// quotes.
fn read_stub_argv(path: &Path) -> String {
    let raw = std::fs::read_to_string(path).unwrap();
    let unix = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::new();
    let mut lines = unix.split('\n').peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() && lines.peek().is_none() {
            break;
        }
        out.push_str(unquote_cmd_echo_line(line));
        out.push('\n');
    }
    out
}

fn unquote_cmd_echo_line(line: &str) -> &str {
    let bytes = line.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        &line[1..line.len() - 1]
    } else {
        line
    }
}

/// `PATH` with `dir` in front, so a stub shadows any real sibling.
fn path_with(dir: &Path) -> OsString {
    let mut value = OsString::from(dir.as_os_str());
    #[cfg(windows)]
    value.push(";");
    #[cfg(not(windows))]
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

    let output = spawn_output(
        Command::new(&cli)
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
            ]),
    );
    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert_eq!(
        read_stub_argv(&argv_out),
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

    let output = spawn_output(
        Command::new(&cli)
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
            ]),
    );
    assert_eq!(output.status.code(), Some(EXIT_OK));
    assert_eq!(
        read_stub_argv(&argv_out),
        format!(
            "run\ntelegram\n--server-root\n{}\n--token\nt0ken\n",
            root.display()
        )
    );
}

/// `skill export` reads the documents out of the binary, so it needs no socket
/// and no source checkout: the four land under `.agents/skills` in the working
/// directory, and a second run over that tree matches every byte.
#[test]
fn skill_export_writes_the_shipped_documents_under_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let export = || {
        Command::new(bin())
            .current_dir(dir.path())
            .args(["skill", "export"])
            .output()
            .unwrap()
    };
    let output = export();
    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "{}",
        stderr_of(&output)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        body["data"]["written"].as_array().unwrap().len(),
        4,
        "{body}"
    );
    for name in [
        "onlyne-supervisor",
        "onlyne-role",
        "onlyne-role-payload-v2",
        "onlyne",
    ] {
        let file = dir
            .path()
            .join(".agents/skills")
            .join(name)
            .join("SKILL.md");
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        assert!(
            text.starts_with("---\nname:"),
            "{} carries no skill frontmatter",
            file.display()
        );
    }

    let again = export();
    assert_eq!(again.status.code(), Some(EXIT_OK), "{}", stderr_of(&again));
    let body: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert!(
        body["data"]["written"].as_array().unwrap().is_empty(),
        "a matching document is left alone: {body}"
    );
    assert_eq!(
        body["data"]["unchanged"].as_array().unwrap().len(),
        4,
        "{body}"
    );
}

/// A file whose bytes differ from the shipped document stops the export by
/// name, and `--force` is what rewrites it. `--set` narrows the selection.
#[test]
fn skill_export_refuses_a_different_document_until_force_rewrites_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("skills-root");
    let supervisor = root.join("onlyne-supervisor").join("SKILL.md");
    let role = root.join("onlyne-role").join("SKILL.md");
    let dest = |args: &[&str]| {
        let mut command = Command::new(bin());
        command.current_dir(dir.path()).args(args).arg(&root);
        command.output().unwrap()
    };

    let narrowed = dest(&["skill", "export", "--set", "supervisor", "--dest"]);
    assert_eq!(
        narrowed.status.code(),
        Some(EXIT_OK),
        "{}",
        stderr_of(&narrowed)
    );
    assert!(supervisor.is_file(), "--set supervisor writes the manual");
    assert!(!role.exists(), "--set supervisor selects that group alone");

    std::fs::create_dir_all(role.parent().unwrap()).unwrap();
    std::fs::write(&role, "a document an operator edited\n").unwrap();
    let refused = dest(&["skill", "export", "--dest"]);
    assert_eq!(refused.status.code(), Some(EXIT_REFUSAL));
    assert_eq!(
        stderr_of(&refused),
        format!(
            "onlyne: refusing to overwrite {}; pass --force\n",
            role.display()
        )
    );
    assert!(
        refused.stdout.is_empty(),
        "a refusal answers nothing on stdout"
    );
    assert_eq!(
        std::fs::read_to_string(&role).unwrap(),
        "a document an operator edited\n",
        "a refusal writes nothing"
    );

    let forced = dest(&["skill", "export", "--force", "--dest"]);
    assert_eq!(
        forced.status.code(),
        Some(EXIT_OK),
        "{}",
        stderr_of(&forced)
    );
    let text = std::fs::read_to_string(&role).unwrap();
    assert!(
        text.starts_with("---\nname: onlyne-role"),
        "the shipped role document came back: {text:.80}"
    );
}
