use chrono::Utc;
use onlyne_net::KeyPair;
use onlyne_proto::Report;
use onlyne_server::state::{Server, ServerInit};
use onlyne_server::{projection, stale};
use tempfile::tempdir;

const CERT_PIN: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn server_spec(key: &str) -> String {
    format!(
        r#"[server]
name = "local"
listen = "127.0.0.1:0"
cert_pin = "{CERT_PIN}"
stale_watch_secs = 60

[[client]]
role = "builder"
key = "{key}"
"#
    )
}

#[test]
fn server_observer_emits_stale_working_without_auto_settling() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("server");
    std::fs::create_dir_all(root.join(".onlyne")).unwrap();
    let key = KeyPair::from_seed([7_u8; 32]).public_str();
    std::fs::write(root.join(".onlyne/spec.toml"), server_spec(&key)).unwrap();
    let state = Server::open(&ServerInit { root, listen: None }).unwrap();
    let task_id = onlyne_proto::new_task_id();
    projection::report(
        &state,
        "builder",
        &Report::Ready {
            task_id: task_id.clone(),
            session_id: "sess-stale".into(),
            generation: 1,
            seq: 1,
            cluster_ref: None,
        },
    )
    .unwrap();
    let old = (Utc::now() - chrono::Duration::seconds((stale::STALE_WATCH_GRACE_SECS + 2) as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let conn = rusqlite::Connection::open(state.ledger.path()).unwrap();
    conn.execute(
        "UPDATE sessions SET updated_at=?1 WHERE task_id=?2",
        rusqlite::params![old, task_id],
    )
    .unwrap();
    drop(conn);

    let events = stale::observe_once(&state, Utc::now()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, stale::KIND_STALE_WORKING);
    let row = state.ledger.get_session_row(&task_id).unwrap().unwrap();
    assert_eq!(
        onlyne_server::projection::row_from_write(&row).public_lifecycle,
        onlyne_proto::Lifecycle::Working,
        "the watched row is working on the projection it stores"
    );
    let faults = state.ledger.open_faults().unwrap();
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0].kind, stale::KIND_STALE_WORKING);
    assert_eq!(faults[0].task_id.as_deref(), Some(task_id.as_str()));
}
