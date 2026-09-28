//! Liveness in memory, and readers that do not queue behind the writer.
//!
//! Slice 5's acceptance, one case per line it promises:
//!
//! - a session that beats N times without a content change performs **zero**
//!   session-row writes — counted by the store (`ServerLedger::session_row_writes`)
//!   rather than read off the code — while `onlyne sessions` still shows a
//!   `last_seen` that advances between two reads;
//! - an admin read answers while a delivery write is in flight, without taking
//!   the writer's connection;
//! - after a stop and a restart, the persisted `last_seen` is no older than the
//!   interval the server states.
//!
//! The interval is the cluster's presence window, `[server]
//! heartbeat_timeout_ms` (`projection::last_seen_flush_secs`), so a case sets
//! that window to what it means to observe: a minute, past which no beat can
//! outrun the row inside a case, or one second, which the real clock reaches.

use chrono::Utc;
use onlyne_net::KeyPair;
use onlyne_proto::{
    Body, Causality, Lifecycle, MsgKind, Outcome, Principal, QuerySessionsArgs, Report, SessionRow,
};
use onlyne_server::projection;
use onlyne_server::relay::{self, RelayReply};
use onlyne_server::state::{Server, ServerInit};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::{TempDir, tempdir};

const CERT_PIN: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn spec_text(heartbeat_timeout_ms: u64) -> String {
    format!(
        r#"[server]
name = "local"
listen = "127.0.0.1:0"
cert_pin = "{CERT_PIN}"
heartbeat_timeout_ms = {heartbeat_timeout_ms}

[[client]]
role = "supervisor"
key = "{key}"
allowed_senders = []
allowed_targets = ["builder"]

[[client]]
role = "builder"
key = "{key}"
allowed_senders = ["supervisor"]
allowed_targets = ["supervisor"]
"#,
        key = KeyPair::from_seed([7_u8; 32]).public_str()
    )
}

struct Fixture {
    _dir: TempDir,
    state: Arc<onlyne_server::State>,
}

/// One server root whose presence window — and so whose staleness interval —
/// is the caller's.
fn open_server(heartbeat_timeout_ms: u64) -> Fixture {
    let dir = tempdir().expect("temp dir");
    let root = dir.path().join("server");
    std::fs::create_dir_all(root.join(".onlyne")).expect("create the root");
    std::fs::write(
        root.join(".onlyne/spec.toml"),
        spec_text(heartbeat_timeout_ms),
    )
    .expect("write the spec");
    let state = Server::open(&ServerInit {
        root: root.clone(),
        listen: None,
    })
    .expect("open the server");
    Fixture { _dir: dir, state }
}

/// One session row, opened the way a session's first turn opens one.
fn open_session(state: &Arc<onlyne_server::State>, task_id: &str, session_id: &str) {
    projection::report(
        state,
        "builder",
        &Report::Ready {
            task_id: task_id.to_string(),
            session_id: session_id.to_string(),
            generation: 1,
            seq: 1,
            cluster_ref: None,
        },
    )
    .expect("the ready report lands");
}

/// A beat that observed nothing new, shaped as the client shapes it.
///
/// This is the client's own sync frame: the tuple the row already holds, and
/// the projection that row already carries, republished because the beat is due
/// rather than because anything changed. It is the frame that took the
/// projection write, the event, and the broadcast in v1.
fn no_op_beat(state: &Arc<onlyne_server::State>, task_id: &str) -> Report {
    let row = state
        .ledger
        .session_row_for_task(task_id)
        .expect("the row reads")
        .expect("the row is present");
    Report::Heartbeat {
        task_id: task_id.to_string(),
        session_id: row.session_id.clone(),
        generation: row.generation.max(0) as u64,
        seq: row.seq.max(0) as u64,
        observed: serde_json::Value::Null,
        projection: Some(projection::projection_from_write(&row)),
        cluster_ref: None,
    }
}

/// The `last_seen` the file holds, read around the server.
///
/// The live value is in the process; this is what a restart, and anything else
/// reading the file rather than the cluster, is left with.
fn persisted_last_seen(path: &Path, session_id: &str) -> i64 {
    let conn = rusqlite::Connection::open(path).expect("open the state file");
    let text: String = conn
        .query_row(
            "SELECT last_seen FROM sessions WHERE session_id=?",
            [session_id],
            |row| row.get(0),
        )
        .expect("the row is on disk");
    chrono::DateTime::parse_from_rfc3339(&text)
        .expect("a stored stamp")
        .timestamp()
}

/// The admin surface's read of one task's row — what `onlyne sessions`, the
/// board, and the TUI take.
async fn admin_sessions(state: &Arc<onlyne_server::State>, task_id: &str) -> SessionRow {
    let mut rows = projection::sessions_read(
        state,
        QuerySessionsArgs {
            task_id: Some(task_id.to_string()),
            limit: 10,
            ..QuerySessionsArgs::default()
        },
        true,
    )
    .await
    .expect("the read answers");
    assert_eq!(rows.len(), 1, "one task, one row");
    rows.remove(0)
}

fn stamp(row: &SessionRow) -> i64 {
    row.last_seen
        .as_deref()
        .expect("a row a reader is handed carries `last_seen`")
        .parse()
        .expect("unix seconds")
}

/// A session that beats without changing anything performs zero session-row
/// writes, and the row a reader is handed still moves.
///
/// The window is a minute, so no interval can pass inside the case: the beats
/// below are the only thing that could write. The count is the store's own, and
/// the file is read beside the answer to show the row a reader is handed is not
/// the row that is frozen on disk.
#[tokio::test]
async fn a_beat_that_changed_nothing_writes_no_session_row() {
    let fixture = open_server(60_000);
    let state = &fixture.state;
    let task_id = onlyne_proto::new_task_id();
    open_session(state, &task_id, "sess-live");
    let path = state.ledger.path().to_path_buf();

    let writes_before = state.ledger.session_row_writes();
    let events_before = state.event_head();
    let stored_before = persisted_last_seen(&path, "sess-live");

    for _ in 0..8 {
        let outcome =
            projection::report(state, "builder", &no_op_beat(state, &task_id)).expect("the beat");
        assert!(
            !outcome.applied,
            "a beat that observed nothing new is not a projection write"
        );
    }
    assert_eq!(
        state.ledger.session_row_writes(),
        writes_before,
        "eight beats without a content change wrote no session row"
    );
    assert_eq!(state.event_head(), events_before, "and published no event");

    // `last_seen` is whole unix seconds, so what a reader can see move is the
    // second it names. The beats go on the whole time they are read: they are
    // the only thing that could move it, and it moves with no write beside it.
    let first_seen = stamp(&admin_sessions(state, &task_id).await);
    let mut moved = None;
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        projection::report(state, "builder", &no_op_beat(state, &task_id)).expect("the beat");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let row = admin_sessions(state, &task_id).await;
        if stamp(&row) > first_seen {
            moved = Some(row);
            break;
        }
    }
    let row = moved.expect("`last_seen` advances between two reads while the beats go on");

    assert_eq!(
        state.ledger.session_row_writes(),
        writes_before,
        "and none of those beats wrote a row either"
    );
    assert_eq!(
        persisted_last_seen(&path, "sess-live"),
        stored_before,
        "the file still holds what the last content write left"
    );
    assert!(
        stamp(&row) > stored_before,
        "the reader is handed the live value, not the frozen row: {} vs {}",
        stamp(&row),
        stored_before
    );
}

/// The row is refreshed at the stated interval and not before, and a reader is
/// handed the newer of the two values either way.
///
/// The beats here carry their own clock — `ServerLedger::beat_session` takes the
/// beat's second — so the boundary is exact rather than raced. The server path
/// above is the same call with `Utc::now()` for its clock and its presence
/// window for the interval.
#[test]
fn the_row_is_refreshed_at_the_stated_interval_and_not_before() {
    let fixture = open_server(1_000);
    let state = &fixture.state;
    let task_id = onlyne_proto::new_task_id();
    open_session(state, &task_id, "sess-interval");
    let path = state.ledger.path().to_path_buf();
    let persisted = persisted_last_seen(&path, "sess-interval");
    let writes_before = state.ledger.session_row_writes();

    // Inside the interval: the beat is memory, and the row keeps its value.
    assert!(
        !state
            .ledger
            .beat_session("sess-interval", persisted, 1)
            .expect("the beat lands"),
        "a beat inside the interval does not reach the table"
    );
    assert_eq!(persisted_last_seen(&path, "sess-interval"), persisted);
    assert_eq!(state.ledger.session_row_writes(), writes_before);

    // At the interval: one write, and the row now holds the beat.
    let at = persisted + 1;
    assert!(
        state
            .ledger
            .beat_session("sess-interval", at, 1)
            .expect("the beat lands")
    );
    assert_eq!(
        persisted_last_seen(&path, "sess-interval"),
        at,
        "the flushed row holds the beat's own second"
    );
    assert_eq!(
        state.ledger.session_row_writes(),
        writes_before + 1,
        "one write for the interval, not one per beat"
    );

    // A wider interval, with beats whose clock runs ahead of the row: the row
    // stays where the last flush left it and the reader is handed the beat.
    let ahead = at + 30;
    assert!(
        !state
            .ledger
            .beat_session("sess-interval", ahead, 60)
            .expect("the beat lands")
    );
    assert_eq!(persisted_last_seen(&path, "sess-interval"), at);
    assert_eq!(
        state
            .ledger
            .get_session_row("sess-interval")
            .expect("the row reads")
            .expect("the row is present")
            .last_seen,
        ahead,
        "the reader is handed the beat while the file still holds the last flush"
    );

    // And a beat the row already caught up with is spent: the value a reader
    // gets is the row's, not a remembered one.
    assert!(
        !state
            .ledger
            .beat_session("sess-interval", at, 60)
            .expect("the beat lands")
    );
    assert_eq!(
        state
            .ledger
            .get_session_row("sess-interval")
            .expect("the row reads")
            .expect("the row is present")
            .last_seen,
        ahead,
        "an older beat never moves the answer backwards"
    );
}

/// A content change and an ending each reach the row, with one write apiece.
///
/// The two other moments the row is refreshed for: a publish whose tuple moved,
/// and the report that closes the session. Neither is a beat, and both are
/// visible on disk.
#[tokio::test]
async fn a_content_change_and_an_ending_each_refresh_the_row() {
    let fixture = open_server(60_000);
    let state = &fixture.state;
    let task_id = onlyne_proto::new_task_id();
    open_session(state, &task_id, "sess-content");
    let path = state.ledger.path().to_path_buf();
    let writes_before = state.ledger.session_row_writes();

    let row = state
        .ledger
        .session_row_for_task(&task_id)
        .expect("the row reads")
        .expect("the row is present");
    let mut moved = projection::projection_from_write(&row);
    moved.observed = Some(serde_json::json!({ "step": 2 }));

    let at = Utc::now().timestamp();
    let outcome = projection::report(
        state,
        "builder",
        &Report::Heartbeat {
            task_id: task_id.clone(),
            session_id: row.session_id.clone(),
            generation: row.generation.max(0) as u64,
            seq: row.seq.max(0) as u64 + 1,
            observed: serde_json::Value::Null,
            projection: Some(moved),
            cluster_ref: None,
        },
    )
    .expect("the publish lands");
    assert!(outcome.applied, "a publish that moved the tuple applies");
    assert_eq!(
        state.ledger.session_row_writes(),
        writes_before + 1,
        "one content write, one row"
    );
    assert!(
        persisted_last_seen(&path, "sess-content") >= at,
        "and the row carries the time of the write that changed it"
    );

    projection::report(
        state,
        "builder",
        &Report::Complete {
            task_id: task_id.clone(),
            outcome: Outcome::Done,
            head: Some("finished".to_string()),
            details: None,
            files: Vec::new(),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .expect("the ending lands");
    assert_eq!(
        state.ledger.session_row_writes(),
        writes_before + 2,
        "the ending is a write of its own"
    );
    assert!(
        persisted_last_seen(&path, "sess-content") >= at,
        "and it leaves the row fresh"
    );
    let ended = admin_sessions(state, &task_id).await;
    assert_eq!(ended.public_lifecycle, Lifecycle::Exited);
    assert_eq!(
        stamp(&ended),
        persisted_last_seen(&path, "sess-content"),
        "a session that ended has no live beat left to outrun its row"
    );
}

/// An admin read answers while a delivery write is in flight.
///
/// The write is held in flight by an outside connection holding SQLite's write
/// lock, so the delivery write is genuinely blocked inside the store with the
/// writer's connection checked out. Two things are read off the answer, and
/// neither is read off the code: the delivery write is still in flight when the
/// read has returned, and the read was quick. The second is what says the read
/// did not take the writer's connection — on that connection it cannot answer
/// before the connection is free, which is the store's five-second lock wait,
/// measured at 5.06s with this read pointed back at the writer. A board that
/// takes five seconds to refresh is the queue this slice removes.
#[tokio::test]
async fn an_admin_read_answers_while_a_delivery_write_is_in_flight() {
    let fixture = open_server(60_000);
    let state = &fixture.state;
    let task_id = onlyne_proto::new_task_id();
    open_session(state, &task_id, "sess-reader");

    let envelope = onlyne_proto::new_envelope(
        MsgKind::Task,
        Principal::role("supervisor"),
        Principal::role("builder"),
        Body::text("deliver this"),
        Some(Causality::root(task_id.clone())),
    )
    .expect("a valid task");
    let msg_id = match relay::send(state, &envelope, true, Some("builder")).expect("the send runs")
    {
        RelayReply::Accepted(outcome) => outcome.receipt.msg_id,
        other => panic!("the dispatch was refused: {other:?}"),
    };

    // The outside connection takes the file's write lock and holds it: every
    // write through the store now waits inside SQLite, with the writer's lock
    // held for as long as that wait lasts.
    let holder = rusqlite::Connection::open(state.ledger.path()).expect("open the state file");
    holder
        .execute_batch("BEGIN IMMEDIATE")
        .expect("take the write lock");

    let holder_state = state.clone();
    let held_msg = msg_id.clone();
    let write = tokio::task::spawn_blocking(move || holder_state.ledger.mark_in_flight(&held_msg));
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !write.is_finished(),
        "the delivery write is in flight, waiting on the file lock"
    );

    let started = Instant::now();
    let read = projection::sessions_read(
        state,
        QuerySessionsArgs {
            task_id: Some(task_id.clone()),
            limit: 10,
            ..QuerySessionsArgs::default()
        },
        true,
    )
    .await
    .expect("the read answers");
    let took = started.elapsed();
    assert_eq!(read.len(), 1, "the board still sees the session");
    assert_eq!(read[0].session_id, "sess-reader");
    assert!(
        !write.is_finished(),
        "the read did not take the writer's connection: the write is still where it was"
    );
    assert!(
        took < Duration::from_secs(1),
        "nor wait for the writer's lock to come free: the read took {took:?}"
    );

    holder.execute_batch("COMMIT").expect("release the lock");
    assert!(
        write
            .await
            .expect("the write task joins")
            .expect("the write lands"),
        "the delivery write completes once the lock is free"
    );
}

/// After a stop and a restart, the persisted `last_seen` is no older than the
/// stated interval.
///
/// The window is one second here, so the real clock reaches the interval inside
/// the case: the beats go on while the file is read at every one of them, and
/// the promise checked is the reader's — what a restart is left with is never
/// more than one interval behind the beat that just landed. The restarted
/// process holds no beats at all, so what it answers is the file and nothing
/// else.
#[test]
fn a_persisted_last_seen_survives_a_restart_within_the_stated_interval() {
    let dir = tempdir().expect("temp dir");
    let root = dir.path().join("server");
    std::fs::create_dir_all(root.join(".onlyne")).expect("create the root");
    std::fs::write(root.join(".onlyne/spec.toml"), spec_text(1_000)).expect("write the spec");
    let state = Server::open(&ServerInit {
        root: root.clone(),
        listen: None,
    })
    .expect("open the server");
    let task_id = onlyne_proto::new_task_id();
    open_session(&state, &task_id, "sess-restart");
    let path = state.ledger.path().to_path_buf();
    let opened_at = persisted_last_seen(&path, "sess-restart");

    let deadline = Instant::now() + Duration::from_secs(4);
    let mut flushes = 0;
    while Instant::now() < deadline {
        let writes_before = state.ledger.session_row_writes();
        projection::report(&state, "builder", &no_op_beat(&state, &task_id)).expect("the beat");
        if state.ledger.session_row_writes() > writes_before {
            flushes += 1;
        }
        let persisted = persisted_last_seen(&path, "sess-restart");
        assert!(
            Utc::now().timestamp() - persisted <= 1,
            "the file is more than one interval behind the beat that just landed"
        );
        assert!(
            persisted >= opened_at,
            "and no beat moves the stored value backwards"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        flushes > 0,
        "the interval arrived inside the case: at least one beat reached the row"
    );

    let persisted_at_stop = persisted_last_seen(&path, "sess-restart");
    assert!(
        Utc::now().timestamp() - persisted_at_stop <= 1,
        "the row is no older than one interval at the stop"
    );

    // The stop: the process, its connections, and every beat it held go away.
    drop(state);
    let restarted = Server::open(&ServerInit {
        root: root.clone(),
        listen: None,
    })
    .expect("the server restarts on the same root");

    let rows = projection::sessions(
        &restarted,
        QuerySessionsArgs {
            task_id: Some(task_id.clone()),
            limit: 10,
            ..QuerySessionsArgs::default()
        },
    )
    .expect("the restarted server answers");
    assert_eq!(rows.len(), 1, "the row survived the restart");
    let answered = stamp(&rows[0]);
    assert_eq!(
        answered, persisted_at_stop,
        "a process with no beats answers exactly the file"
    );
    assert!(
        Utc::now().timestamp() - answered <= 1,
        "and what it answers is no older than one interval"
    );
}
