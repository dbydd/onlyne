//! Live `onlyne-view` content over a real local adapter socket.

use onlyne_adapter::AdapterServer;
use onlyne_proto::{
    AdapterMsg, ByeNotice, ContentFrame, ErrorCode, HelloAck, HelloArgs, HostOp, MountKind,
    PROTOCOL_VERSION, PluginOp, ResBody, ServerInfo, WatchContentArgs,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::JoinHandle;
use std::time::Duration;

const TASK: &str = "t-live";

struct ConnectionScript {
    frames: Vec<ContentFrame>,
}

fn ack(hello: &HelloArgs) -> Result<HelloAck, (ErrorCode, String)> {
    assert_eq!(hello.kind, MountKind::Admin, "viewer hello mounts admin");
    assert!(hello.mount.is_none(), "admin hello carries mount: null");
    Ok(HelloAck {
        protocol: PROTOCOL_VERSION,
        role: "planner".to_string(),
        session_id: None,
        generation: 1,
        prose: String::new(),
        server: ServerInfo {
            connected: true,
            cluster: "local".to_string(),
            name: "scripted-view-host".to_string(),
        },
        host_capabilities: Vec::new(),
    })
}

fn content(seq: u64, record: Value) -> ContentFrame {
    ContentFrame {
        seq,
        task_id: TASK.to_string(),
        session_id: Some("s-live".to_string()),
        at: "2026-09-19T12:00:00Z".to_string(),
        record,
    }
}

fn spawn_host(socket: &Path, scripts: Vec<ConnectionScript>) -> JoinHandle<Vec<WatchContentArgs>> {
    let socket = socket.to_path_buf();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("scripted host runtime");
        runtime.block_on(async move {
            use onlyne_layout::local_socket::prelude::TokioListener;

            let listener = onlyne_layout::bind_local(&socket)
                .await
                .expect("bind scripted client socket");
            ready_tx.send(()).expect("announce bound socket");
            let mut subscriptions = Vec::new();
            for script in scripts {
                let stream = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                    .await
                    .expect("viewer connected before host timeout")
                    .expect("accept viewer");
                let mut connection = AdapterServer::accept(stream, ack)
                    .await
                    .expect("accept admin hello");
                let request =
                    tokio::time::timeout(Duration::from_secs(3), connection.inbound.recv())
                        .await
                        .expect("watch_content arrived before timeout")
                        .expect("viewer kept the connection through watch_content");
                let request_id = request.id.expect("watch_content is a request");
                let AdapterMsg::Plugin(PluginOp::WatchContent(args)) = request.msg else {
                    panic!("viewer sent a non-watch operation");
                };
                connection
                    .io
                    .respond(request_id, ResBody::ok(Value::Null))
                    .await
                    .expect("ack watch_content");
                subscriptions.push(args);
                for frame in script.frames {
                    connection
                        .io
                        .notify(AdapterMsg::Host(HostOp::Content(Box::new(frame))))
                        .await
                        .expect("push scripted content");
                }
                connection
                    .io
                    .notify(AdapterMsg::Host(HostOp::Bye(ByeNotice {
                        reason: "scripted reconnect".to_string(),
                    })))
                    .await
                    .expect("end scripted connection");
                // `notify` queues writes on the SDK writer. Let that finite queue
                // drain before dropping this connection to script a reconnect.
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            subscriptions
        })
    });
    ready_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("scripted socket became ready");
    worker
}

fn spawn_refusing_host(socket: &Path) -> JoinHandle<()> {
    let socket = socket.to_path_buf();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("refusing host runtime");
        runtime.block_on(async move {
            use onlyne_layout::local_socket::prelude::TokioListener;

            let listener = onlyne_layout::bind_local(&socket)
                .await
                .expect("bind refusing client socket");
            ready_tx.send(()).expect("announce bound socket");
            let stream = listener.accept().await.expect("accept viewer");
            let refused = AdapterServer::accept(stream, |_hello| {
                Err((ErrorCode::Forbidden, "viewer refused".to_string()))
            })
            .await;
            assert!(refused.is_err(), "the scripted hello is refused");
        });
    });
    ready_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("refusing socket became ready");
    worker
}

fn journal_path(dir: &Path) -> PathBuf {
    dir.join(format!(".onlyne/logs/session-{TASK}.events.jsonl"))
}

fn write_journal(dir: &Path, records: &[Value]) {
    let path = journal_path(dir);
    std::fs::create_dir_all(path.parent().expect("logs directory")).expect("create logs");
    let mut body = records
        .iter()
        .map(|record| serde_json::to_string(record).expect("serialize fixture"))
        .collect::<Vec<_>>()
        .join("\n");
    body.push('\n');
    std::fs::write(path, body).expect("write journal fixture");
}

fn run_view(dir: &Path, mode: &str, socket: Option<&Path>) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_onlyne-view"));
    command
        .args(["--once", "--workspace"])
        .arg(dir)
        .args(["--task", TASK, "--mode", mode]);
    if let Some(socket) = socket {
        command.arg("--socket").arg(socket);
    }
    let output = command.output().expect("run onlyne-view");
    assert!(
        output.status.success(),
        "viewer exited {} with stderr {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_live(
    dir: &Path,
    mode: &str,
    scripts: Vec<ConnectionScript>,
) -> (String, Vec<WatchContentArgs>) {
    let socket = dir.join("client.sock");
    let host = spawn_host(&socket, scripts);
    let output = run_view(dir, mode, Some(&socket));
    let subscriptions = host.join().expect("scripted host thread");
    (output, subscriptions)
}

fn body(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.starts_with('│'))
        .map(|line| line.strip_prefix('│').unwrap_or(line))
        .map(|line| line.strip_suffix('│').unwrap_or(line))
        .map(str::trim_end)
        .collect()
}

fn conversation() -> Vec<Value> {
    vec![
        json!({"onlyne": {
            "kind": "dispatch",
            "task_id": TASK,
            "prose": "Ship it.",
            "at": "2026-09-19T12:00:00Z"
        }}),
        json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": {"type": "text", "text": "Checking."}
        }),
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "Done."}
        }),
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-1",
            "status": "in_progress",
            "title": "Edit file.rs",
            "kind": "edit"
        }),
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-1",
            "status": "completed"
        }),
    ]
}

#[test]
fn a_refused_admin_handshake_falls_back_to_the_journal() {
    let dir = tempfile::tempdir().expect("workspace");
    let records = conversation();
    write_journal(dir.path(), &records);
    let socket = dir.path().join("refusing.sock");
    let host = spawn_refusing_host(&socket);

    let output = run_view(dir.path(), "full", Some(&socket));
    host.join().expect("refusing host thread");

    assert_eq!(
        body(&output),
        vec![
            "> Ship it.",
            "",
            "~ Checking.",
            "",
            "Done.",
            "",
            "  [edit] Edit file.rs (completed)",
        ],
        "refusing the live mount leaves the journal render intact\n{output}"
    );
    assert!(
        output.contains("source journal fallback (socket"),
        "fallback is visible in the footer\n{output}"
    );
    assert!(
        output.contains("refusing.sock: hello on"),
        "the footer identifies the handshake stage that fell back\n{output}"
    );
}

#[test]
fn a_real_socket_renders_the_same_full_and_compact_content_as_the_file() {
    let records = conversation();
    for mode in ["full", "compact"] {
        let file_dir = tempfile::tempdir().expect("file workspace");
        write_journal(file_dir.path(), &records);
        let from_file = run_view(file_dir.path(), mode, None);

        let socket_dir = tempfile::tempdir().expect("socket workspace");
        let frames = records
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, record)| content(index as u64 + 1, record))
            .collect();
        let (from_socket, subscriptions) =
            run_live(socket_dir.path(), mode, vec![ConnectionScript { frames }]);

        assert_eq!(
            body(&from_socket),
            body(&from_file),
            "{mode} classifies and groups socket Values exactly like journal lines\n{from_socket}"
        );
        assert_eq!(subscriptions.len(), 1);
        assert_eq!(subscriptions[0].task_id.as_deref(), Some(TASK));
        assert_eq!(subscriptions[0].since, None);
        assert!(
            from_socket.contains("source socket"),
            "live source is visible in the footer\n{from_socket}"
        );
    }
}

#[test]
fn the_head_inclusive_chunk_repeat_is_dropped_before_grouping() {
    let dir = tempfile::tempdir().expect("workspace");
    let head = json!({
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": "Hello"}
    });
    let continuation = json!({
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": " world"}
    });
    write_journal(dir.path(), std::slice::from_ref(&head));

    let (output, subscriptions) = run_live(
        dir.path(),
        "full",
        vec![ConnectionScript {
            frames: vec![content(7, head), content(8, continuation)],
        }],
    );

    assert_eq!(body(&output), vec!["Hello world"]);
    assert!(
        !output.contains("HelloHello world"),
        "the replay never reached the chunk accumulator\n{output}"
    );
    assert_eq!(subscriptions[0].since, None);
}

#[test]
fn reconnect_resumes_after_the_second_frame_and_appends_only_newer_records() {
    let dir = tempfile::tempdir().expect("workspace");
    let chunk = |text: &str| {
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": text}
        })
    };
    let (output, subscriptions) = run_live(
        dir.path(),
        "full",
        vec![
            ConnectionScript {
                frames: vec![content(40, chunk("A")), content(41, chunk("B"))],
            },
            ConnectionScript {
                frames: vec![content(42, chunk("C"))],
            },
        ],
    );

    assert_eq!(
        subscriptions
            .iter()
            .map(|args| args.since)
            .collect::<Vec<_>>(),
        vec![None, Some(41)],
        "the second watch carries the last frame's cursor"
    );
    assert_eq!(
        body(&output),
        vec!["ABC"],
        "records at or before the resume cursor were not rendered again\n{output}"
    );
}
