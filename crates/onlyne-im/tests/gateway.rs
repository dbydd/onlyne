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

/// The gateway role this test mints, the conversation's slug, and the
/// conversation it carries.
const ROLE: &str = "im";
const SLUG: &str = "tg-group-7788";
const CONVERSATION: &str = "-1007788";
const GATEWAY: &str = "tg1";
const CHAT_KEY: &str = "0123456789abcdef0123456789abcdef";

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
/// with its chat key, as `onlyne-im init` and `generate` would.
fn write_root(root: &Path, backend: &Path, script: &Path, conversation: &str) {
    write_root_with_listen(root, backend, script, conversation, "")
}

/// [`write_root`] with an `[im.listen]` table of the caller's own choosing.
fn write_root_with_listen(
    root: &Path,
    backend: &Path,
    script: &Path,
    conversation: &str,
    listen: &str,
) {
    fs::create_dir_all(root.join(".onlyne")).unwrap();
    fs::create_dir_all(root.join(".onlyne/keys")).unwrap();
    fs::write(root.join(".onlyne/keys/role.key"), [7u8; 32]).unwrap();
    fs::write(
        root.join(".onlyne/config.toml"),
        format!(
            r#"
[gateway]
role = "{ROLE}"

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
{listen}
"#,
            backend = backend.display(),
            script = script.display(),
        ),
    )
    .unwrap();

    let workspace = root.join("conversations").join(SLUG);
    fs::create_dir_all(workspace.join(".onlyne")).unwrap();
    fs::create_dir_all(workspace.join("media")).unwrap();
    fs::write(workspace.join(".onlyne/chat.key"), CHAT_KEY).unwrap();
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
    assert_eq!(value["chat"], CHAT_KEY);
    assert_eq!(value["gateway"], GATEWAY);
    assert_eq!(value["event"]["id"], "msg-42");
    assert_eq!(value["event"]["text"], "deploy please");

    // `argv[1]` is the mode word, and the conversation's own paths arrive as
    // environment variables rather than as arithmetic inside the script.
    let mode = fs::read_to_string(root.join("seen.json.mode")).unwrap();
    assert_eq!(mode, "trigger");
    let workspace = root.join("conversations").join(SLUG);
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

/// `[im.listen]` is absent, so the gateway serves every conversation: an event
/// from one no workspace carries is adopted and served.
#[test]
fn an_unknown_conversation_is_adopted_and_served() {
    // The default is listen-all, so an event from a conversation nobody
    // registered is *adopted*: a chat key is minted, a workspace is written
    // under `conversations/`, and the trigger script runs for it. Without this
    // the first chat id is unknowable — it only exists once the bot is in the
    // conversation.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let backend = root.join("fake_backend.py");
    let script = root.join("trigger.sh");
    let out = root.join("seen.json");
    write_fake_backend(&backend, "-7777777");
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

    // The gateway stays up (the backend sleeps) and the script has run by now.
    std::thread::sleep(Duration::from_secs(3));
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        out.exists(),
        "the trigger script never ran for the adopted conversation"
    );
    let log = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        log.contains("adopted a conversation nobody registered"),
        "the adoption is not explained in the log:\n{log}"
    );
    // The adoption is on disk, so a restart serves the same conversation under
    // the same chat key rather than minting a rival.
    let adopted = root.join("conversations/tg1-7777777/.onlyne/chat.key");
    assert!(adopted.is_file(), "the adopted workspace is not on disk");
    let coordinates =
        fs::read_to_string(root.join("conversations/tg1-7777777/.onlyne/config.toml")).unwrap();
    assert!(coordinates.contains("-7777777"), "{coordinates}");
}

/// `[im.listen] deny` still refuses: a deny entry wins over the default
/// listen-all, and the event neither adopts nor reaches its script.
#[test]
fn a_denied_conversation_is_refused_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let backend = root.join("fake_backend.py");
    let script = root.join("trigger.sh");
    let out = root.join("seen.json");
    write_fake_backend(&backend, "-8888888");
    write_trigger(&script, &out);
    write_root_with_listen(
        root,
        &backend,
        &script,
        CONVERSATION,
        "\n[im.listen]\ndeny = [\"-8888888\"]\n",
    );

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

    std::thread::sleep(Duration::from_secs(3));
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        !out.exists(),
        "the trigger script ran for a conversation the policy denies"
    );
    assert!(
        !root.join("conversations/tg1-8888888").exists(),
        "a denied conversation was adopted anyway"
    );
    let log = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        log.contains("the listen policy refuses this conversation"),
        "the refusal is not explained in the log:\n{log}"
    );
}

/// `[im.listen] allow` narrows the default: an id not named is refused, and
/// the registry on disk is left untouched.
#[test]
fn an_unlisted_conversation_is_refused_under_an_allow_list() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let backend = root.join("fake_backend.py");
    let script = root.join("trigger.sh");
    let out = root.join("seen.json");
    write_fake_backend(&backend, "-9999999");
    write_trigger(&script, &out);
    write_root_with_listen(
        root,
        &backend,
        &script,
        CONVERSATION,
        "\n[im.listen]\nallow = [\"-1001234\"]\n",
    );

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

    std::thread::sleep(Duration::from_secs(3));
    let _ = child.kill();
    let _ = child.wait();
    assert!(!out.exists(), "the trigger script ran for an unlisted id");
    assert!(
        !root.join("conversations/tg1-9999999").exists(),
        "an unlisted id was adopted"
    );
}
