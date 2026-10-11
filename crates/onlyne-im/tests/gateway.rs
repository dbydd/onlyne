//! The gateway, run for real: a fake backend emits one platform event and the
//! trigger script receives it, with the conversation's own paths in its
//! environment. This is the contract of `docs/v2.2-IM-GATEWAY-PLAN.md` §4.3 and
//! §5, end to end over the new gateway-root layout — argv mode, event JSON on
//! stdin, `ONLYNE_SOCKET` / `ONLYNE_IM_WORKSPACE` / `ONLYNE_IM_MEDIA` in the
//! environment, and the inbound event appended to that conversation's chat log.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The role this test mints, and the conversation it carries.
const ROLE: &str = "tg-group-7788";
const CONVERSATION: &str = "-1007788";
const GATEWAY: &str = "tg1";

/// A backend that writes one `hello` then one `event` on stdout, both as
/// length-prefixed frames (the `onlyne-wire` codec). Its argv is the gateway
/// config's `run`, so the gateway spawns it exactly as an operator's would.
fn write_fake_backend(path: &Path, conversation: &str) {
    let script = format!(
        r#"
import json, struct, sys, time
def frame(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(struct.pack(">I", len(body)) + body)
    sys.stdout.buffer.flush()
frame({{"op": "hello", "args": {{"platform": "telegram", "version": "test",
                                "capabilities": ["message"]}}}})
frame({{"op": "event", "args": {{
    "gateway": "{GATEWAY}", "platform": "telegram", "channel": "telegram",
    "conversation": "{conversation}", "kind": "message", "id": "msg-42",
    "from": {{"id": "u-1", "name": "Ada"}}, "text": "deploy please",
    "at": "2026-10-10T12:00:00Z"}}}})
time.sleep(30)
"#
    );
    fs::write(path, script).unwrap();
}

/// A trigger script that records `$1`, its three environment paths, and its
/// stdin (the event JSON).
fn write_trigger(path: &Path, out: &Path) {
    fs::write(
        path,
        format!(
            "#!/bin/sh\nprintf '%s' \"$1\" > {out}.mode\n\
             printf '%s' \"$ONLYNE_IM_WORKSPACE\" > {out}.workspace\n\
             printf '%s' \"$ONLYNE_IM_MEDIA\" > {out}.media\n\
             cat > {out}\n",
            out = out.display()
        ),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Lay a gateway root down: the root document, and one conversation workspace
/// with a real key, as `onlyne-im init` and `generate` would.
fn write_root(root: &Path, backend: &Path, script: &Path, conversation: &str) {
    fs::create_dir_all(root.join(".onlyne")).unwrap();
    fs::write(
        root.join(".onlyne/config.toml"),
        format!(
            r#"
[server]
endpoint = "127.0.0.1:59999"
cert_pin = "sha256/placeholder"

[[im.backend]]
gateway = "{GATEWAY}"
run = ["python3", "{backend}"]

[[im.push]]
mode = "trigger"
run = ["{script}"]
timeout = "10s"
"#,
            backend = backend.display(),
            script = script.display(),
        ),
    )
    .unwrap();

    let workspace = root.join("conversations").join(ROLE);
    fs::create_dir_all(workspace.join(".onlyne/keys")).unwrap();
    fs::create_dir_all(workspace.join("media")).unwrap();
    fs::write(workspace.join(".onlyne/keys/role.key"), [7u8; 32]).unwrap();
    fs::write(
        workspace.join(".onlyne/config.toml"),
        format!("[chat]\ngateway = \"{GATEWAY}\"\nconversation = \"{conversation}\"\n"),
    )
    .unwrap();
}

#[test]
fn an_inbound_event_reaches_the_trigger_script() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let backend = root.join("fake_backend.py");
    let script = root.join("trigger.sh");
    let out = root.join("seen.json");
    write_fake_backend(&backend, CONVERSATION);
    write_trigger(&script, &out);
    write_root(root, &backend, &script, CONVERSATION);

    let binary = env!("CARGO_BIN_EXE_onlyne-im");
    let log = root.join("gateway.log");
    let log_file = fs::File::create(&log).unwrap();
    let mut child = std::process::Command::new(binary)
        .args(["run", "--root"])
        .arg(root)
        .arg("--socket")
        .arg("/tmp/onlyne-smoke.sock")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .spawn()
        .expect("the gateway starts");

    // Wait for the event to land, and for content rather than mere existence:
    // the script creates its output file before it copies stdin into it. The
    // bound is generous because this spawns two processes.
    let deadline = Instant::now() + Duration::from_secs(60);
    let seen = loop {
        let seen = fs::read_to_string(&out).unwrap_or_default();
        if seen.contains("\"event\"") {
            break seen;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let why = fs::read_to_string(&log).unwrap_or_default();
            panic!("the trigger script never received the event; gateway log:\n{why}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = child.kill();
    let _ = child.wait();

    // The event JSON is the object §4.3 fixes: the role, the platform
    // coordinates, and the message.
    let value: serde_json::Value = serde_json::from_str(&seen).expect("the script got JSON");
    assert_eq!(value["mode"], "trigger");
    assert_eq!(value["role"], ROLE);
    assert_eq!(value["gateway"], GATEWAY);
    assert_eq!(value["event"]["id"], "msg-42");
    assert_eq!(value["event"]["text"], "deploy please");

    // `argv[1]` is the mode word, and the conversation's own paths arrive as
    // environment variables rather than as arithmetic inside the script.
    let mode = fs::read_to_string(root.join("seen.json.mode")).unwrap();
    assert_eq!(mode, "trigger");
    let workspace = root.join("conversations").join(ROLE);
    assert_eq!(
        fs::read_to_string(root.join("seen.json.workspace")).unwrap(),
        workspace.display().to_string()
    );
    assert_eq!(
        fs::read_to_string(root.join("seen.json.media")).unwrap(),
        workspace.join("media").display().to_string()
    );

    // The inbound event is on that conversation's own chat log, in its own
    // workspace, because the log is what an operator reads afterwards.
    let log = fs::read_to_string(workspace.join("chat.jsonl")).expect("the chat log is written");
    let line: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(line["direction"], "in");
    assert_eq!(line["id"], "msg-42");
    assert_eq!(line["text"], "deploy please");
}

/// An event for a conversation no workspace carries is dropped, not misrouted:
/// the workspace's existence is the gateway's whole list of what it serves.
#[test]
fn an_event_for_an_unknown_conversation_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let backend = root.join("fake_backend.py");
    let script = root.join("trigger.sh");
    let out = root.join("seen.json");
    write_fake_backend(&backend, "-9999999");
    write_trigger(&script, &out);
    write_root(root, &backend, &script, CONVERSATION);

    let binary = env!("CARGO_BIN_EXE_onlyne-im");
    let log = root.join("gateway.log");
    let log_file = fs::File::create(&log).unwrap();
    let mut child = std::process::Command::new(binary)
        .args(["run", "--root"])
        .arg(root)
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .spawn()
        .expect("the gateway starts");

    // The gateway stays up (the backend sleeps) and the script is never run.
    std::thread::sleep(Duration::from_secs(3));
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        !out.exists(),
        "the trigger script ran for a conversation this gateway does not serve"
    );
    let log = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        log.contains("no workspace under `conversations/`"),
        "the drop is not explained in the log:\n{log}"
    );
}
