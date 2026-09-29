//! Slice 7's acceptance, one case per line the contract promises
//! (`docs/v2-CONTRACT.md` §"Slice 7").
//!
//! Every case runs a real server process whose spec declares a real `[[hook]]`,
//! and the script that hook names is a real process that reads its event on
//! stdin and writes what it received to a file. Nothing here stubs the runner:
//! the claims are about what a script actually gets and what the server
//! actually records, and a stub would answer neither.
//!
//! The trigger is `reload`: each one appends exactly one `spec_reloaded` event
//! to the server's stream, so a case can count events, name them by `seq`, and
//! never depend on a session to produce one.

use onlyne_proto::{ClientOp, ErrorCode, Event, EventRow, PublishEventArgs, QuerySessionsArgs};
use onlyne_server::router::{self, Session};
use onlyne_server::state::{Server, ServerInit};
use onlyne_testkit::harness::Cluster;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// How long a case waits for a hook process to leave its mark.
const WAIT: Duration = Duration::from_secs(20);

/// The spec every cluster in this file starts from: one role, no routes, and
/// the hook the case is about.
fn spec_with_hooks(hooks: &str) -> String {
    format!(
        r#"[server]
name = "hooks"
listen = "127.0.0.1:0"

[[client]]
role = "planner"
key = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
allowed_senders = ["*"]
allowed_targets = ["planner"]
{hooks}"#
    )
}

/// The same spec with no `[[client]]`: a case that drives a session registers
/// the role it needs, and `register_role` appends that entry — a role declared
/// twice is refused, and a fixture's own declaration would shadow the one the
/// client was initialised from.
fn spec_without_roles(hooks: &str) -> String {
    format!(
        r#"[server]
name = "hooks"
listen = "127.0.0.1:0"
{hooks}"#
    )
}

/// One `[[hook]]` entry, with the command's argv spelled out.
fn hook_entry(on: &[&str], run: &[&str], timeout: &str) -> String {
    let on = on
        .iter()
        .map(|class| format!("\"{class}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let run = run
        .iter()
        .map(|arg| format!("\"{arg}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[[hook]]\non = [{on}]\nrun = [{run}]\ntimeout = \"{timeout}\"\n")
}

/// Write a shell script and answer its path. The directory must outlive every
/// cluster that runs it.
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).expect("write the hook script");
    let mut permissions = fs::metadata(&path).expect("stat the script").permissions();
    use std::os::unix::fs::PermissionsExt as _;
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make the script executable");
    path
}

/// Read a file a hook wrote, waiting until it appears.
fn wait_for(path: &Path, what: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            if !text.is_empty() {
                return text;
            }
        }
        if Instant::now() > deadline {
            panic!("{what} never appeared at {}", path.display());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wait until `predicate` holds, or fail naming what was being waited for.
fn wait_until(mut predicate: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

/// A file a hook appends to, or "" when it does not exist yet.
fn log_of(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// Run one reload and answer the `seq` it appended, read back from the stream.
async fn reload_seq(cluster: &Cluster) -> u64 {
    let before = history(cluster).await.len();
    cluster.reload().await.expect("reload");
    let deadline = Instant::now() + WAIT;
    loop {
        let rows = history(cluster).await;
        if rows.len() > before {
            return rows
                .iter()
                .rev()
                .find(|row| matches!(row.event, Event::SpecReloaded(_)))
                .map(|row| row.seq)
                .expect("the reload appended a spec_reloaded event");
        }
        if Instant::now() > deadline {
            panic!("the reload appended no event");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The cluster's whole event stream, oldest first.
async fn history(cluster: &Cluster) -> Vec<EventRow> {
    cluster.history(0, None).await.expect("read history")
}

/// The events of one class, in `seq` order.
fn of_class<'a>(rows: &'a [EventRow], class: &str) -> Vec<&'a EventRow> {
    rows.iter()
        .filter(|row| row.event.type_name() == class)
        .collect()
}

/// The `hook_failed` faults recorded for one hook, oldest first.
fn hook_faults(rows: &[EventRow], needle: &str) -> Vec<(u64, String)> {
    rows.iter()
        .filter_map(|row| match &row.event {
            Event::Fault(fault) if fault.kind == "hook_failed" => {
                Some((row.seq, fault.reason.clone()))
            }
            _ => None,
        })
        .filter(|(_, reason)| reason.contains(needle))
        .collect()
}

/// A hook reads its event on stdin, is handed the admin socket in
/// `ONLYNE_SOCKET`, and reaches it with nothing else naming the server.
///
/// The script is given the `onlyne` binary as the second argument and asked to
/// dial the socket the environment names; the answer it writes back is the
/// server's own `status` payload, which no other path can produce. A runner
/// that wrote a different path, or none, fails here.
#[tokio::test]
async fn a_hook_reads_its_event_on_stdin_and_reaches_the_admin_socket() {
    let dir = tempdir().expect("tempdir");
    let out = dir.path().join("one");
    let script = write_script(
        dir.path(),
        "one.sh",
        r#"#!/bin/sh
# $1 = where to leave what happened, $2 = the onlyne binary.
out=$1
cli=$2
cat > "$out.event"
printf '%s' "${ONLYNE_SOCKET-}" > "$out.socket"
"$cli" --as admin status > "$out.status" 2> "$out.status.err"
printf '%s' "$?" > "$out.code"
"$cli" --as admin loop --nonsense > /dev/null 2>&1
exit 0
"#,
    );
    let spec = spec_with_hooks(&hook_entry(
        &["spec_reloaded"],
        &[
            "/bin/sh",
            script.to_str().expect("script path"),
            out.to_str().expect("out path"),
            Cluster::bin_path("onlyne")
                .expect("onlyne binary")
                .to_str()
                .expect("cli path"),
        ],
        "10s",
    ));
    let cluster = Cluster::start(&spec).await.expect("cluster start");

    cluster.reload().await.expect("reload");

    let code = wait_for(&out.with_extension("code"), "the hook's exit code");
    assert_eq!(code, "0", "the script must exit zero");
    let event: Value =
        serde_json::from_str(&fs::read_to_string(out.with_extension("event")).expect("event file"))
            .expect("the event on stdin is JSON");
    assert_eq!(event["type"], "spec_reloaded", "{event}");
    assert!(event["seq"].as_u64().unwrap_or(0) >= 1, "{event}");
    assert!(
        event["created_at"]
            .as_str()
            .is_some_and(|stamp| !stamp.is_empty()),
        "the record's own timestamp rides along: {event}"
    );
    assert!(
        event["data"]["spec_hash"].as_str().unwrap_or("").len() > 8,
        "the payload the server persisted rides along: {event}"
    );

    let handed = fs::read_to_string(out.with_extension("socket")).expect("socket file");
    assert_eq!(
        handed.trim(),
        cluster
            .admin_socket()
            .expect("admin socket")
            .to_string_lossy(),
        "ONLYNE_SOCKET must name the admin socket"
    );

    let status_text = fs::read_to_string(out.with_extension("status")).expect("status");
    let status: Value = serde_json::from_str(&status_text).unwrap_or_else(|error| {
        panic!(
            "the hook's status is not JSON ({error}): {status_text} / {}",
            fs::read_to_string(out.with_extension("status.err")).unwrap_or_default()
        )
    });
    assert_eq!(status["ok"], Value::Bool(true), "{status}");
    assert_eq!(
        status["data"]["cluster"], "hooks",
        "the hook reached the admin socket through ONLYNE_SOCKET alone: {status}"
    );
}

/// A hook never runs for a class it does not name.
///
/// The stream carries `spec_reloaded` events and the hook names
/// `delivery_blocked`, so no process may ever start. The marker file is the
/// evidence: a runner that spawned the script for the wrong class would leave
/// one.
#[tokio::test]
async fn a_hook_runs_only_for_the_classes_it_names() {
    let dir = tempdir().expect("tempdir");
    let marker = dir.path().join("ran");
    let script = write_script(
        dir.path(),
        "never.sh",
        r#"#!/bin/sh
printf 'ran\n' >> "$1"
exit 0
"#,
    );
    let spec = spec_with_hooks(&hook_entry(
        &["delivery_blocked"],
        &[
            "/bin/sh",
            script.to_str().expect("script path"),
            marker.to_str().expect("marker path"),
        ],
        "10s",
    ));
    let cluster = Cluster::start(&spec).await.expect("cluster start");

    reload_seq(&cluster).await;
    reload_seq(&cluster).await;
    // The events are on the stream and the hook has had time to look at them.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        of_class(&history(&cluster).await, "spec_reloaded").len() >= 2,
        "the events a hook did not name are still on the stream"
    );
    assert_eq!(
        log_of(&marker),
        "",
        "a hook bound to delivery_blocked must not run for a spec_reloaded event"
    );
}

/// A nonzero exit records one `hook_failed` fault, leaves the original event
/// untouched, and does not stop a second hook bound to the same event.
#[tokio::test]
async fn a_failing_hook_is_faulted_once_and_the_other_hook_still_runs() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log");
    let failing = write_script(
        dir.path(),
        "failing.sh",
        r#"#!/bin/sh
event=$(cat)
printf 'failing %s\n' "$event" >> "$1"
exit 3
"#,
    );
    let working = write_script(
        dir.path(),
        "working.sh",
        r#"#!/bin/sh
event=$(cat)
printf 'working %s\n' "$event" >> "$1"
exit 0
"#,
    );
    let spec = spec_with_hooks(&format!(
        "{}{}",
        hook_entry(
            &["spec_reloaded"],
            &[
                "/bin/sh",
                failing.to_str().expect("script path"),
                log.to_str().expect("log path"),
            ],
            "10s",
        ),
        hook_entry(
            &["spec_reloaded"],
            &[
                "/bin/sh",
                working.to_str().expect("script path"),
                log.to_str().expect("log path"),
            ],
            "10s",
        ),
    ));
    let cluster = Cluster::start(&spec).await.expect("cluster start");

    let seq = reload_seq(&cluster).await;
    wait_until(
        || log_of(&log).contains("working"),
        "the second hook to run despite the first one failing",
    );
    let text = log_of(&log);
    assert!(text.contains("failing"), "{text}");
    assert!(
        text.contains("\"seq\":"),
        "the failing hook read its event too: {text}"
    );

    let rows = history(&cluster).await;
    let faults = hook_faults(&rows, "failing.sh");
    assert_eq!(
        faults.len(),
        1,
        "one fault per failed event, whichever way the hook keeps failing: {faults:?}"
    );
    assert!(
        faults[0].1.contains(&format!("seq {seq}")),
        "the fault names the event: {:?}",
        faults[0]
    );
    let reloads = of_class(&rows, "spec_reloaded");
    assert_eq!(reloads.len(), 1, "the original event stands: {reloads:?}");
    assert_eq!(
        reloads[0].event.type_name(),
        "spec_reloaded",
        "a hook failure does not rewrite the event it could not handle"
    );
}

/// A hook that sleeps far past its bound delays nothing on the stream, and is
/// faulted once.
///
/// The first event wedges the script for far longer than its one-second bound.
/// While it is provably asleep — the marker it writes before sleeping names its
/// own `seq` — the next reload must still complete in well under the bound, and
/// the event it appended must be on the stream beside the first. The fault for
/// the first event is counted after the retries have had time to fail again.
#[tokio::test]
async fn a_wedged_hook_delays_nothing_behind_it_and_is_faulted_once() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log");
    let script = write_script(
        dir.path(),
        "sleepy.sh",
        r#"#!/bin/sh
event=$(cat)
printf '%s\n' "$event" >> "$1"
sleep 30
exit 0
"#,
    );
    let spec = spec_with_hooks(&hook_entry(
        &["spec_reloaded"],
        &[
            "/bin/sh",
            script.to_str().expect("script path"),
            log.to_str().expect("log path"),
        ],
        "1s",
    ));
    let cluster = Cluster::start(&spec).await.expect("cluster start");

    let first = reload_seq(&cluster).await;
    wait_until(
        || log_of(&log).contains(&format!("\"seq\":{first}")),
        "the hook script to start on the first event",
    );

    let started = Instant::now();
    let second = reload_seq(&cluster).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "a hook wedged 30 seconds must not delay the event behind it: {elapsed:?}"
    );
    assert!(second > first, "{second} must follow {first}");
    assert!(
        of_class(&history(&cluster).await, "spec_reloaded").len() >= 2,
        "both events are on the stream while the script is still asleep"
    );

    // The bound is one second, so by now the first event has failed at least
    // once — and the retries that followed are the same fact, not new ones.
    std::thread::sleep(Duration::from_secs(3));
    let rows = history(&cluster).await;
    let faults = hook_faults(&rows, "sleepy.sh");
    assert_eq!(
        faults.len(),
        1,
        "a wedged hook is recorded once, however many attempts it takes: {faults:?}"
    );
    assert!(
        faults[0].1.contains("timeout after"),
        "the reason names the bound that elapsed: {:?}",
        faults[0].1
    );
    // The event behind the wedge is owed, not served: the cursor is a
    // contiguous prefix of handled events, so a script that keeps failing
    // keeps everything after its failure owed — the fault above is how the
    // operator hears about it. What is *not* delayed is the stream: both
    // events were published while the script was still asleep.
    let text = log_of(&log);
    assert!(
        text.matches(&format!("\"seq\":{first}")).count() >= 2,
        "the event that failed is offered again, not stepped over: {text}"
    );
    assert!(
        !text.contains(&format!("\"seq\":{second}")),
        "nothing is served from behind a failure the cursor cannot pass: {text}"
    );
}

/// After a restart the runner resumes from the last `seq` it handled: no event
/// skipped, none handled twice.
///
/// The backlog is half processed when the server dies — `S1` handled, `S2`
/// failed and owed, `S3` published and owed — and the cursor is what makes the
/// rest of it reachable. The failing hook exits immediately (no orphan sleeps
/// through the restart), so the log's own order is the whole story: `S1` once,
/// then `S2` and `S3` once each, with nothing repeated.
///
/// The script's rule is the phase one backlog: the first event it is handed
/// (`S1`) is handled, and every attempt after that exits `7` until a release
/// file appears. So `S1` is the handled prefix, `S2` is the failed event the
/// cursor stops under, and `S3` is never offered while that failure stands.
#[tokio::test]
async fn a_restart_resumes_from_the_last_successful_seq() {
    let dir = tempdir().expect("tempdir");
    let log = dir.path().join("log");
    let release = dir.path().join("release");
    let script = write_script(
        dir.path(),
        "resume.sh",
        r#"#!/bin/sh
# $1 = log, $2 = release file. The first event the hook is handed is handled;
# everything after it exits 7 until the release file exists.
event=$(cat)
seq=$(printf '%s' "$event" | sed -n 's/.*"seq":\([0-9]*\).*/\1/p')
printf 'start %s\n' "$seq" >> "$1"
if [ -f "$2" ]; then
  printf 'done %s\n' "$seq" >> "$1"
  exit 0
fi
if [ ! -f "$1.started" ]; then
  : > "$1.started"
  printf 'done %s\n' "$seq" >> "$1"
  exit 0
fi
printf 'owed %s\n' "$seq" >> "$1"
exit 7
"#,
    );
    let spec = spec_with_hooks(&hook_entry(
        &["spec_reloaded"],
        &[
            "/bin/sh",
            script.to_str().expect("script path"),
            log.to_str().expect("log path"),
            release.to_str().expect("release path"),
        ],
        "10s",
    ));
    let cluster = Cluster::start(&spec).await.expect("cluster start");

    // Phase one: S1 is handled, S2 is owed, and S3 is published behind it.
    let first = reload_seq(&cluster).await;
    let second = reload_seq(&cluster).await;
    let third = reload_seq(&cluster).await;
    assert!(first < second && second < third, "{first} {second} {third}");
    wait_until(
        || log_of(&log).contains("start 2"),
        "the hook to reach the second event",
    );
    std::thread::sleep(Duration::from_millis(500));
    let before = log_of(&log);
    assert!(before.contains("done 1"), "{before}");
    assert!(
        !before.contains("start 3"),
        "an event behind a failed one is owed, not skipped into: {before}"
    );
    assert!(before.contains("owed 2"), "{before}");

    // Phase two: the same cluster in a new process resumes where it left off.
    fs::write(&release, b"go").expect("write the release file");
    cluster.restart_server().await.expect("restart");

    wait_until(
        || {
            let text = log_of(&log);
            text.contains("done 2") && text.contains("done 3")
        },
        "the restarted runner to work through the backlog it owed",
    );
    let after = log_of(&log);
    let count = |needle: &str| after.matches(needle).count();
    assert_eq!(
        count("done 1"),
        1,
        "a handled event is not handled twice: {after}"
    );
    assert_eq!(count("done 2"), 1, "{after}");
    assert_eq!(count("done 3"), 1, "{after}");
    assert_eq!(
        count("start 1"),
        1,
        "the restart must not walk back past the cursor: {after}"
    );
    // Every event the stream carried reached the hook exactly once.
    let rows = history(&cluster).await;
    let published = of_class(&rows, "spec_reloaded")
        .iter()
        .map(|row| row.seq)
        .collect::<Vec<_>>();
    for seq in &published[..3] {
        assert!(
            after.contains(&format!("done {seq}")),
            "every published event is handled, none skipped: {seq} missing from {after}"
        );
    }
}

/// A client may publish the settlement classes to the server's stream, and
/// nothing else.
///
/// The positive half is the whole reason the verb exists: the server appends
/// what the client witnessed and answers nothing. The negative half is the
/// closed set: a class the client made up is refused by name, so a hook cannot
/// be bound to a spelling nobody publishes.
#[tokio::test]
async fn a_client_publishes_only_the_settlement_classes() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("server");
    fs::create_dir_all(root.join(".onlyne")).expect("create the root");
    // `Server::open` is driven directly here, so this case writes its own
    // `cert_pin`: `Cluster::start` is what harvests one from the root.
    fs::write(
        root.join(".onlyne/spec.toml"),
        r#"[server]
name = "publish"
listen = "127.0.0.1:0"
cert_pin = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="

[[client]]
role = "planner"
key = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
allowed_senders = ["*"]
allowed_targets = ["planner"]
"#,
    )
    .expect("write the spec");
    let state = Server::open(&ServerInit {
        root: root.clone(),
        listen: None,
    })
    .expect("open the server");
    let mut session = Session {
        role: Some("planner".to_string()),
        authorised: Some("planner".to_string()),
        authenticated: true,
        ..Session::default()
    };

    let body = router::dispatch_client(
        &state,
        &mut session,
        ClientOp::PublishEvent(PublishEventArgs {
            class: "delivery_blocked".to_string(),
            payload: json!({"task_id": "t-1", "role": "planner"}),
        }),
    )
    .await;
    assert!(body.ok, "{body:?}");
    assert_eq!(
        body.data,
        Some(Value::Null),
        "the server answers nothing for a published event"
    );
    let rows = state.ledger.events_since(0, 16).expect("read the stream");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].kind, "delivery_blocked");
    // The ledger keeps the event as the tagged enum the wire carries, which is
    // what the history op decodes back into an `Event`; the class is the tag.
    assert_eq!(rows[0].data["type"], "delivery_blocked");
    assert_eq!(rows[0].data["data"]["task_id"], "t-1");

    let body = router::dispatch_client(
        &state,
        &mut session,
        ClientOp::PublishEvent(PublishEventArgs {
            class: "made_up_class".to_string(),
            payload: json!({}),
        }),
    )
    .await;
    assert!(!body.ok, "{body:?}");
    let error = body.error.expect("a refusal carries its reason");
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.field.as_deref(), Some("class"));
    assert!(
        error.message.contains("made_up_class"),
        "the refusal names the class: {}",
        error.message
    );
    assert_eq!(
        state
            .ledger
            .events_since(0, 16)
            .expect("read the stream")
            .len(),
        1,
        "a refused class appends nothing"
    );
}

/// A real session that settles `blocked` runs the hook bound to it, exactly
/// once, and runs no hook bound to a class the session never triggered.
///
/// This is the case `ClientOp::PublishEvent` exists for: the class belongs to
/// the client — no other publisher can invent a turn that ended a certain way —
/// so a hook bound to it sees the event only if the client's word reaches the
/// server's stream. The script is the plan's own example, and the session is a
/// real one: a client daemon, a fake runtime on a real adapter socket, and two
/// turns that end without a completion over a delivery that never completed.
///
/// The second hook is bound to `handoff`, which this delivery never reaches:
/// one session's endings therefore select by class, and the file the bound
/// hook appends to holds one line rather than one per pass.
#[tokio::test]
async fn a_session_that_settles_blocked_runs_the_hook_bound_to_it() {
    let dir = tempdir().expect("tempdir");
    let out = dir.path().join("blocked");
    let quiet = dir.path().join("handoff-ran");
    let script = write_script(
        dir.path(),
        "blocked.sh",
        r#"#!/bin/sh
printf '%s\n' "$(cat)" >> "$1"
exit 0
"#,
    );
    let never = write_script(
        dir.path(),
        "handoff.sh",
        r#"#!/bin/sh
cat > /dev/null
printf 'ran\n' >> "$1"
exit 0
"#,
    );
    let spec = spec_without_roles(&format!(
        "{}{}",
        hook_entry(
            &["delivery_blocked"],
            &[
                "/bin/sh",
                script.to_str().expect("script path"),
                out.to_str().expect("out path"),
            ],
            "10s",
        ),
        hook_entry(
            &["handoff"],
            &[
                "/bin/sh",
                never.to_str().expect("script path"),
                quiet.to_str().expect("marker path"),
            ],
            "10s",
        ),
    ));
    let cluster = Cluster::start(&spec).await.expect("cluster start");
    let workspace = cluster
        .register_role(
            "planner",
            "planner prose",
            Some(
                r#"allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]"#,
            ),
        )
        .await
        .expect("register the role");
    cluster
        .start_client(&workspace)
        .await
        .expect("start the client");
    cluster
        .wait_role_online("planner")
        .await
        .expect("the role links");
    // The drive declares `inject`, which is what a plugin needs for the host to
    // hand it the task as an `assign` frame — the other door is the plugin's own
    // stdin, and a script waiting for an assignment never sees one. So the first
    // turn that ends without a completion spends the delivery's one nudge and
    // the second settles it blocked, which is the ending the hook is bound to.
    let agent = onlyne_testkit::AgentScript {
        hello: onlyne_testkit::ScriptHello {
            capabilities: vec![
                onlyne_proto::Capability::Register,
                onlyne_proto::Capability::Report,
                onlyne_proto::Capability::Inject,
            ],
        },
        steps: vec![
            json!({"wait_assign": true}),
            json!({"report": "ready"}),
            json!({"report": "heartbeat"}),
            json!({"report": "idle"}),
            json!({"report": "heartbeat"}),
            json!({"report": "idle"}),
        ],
        repeat: false,
    };
    cluster
        .start_fake_agent(&workspace, &agent)
        .await
        .expect("start the fake agent");

    let sent = cluster
        .admin_send("planner", "planner", "one turn that never completes")
        .await
        .expect("admin send");
    let task = sent["task"]
        .as_str()
        .or_else(|| sent["receipt"]["task"].as_str())
        .expect("the send names its task")
        .to_string();

    let text = wait_for(&out, "the hook bound to delivery_blocked");
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "one blocked delivery runs the bound hook once: {text}"
    );
    let event: Value = serde_json::from_str(lines[0]).expect("the event on stdin is JSON");
    assert_eq!(
        event["type"], "delivery_blocked",
        "the hook is handed the settlement's own class: {event}"
    );
    assert_eq!(event["data"]["task_id"], task.as_str(), "{event}");
    assert_eq!(event["data"]["role"], "planner", "{event}");
    assert!(event["seq"].as_u64().unwrap_or(0) >= 1, "{event}");

    // The server's stream carries the same fact, which is where the client sent
    // it: no local copy is the authority.
    let rows = history(&cluster).await;
    let blocked = of_class(&rows, "delivery_blocked");
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert_eq!(blocked[0].event.type_name(), "delivery_blocked");
    assert_eq!(
        blocked[0].seq,
        event["seq"].as_u64().unwrap(),
        "the hook's `seq` is the server's own"
    );
    let sessions = cluster
        .query_sessions(QuerySessionsArgs::default())
        .await
        .expect("query sessions");
    assert!(
        sessions
            .iter()
            .any(|row| row.task_id.as_deref() == Some(task.as_str())),
        "{sessions:?}"
    );

    // The sibling class of the same publisher is on the stream, and it is not
    // the one the second hook names: this delivery hands nothing on, so that
    // hook may never run — and the marker proves none did.
    assert!(
        !of_class(&rows, "turn_end_without_complete").is_empty(),
        "the session's own endings are on the same stream: {rows:?}"
    );
    assert!(
        of_class(&rows, "handoff").is_empty(),
        "this delivery never hands off: {:?}",
        of_class(&rows, "handoff")
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        log_of(&quiet),
        "",
        "a hook bound to handoff must not run for a blocked settlement"
    );
    let settled_text = log_of(&out);
    let settled: Vec<&str> = settled_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        settled.len(),
        1,
        "the bound hook ran once, not once per pass: {settled:?}"
    );
}
