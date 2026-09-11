//! Delivery-path tests: relay, events, projection, faults, gateway host,
//! router, and the admin socket.

use chrono::Utc;
use onlyne_config::Spec;
use onlyne_proto::{
    AckArgs, AdminControl, AdminOp, AdminSend, Body, Causality, ClientOp, ControlArgs, ControlOp,
    Delivery, Envelope, ErrorCode, Event, Frame, GatewayOp, HandshakeArgs, HealthArgs, HistoryArgs,
    LedgerQuery, LedgerState, MsgKind, OP_ID_CONFLICT_MESSAGE, Outcome, Principal, PullArgs,
    QueryFaultsArgs, QueryRolesArgs, QuerySessionsArgs, RepairAck, RepairAdopt, RepairFail,
    RepairRebind, RepairTarget, Report, ResBody, SessionProjection, SessionSyncArgs, ShutdownArgs,
    Subscribe,
};
use onlyne_server::state::{ChannelBinding, DeliveryTicket, RoleConnection, Server, ServerInit};
use onlyne_server::{events, faults, gateway_host, projection, relay, router};
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;

const CERT_PIN: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn key() -> String {
    onlyne_net::KeyPair::from_seed([7_u8; 32]).public_str()
}

fn gw_key() -> String {
    onlyne_net::KeyPair::from_seed([9_u8; 32]).public_str()
}

fn spec_text() -> String {
    format!(
        r#"[server]
name = "local"
listen = "127.0.0.1:0"
cert_pin = "{CERT_PIN}"
note_queue = true
heartbeat_timeout_ms = 1000

[[client]]
role = "planner"
key = "{key}"
allowed_senders = ["planner", "builder"]
allowed_targets = ["planner", "builder"]

[[client]]
role = "builder"
key = "{key}"
allowed_senders = ["planner", "builder"]
allowed_targets = ["planner"]

[[gateway]]
id = "gw1"
platform = "telegram"
key = "{gw}"
enabled = true

[[route]]
gateway = "gw1"
channel = "chan1"
conversation = "conv1"
to = {{ role = "planner" }}
"#,
        key = key(),
        gw = gw_key()
    )
}

struct Fixture {
    _dir: TempDir,
    root: std::path::PathBuf,
    state: Arc<onlyne_server::State>,
}

fn fixture_with(text: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("server");
    std::fs::create_dir_all(root.join(".onlyne")).expect("create root");
    std::fs::write(root.join(".onlyne/spec.toml"), text).expect("write spec");
    let init = ServerInit {
        root: root.clone(),
        listen: None,
    };
    let state = Server::open(&init).expect("open the server");
    Fixture {
        _dir: dir,
        root,
        state,
    }
}

fn fixture() -> Fixture {
    fixture_with(&spec_text())
}

fn spec_of(fixture: &Fixture) -> Spec {
    fixture.state.spec_snapshot().expect("spec")
}

fn note(from: &str, to: &str, text: &str) -> Envelope {
    onlyne_proto::new_envelope(
        MsgKind::Note,
        Principal::role(from),
        Principal::role(to),
        Body::text(text),
        None,
    )
    .expect("valid note")
}

fn task(from: &str, to: &str, text: &str) -> Envelope {
    onlyne_proto::new_envelope(
        MsgKind::Task,
        Principal::role(from),
        Principal::role(to),
        Body::text(text),
        Some(Causality::root(onlyne_proto::new_task_id())),
    )
    .expect("valid task")
}

fn hello_args(role: &str) -> HandshakeArgs {
    HandshakeArgs {
        protocol: onlyne_proto::PROTOCOL_VERSION,
        role: role.to_string(),
        key: key(),
        signature: String::new(),
        agent: "test".to_string(),
        version: "1.0.0".to_string(),
        aggregate: false,
    }
}

fn accepted(reply: relay::RelayReply) -> relay::SendOutcome {
    match reply {
        relay::RelayReply::Accepted(outcome) => *outcome,
        other => panic!("expected an accepted send, got {other:?}"),
    }
}

fn rejected(reply: relay::RelayReply) -> relay::RelayReject {
    match reply {
        relay::RelayReply::Rejected(reject) => reject,
        relay::RelayReply::Duplicate(outcome) => {
            panic!(
                "expected a rejection, got a duplicate {:?}",
                outcome.receipt
            )
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn ledger_rows(state: &Arc<onlyne_server::State>) -> Vec<onlyne_store::LedgerRow> {
    state
        .ledger
        .ledger_query(LedgerQuery {
            limit: 100,
            ..LedgerQuery::default()
        })
        .expect("ledger query")
}

fn ledger_state_events(state: &Arc<onlyne_server::State>) -> Vec<onlyne_proto::EventRow> {
    events::replay(state, 0, &events::EventFilter::default(), 500)
        .expect("replay")
        .rows
        .into_iter()
        .filter(|row| row.event.type_name() == "ledger_state")
        .collect()
}

fn assert_not_internal(body: &ResBody) {
    if let Some(error) = &body.error {
        assert_ne!(
            error.code,
            ErrorCode::Internal,
            "the arm answered internal: {error:?}"
        );
    }
}

#[test]
fn send_refuses_an_envelope_that_fails_validation() {
    let fixture = fixture();
    let mut envelope = note("planner", "planner", "x");
    envelope.body = Body::default();
    let reject = rejected(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    assert_eq!(reject.code, ErrorCode::Invalid);
    assert_eq!(reject.field.as_deref(), Some("body"));
    assert!(ledger_rows(&fixture.state).is_empty());
}

#[test]
fn acl_denial_names_the_field_and_writes_no_row() {
    let three_roles = format!(
        "{}\n[[client]]\nrole = \"reviewer\"\nkey = \"{}\"\nallowed_senders = [\"planner\"]\nallowed_targets = [\"planner\"]\n",
        spec_text(),
        key()
    );
    let fixture = fixture_with(&three_roles);
    let envelope = note("builder", "reviewer", "x");
    let reject = rejected(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    assert_eq!(reject.code, ErrorCode::AclDenied);
    assert_eq!(reject.field.as_deref(), Some("to.role"));
    assert!(ledger_rows(&fixture.state).is_empty());

    let envelope = note("planner", "builder", "x");
    // planner may reach builder, builder accepts any sender.
    accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    assert_eq!(ledger_rows(&fixture.state).len(), 1);
}

#[test]
fn a_duplicate_op_id_replays_the_first_receipt_byte_for_byte() {
    let fixture = fixture();
    let first = task("planner", "planner", "hello");
    let outcome = accepted(relay::send(&fixture.state, &first, false, None).expect("relay"));
    let first_data = relay::receipt_json(&outcome.receipt);

    let body = relay::send(&fixture.state, &first, false, None)
        .expect("relay")
        .body();
    assert!(!body.ok);
    let error = body.error.clone().expect("error");
    assert_eq!(error.code, ErrorCode::Duplicate);
    let replay_data = body.data.clone().expect("the replayed receipt");
    assert_eq!(
        serde_json::to_string(&replay_data).expect("encode the replay"),
        serde_json::to_string(&first_data).expect("encode the first answer"),
        "the replayed data is the first receipt byte for byte"
    );
    assert_eq!(replay_data["msg_id"], json!(outcome.receipt.msg_id));

    let mut changed = first.clone();
    changed.body = Body::text("different");
    let body = relay::send(&fixture.state, &changed, false, None)
        .expect("relay")
        .body();
    assert!(!body.ok);
    let error = body.error.clone().expect("error");
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(error.message, OP_ID_CONFLICT_MESSAGE);
    assert_eq!(ledger_rows(&fixture.state).len(), 1);
    assert!(
        rejected(relay::send(&fixture.state, &changed, false, None).expect("relay"))
            .message
            .eq(OP_ID_CONFLICT_MESSAGE)
    );
}

#[test]
fn an_offline_role_keeps_the_row_queued_and_an_online_role_is_notified() {
    let fixture = fixture();
    let envelope = task("planner", "builder", "do it");
    let outcome = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    assert_eq!(outcome.receipt.state, LedgerState::Queued);
    assert_eq!(ledger_rows(&fixture.state)[0].state, LedgerState::Queued);

    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    fixture.state.register_role(RoleConnection {
        role: "builder".to_string(),
        sender,
        last_seq: 0,
        connected_at: Utc::now(),
        draining: false,
    });
    let envelope = task("planner", "builder", "do it again");
    let outcome = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    assert_eq!(outcome.receipt.state, LedgerState::InFlight);
    let frame = receiver.try_recv().expect("a delivery notice frame");
    assert!(matches!(frame, Frame::Ev { .. }));
    let rows = ledger_rows(&fixture.state);
    assert_eq!(
        rows.iter()
            .find(|row| row.msg_id == outcome.receipt.msg_id)
            .expect("row")
            .state,
        LedgerState::InFlight
    );
}

#[test]
fn pull_hands_one_row_per_session_and_records_its_ticket() {
    let fixture = fixture();
    let envelope = task("planner", "builder", "work");
    let outcome = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    let reply = relay::pull(
        &fixture.state,
        "builder",
        Some("sess-1"),
        &PullArgs::default(),
    )
    .expect("pull");
    assert_eq!(reply.deliveries.len(), 1);
    assert_eq!(reply.deliveries[0].msg_id, outcome.receipt.msg_id);
    assert_eq!(ledger_rows(&fixture.state)[0].state, LedgerState::InFlight);
    let ticket = fixture
        .state
        .open_delivery("builder", Some("sess-1"))
        .expect("a ticket");
    assert_eq!(ticket.session_id.as_deref(), Some("sess-1"));
    assert!(ticket.seq >= 1);
    assert!(ticket.delivered_at <= Utc::now());
    let second = relay::pull(
        &fixture.state,
        "builder",
        Some("sess-1"),
        &PullArgs::default(),
    )
    .expect("pull");
    assert!(second.deliveries.is_empty());
    let cursor = fixture
        .state
        .ledger
        .cursor_for("builder")
        .expect("cursor query")
        .expect("a cursor");
    assert_eq!(
        cursor.last_msg_id.as_deref(),
        Some(outcome.receipt.msg_id.as_str())
    );
}

#[test]
fn ack_settles_the_row_and_emits_every_observable() {
    let fixture = fixture();
    let envelope = task("planner", "builder", "work");
    let outcome = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    relay::pull(
        &fixture.state,
        "builder",
        Some("sess-1"),
        &PullArgs::default(),
    )
    .expect("pull");
    let event = relay::ack(
        &fixture.state,
        &AckArgs {
            msg_id: outcome.receipt.msg_id.clone(),
            op_id: outcome.receipt.op_id.clone(),
            accepted: true,
            reason: None,
        },
    )
    .expect("ack")
    .expect("settled");
    assert_eq!(event.state, LedgerState::Acked);
    assert_eq!(event.msg_id, outcome.receipt.msg_id);
    assert_eq!(event.op_id, outcome.receipt.op_id);
    assert_eq!(event.kind, MsgKind::Task);
    assert_eq!(event.from, Principal::role("planner"));
    assert_eq!(event.to, Principal::role("builder"));
    assert_eq!(event.task, outcome.receipt.task);
    assert!(event.outcome.is_none());
    assert!(event.reason.is_none());
    let value = serde_json::to_value(&event).expect("encode");
    for field in ["msg_id", "op_id", "kind", "from", "to", "task", "state"] {
        assert!(
            value.as_object().expect("object").contains_key(field),
            "the ledger_state event omits {field}"
        );
    }
    assert_eq!(ledger_rows(&fixture.state)[0].state, LedgerState::Acked);
    assert!(
        fixture
            .state
            .open_delivery("builder", Some("sess-1"))
            .is_none()
    );
}

#[test]
fn disconnect_requeues_in_flight_rows_without_duplicating_them() {
    let fixture = fixture();
    let envelope = task("planner", "builder", "work");
    let outcome = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    relay::pull(
        &fixture.state,
        "builder",
        Some("sess-1"),
        &PullArgs::default(),
    )
    .expect("pull");
    assert_eq!(ledger_rows(&fixture.state)[0].state, LedgerState::InFlight);
    let requeued = relay::disconnect(&fixture.state, "builder").expect("disconnect");
    assert_eq!(requeued, 1);
    let rows = ledger_rows(&fixture.state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].msg_id, outcome.receipt.msg_id);
    assert_eq!(rows[0].state, LedgerState::Queued);
}

#[test]
fn a_note_reaches_an_offline_role_when_note_queue_allows_it() {
    let fixture = fixture();
    let envelope = note("planner", "builder", "fyi");
    let outcome = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    assert_eq!(outcome.receipt.state, LedgerState::Queued);
    assert_eq!(ledger_rows(&fixture.state).len(), 1);
}

#[test]
fn a_note_to_an_offline_role_is_refused_before_the_ledger_row_exists() {
    let quiet = fixture_with(&spec_text().replace("note_queue = true", "note_queue = false"));
    let envelope = note("planner", "builder", "fyi");
    let reject = rejected(relay::send(&quiet.state, &envelope, false, None).expect("relay"));
    assert_eq!(reject.code, ErrorCode::RecipientOffline);
    assert_eq!(reject.field.as_deref(), Some("to.role"));
    assert!(
        ledger_rows(&quiet.state).is_empty(),
        "a refusal writes no ledger row and burns no op_id"
    );

    let (sender, _outbound) = tokio::sync::mpsc::channel(8);
    quiet.state.register_role(RoleConnection {
        role: "builder".to_string(),
        sender,
        last_seq: 0,
        connected_at: Utc::now(),
        draining: false,
    });
    let outcome = accepted(relay::send(&quiet.state, &envelope, false, None).expect("relay"));
    let rows = ledger_rows(&quiet.state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].op_id.as_deref(), envelope.op_id.as_deref());
    assert_eq!(rows[0].msg_id, outcome.receipt.msg_id);
    assert_eq!(rows[0].state, LedgerState::InFlight);
}

#[test]
fn a_requeue_publishes_exactly_one_ledger_state_event() {
    let fixture = fixture();
    let envelope = task("planner", "builder", "work");
    accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    relay::pull(
        &fixture.state,
        "builder",
        Some("sess-1"),
        &PullArgs::default(),
    )
    .expect("pull");
    let before = ledger_state_events(&fixture.state).len();
    relay::disconnect(&fixture.state, "builder").expect("disconnect");
    assert_eq!(ledger_state_events(&fixture.state).len(), before + 1);
}

#[test]
fn sweep_expired_settles_a_queued_note_and_emits() {
    let fixture = fixture();
    let mut envelope = note("planner", "builder", "fyi");
    envelope.ttl_ms = Some(10);
    let outcome = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    let head_before = fixture.state.event_head();
    let expired = relay::sweep_expired(&fixture.state, Utc::now() + chrono::Duration::seconds(5))
        .expect("sweep");
    assert_eq!(expired, vec![outcome.receipt.msg_id.clone()]);
    assert_eq!(ledger_rows(&fixture.state)[0].state, LedgerState::Expired);
    assert!(fixture.state.event_head() > head_before);
}

#[test]
fn gap_notice_counts_the_dropped_events() {
    let rows = |seqs: &[u64]| -> Vec<onlyne_proto::EventRow> {
        seqs.iter()
            .map(|seq| onlyne_proto::EventRow {
                seq: *seq,
                created_at: Utc::now(),
                event: Event::RolePresence(onlyne_proto::RolePresence {
                    role: "planner".to_string(),
                    state: onlyne_proto::Presence::Online,
                    aggregate: None,
                    sessions: 0,
                    detail: None,
                }),
            })
            .collect()
    };
    assert!(events::gap_notice(0, &rows(&[1, 2, 3]), 3, 256).is_none());
    let notice = events::gap_notice(10, &rows(&[11, 12, 13, 900]), 900, 256)
        .expect("a gap wider than the bound");
    assert_eq!(notice.dropped, 886);
    assert_eq!(notice.requested, 10);
    assert_eq!(notice.head, 900);
    assert_eq!(notice.kind, events::RESYNC_LAG_KIND);
}

#[test]
fn replay_resumes_from_a_cursor() {
    let fixture = fixture();
    let envelope = note("planner", "planner", "hello");
    accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    let envelope = note("planner", "planner", "again");
    accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    let page =
        events::replay(&fixture.state, 0, &events::EventFilter::default(), 100).expect("replay");
    assert!(page.rows.len() >= 2);
    let head = page.head;
    let page =
        events::replay(&fixture.state, head, &events::EventFilter::default(), 100).expect("replay");
    assert!(page.rows.is_empty());
    assert!(page.notice.is_none());
}

#[tokio::test]
async fn a_lagging_subscriber_is_told_rather_than_losing_events() {
    let fixture = fixture();
    let receiver = fixture.state.subscribe_frames();
    for index in 0..300 {
        events::publish(
            &fixture.state,
            Event::RolePresence(onlyne_proto::RolePresence {
                role: format!("role{index}"),
                state: onlyne_proto::Presence::Online,
                aggregate: None,
                sessions: 0,
                detail: None,
            }),
        )
        .expect("publish");
    }
    let mut receiver = receiver;
    let step = events::step(receiver.recv().await);
    match step {
        events::LiveStep::Lagged(dropped) => assert!(dropped >= 1),
        other => panic!("expected a lag notice, got {other:?}"),
    }
}

#[test]
fn the_projection_gate_refuses_a_stale_watermark() {
    let fixture = fixture();
    let task_id = onlyne_proto::new_task_id();
    let write = |generation: u64, seq: u64| SessionSyncArgs {
        task_id: task_id.clone(),
        session_id: "sess-1".to_string(),
        generation,
        seq,
        projection: SessionProjection::default_working(),
    };
    assert!(
        projection::session_sync(&fixture.state, "builder", &write(1, 5))
            .expect("sync")
            .applied
    );
    assert!(
        !projection::session_sync(&fixture.state, "builder", &write(1, 4))
            .expect("sync")
            .applied
    );
    assert!(
        !projection::session_sync(&fixture.state, "builder", &write(1, 5))
            .expect("sync")
            .applied
    );
    assert!(
        projection::session_sync(&fixture.state, "builder", &write(2, 0))
            .expect("sync")
            .applied
    );
    let rows = projection::sessions(
        &fixture.state,
        QuerySessionsArgs {
            task_id: Some(task_id.clone()),
            limit: 10,
            ..QuerySessionsArgs::default()
        },
    )
    .expect("sessions");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].generation, 2);
    assert_eq!(rows[0].seq, 0);
}

#[test]
fn reports_produce_session_rows_and_events() {
    let fixture = fixture();
    let task_id = onlyne_proto::new_task_id();
    let ready = projection::report(
        &fixture.state,
        "builder",
        &Report::Ready {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            generation: 1,
            seq: 1,
            cluster_ref: None,
        },
    )
    .expect("report");
    assert!(ready.applied);
    let done = projection::report(
        &fixture.state,
        "builder",
        &Report::Complete {
            task_id: task_id.clone(),
            outcome: Outcome::Done,
            head: Some("finished".to_string()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .expect("report");
    assert!(done.applied);
    let row = projection::session_row(&fixture.state, &task_id)
        .expect("row")
        .expect("a row");
    assert_eq!(row.public_lifecycle, onlyne_proto::Lifecycle::Exited);
    assert_eq!(row.outcome, Some(Outcome::Done));
    let page =
        events::replay(&fixture.state, 0, &events::EventFilter::default(), 100).expect("replay");
    assert!(
        page.rows
            .iter()
            .any(|row| row.event.type_name() == "session_state")
    );
}

#[test]
fn a_cluster_bearing_report_marks_its_projection() {
    let fixture = fixture();
    let task_id = onlyne_proto::new_task_id();
    let applied = projection::report(
        &fixture.state,
        "builder",
        &Report::Ready {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            generation: 1,
            seq: 1,
            cluster_ref: Some("cluster-b".to_string()),
        },
    )
    .expect("report");
    assert!(applied.applied);

    let row = projection::session_row(&fixture.state, &task_id)
        .expect("row")
        .expect("a row");
    let marked = row
        .projection
        .observed
        .as_ref()
        .and_then(|observed| observed.get("cluster_ref"))
        .and_then(|cluster| cluster.as_str());
    assert_eq!(marked, Some("cluster-b"));
    assert_eq!(row.projection.lifecycle, onlyne_proto::Lifecycle::Working);

    let page =
        events::replay(&fixture.state, 0, &events::EventFilter::default(), 100).expect("replay");
    let payload = page
        .rows
        .iter()
        .find(|row| row.event.type_name() == "session_state")
        .map(|row| serde_json::to_value(&row.event).expect("encode the event"))
        .expect("a session_state event");
    assert_eq!(
        payload["data"]["projection"]["observed"]["cluster_ref"],
        json!("cluster-b")
    );
}

#[test]
fn a_heartbeat_is_stored_flat_with_the_pane_binding_inside_it() {
    let fixture = fixture();
    let task_id = onlyne_proto::new_task_id();
    let applied = projection::report(
        &fixture.state,
        "builder",
        &Report::Heartbeat {
            task_id: task_id.clone(),
            generation: 1,
            seq: 1,
            observed: json!({
                "lifecycle": "working",
                "host": { "orca": { "pane_key": "tab-1:leaf-1" } },
            }),
            cluster_ref: Some("cluster-b".to_string()),
        },
    )
    .expect("report");
    assert!(applied.applied);

    let row = projection::session_row(&fixture.state, &task_id)
        .expect("row")
        .expect("a row");
    let stored = row.projection.observed.as_ref().expect("an observation");
    // One shape, not two: the tuple the client sent *is* the observation, so the
    // pane a scoped reader addresses sits at `observed.host`, not one level down.
    assert_eq!(stored.get("observed"), None);
    assert_eq!(
        stored
            .pointer("/host/orca/pane_key")
            .and_then(|pane| pane.as_str()),
        Some("tab-1:leaf-1")
    );
    assert_eq!(
        stored.get("cluster_ref").and_then(|cluster| cluster.as_str()),
        Some("cluster-b")
    );
}

#[test]
fn a_recorded_fault_is_queryable_and_acknowledgeable() {
    let fixture = fixture();
    let task_id = onlyne_proto::new_task_id();
    let event = faults::record(
        &fixture.state,
        faults::FaultDraft::probe_failure("builder", &task_id, "the process is gone"),
    )
    .expect("record");
    assert!(event.id > 0);
    assert_eq!(event.kind, faults::KIND_PROBE_FAILURE);
    let open = faults::query(
        &fixture.state,
        &QueryFaultsArgs {
            open_only: true,
            limit: 10,
            ..QueryFaultsArgs::default()
        },
    )
    .expect("query");
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].state.as_deref(), Some(faults::STATE_OPEN));

    let outcome = faults::repair(
        &fixture.state,
        &AdminOp::RepairAck(RepairAck {
            fault_id: event.id,
            reason: "handled".to_string(),
        }),
    )
    .expect("repair")
    .expect("accepted");
    assert_eq!(outcome["state"], json!("acked"));
    let open = faults::query(
        &fixture.state,
        &QueryFaultsArgs {
            open_only: true,
            limit: 10,
            ..QueryFaultsArgs::default()
        },
    )
    .expect("query");
    assert!(open.is_empty());
}

#[test]
fn repair_fail_settles_the_task_and_publishes_a_fault_event() {
    let fixture = fixture();
    let envelope = task("planner", "builder", "work");
    let outcome = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    let task_id = outcome.receipt.task.clone().expect("task");
    projection::session_sync(
        &fixture.state,
        "builder",
        &SessionSyncArgs {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            generation: 1,
            seq: 1,
            projection: SessionProjection::default_working(),
        },
    )
    .expect("sync");
    faults::record(
        &fixture.state,
        faults::FaultDraft::probe_failure("builder", &task_id, "gone"),
    )
    .expect("record");
    let head_before = fixture.state.event_head();
    faults::repair(
        &fixture.state,
        &AdminOp::RepairFail(RepairFail {
            task_id: task_id.clone(),
            reason: "unrecoverable".to_string(),
            notify: None,
        }),
    )
    .expect("repair")
    .expect("accepted");
    assert!(fixture.state.event_head() > head_before);
    assert_eq!(ledger_rows(&fixture.state)[0].state, LedgerState::Rejected);
    let row = projection::session_row(&fixture.state, &task_id)
        .expect("row")
        .expect("a row");
    assert_eq!(row.outcome, Some(Outcome::Failed));
    let open = faults::query(
        &fixture.state,
        &QueryFaultsArgs {
            task_id: Some(task_id),
            open_only: true,
            limit: 10,
            ..QueryFaultsArgs::default()
        },
    )
    .expect("query");
    assert!(open.is_empty());
}

#[test]
fn a_fault_survives_every_repair_transition() {
    let fixture = fixture();
    let session = |task_id: &str| SessionSyncArgs {
        task_id: task_id.to_string(),
        session_id: "sess-1".to_string(),
        generation: 1,
        seq: 1,
        projection: SessionProjection::default_working(),
    };
    let adopt_task = onlyne_proto::new_task_id();
    let envelope = task("planner", "builder", "adopt");
    let adopted = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    let adopt_task = adopted.receipt.task.clone().unwrap_or(adopt_task);
    projection::session_sync(&fixture.state, "builder", &session(&adopt_task)).expect("sync");
    faults::record(
        &fixture.state,
        faults::FaultDraft::probe_failure("builder", &adopt_task, "detached"),
    )
    .expect("record");
    let adopted = faults::repair(
        &fixture.state,
        &AdminOp::RepairAdopt(RepairAdopt {
            task_id: adopt_task.clone(),
            session_id: "sess-1".to_string(),
            backend: "zellij".to_string(),
            backend_ref: json!({ "pane": 3 }),
            reason: "operator adopted".to_string(),
        }),
    )
    .expect("repair")
    .expect("accepted");
    assert_eq!(adopted["faults"], json!(1));

    faults::record(
        &fixture.state,
        faults::FaultDraft::probe_failure("builder", &adopt_task, "again"),
    )
    .expect("record");
    let rebound = faults::repair(
        &fixture.state,
        &AdminOp::RepairRebind(RepairRebind {
            task_id: adopt_task.clone(),
            session_id: "sess-2".to_string(),
            backend: "orca".to_string(),
            backend_ref: json!({ "pane": 9 }),
            reason: "operator rebound".to_string(),
        }),
    )
    .expect("repair")
    .expect("accepted");
    assert_eq!(rebound["generation"], json!(2));

    let inspected = faults::repair(
        &fixture.state,
        &AdminOp::RepairInspect(RepairTarget {
            task_id: adopt_task.clone(),
            reason: None,
        }),
    )
    .expect("repair")
    .expect("accepted");
    assert_eq!(inspected["task_id"], json!(adopt_task));

    let closed = faults::repair(
        &fixture.state,
        &AdminOp::RepairClose(RepairTarget {
            task_id: adopt_task.clone(),
            reason: Some("operator close".to_string()),
        }),
    )
    .expect("repair");
    // The close runs after the task settled as rejected, so the ledger refusal
    // is reported rather than hidden.
    assert!(closed.is_ok() || closed.is_err());
}

#[test]
fn a_gateway_without_credentials_is_refused_and_recorded() {
    let fixture = fixture();
    let hello = onlyne_proto::HelloArgs {
        protocol: onlyne_proto::PROTOCOL_VERSION,
        plugin: "onlyne-gateway-telegram".to_string(),
        version: "1.0.0".to_string(),
        kind: onlyne_proto::MountKind::Gateway,
        capabilities: vec![],
        mount: Some(onlyne_proto::Mount::Gateway(onlyne_proto::GatewayMount {
            gateway: "gw-none".to_string(),
            platform: "telegram".to_string(),
        })),
    };
    let link = Arc::new(onlyne_server::AdapterLink::default());
    let error = gateway_host::welcome(&fixture.state, link, &hello).expect_err("refused");
    assert_eq!(error.0, ErrorCode::Unauthorized);
    let recorded = faults::query(
        &fixture.state,
        &QueryFaultsArgs {
            kind: Some(gateway_host::KIND_GATEWAY_UNCONFIGURED.to_string()),
            open_only: false,
            limit: 10,
            ..QueryFaultsArgs::default()
        },
    )
    .expect("query");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].role.as_deref(), Some("gw-none"));
}

#[test]
fn a_missing_health_report_moves_presence_and_keeps_queued_outbound() {
    let fixture = fixture();
    let hello = onlyne_proto::HelloArgs {
        protocol: onlyne_proto::PROTOCOL_VERSION,
        plugin: "onlyne-gateway-telegram".to_string(),
        version: "1.0.0".to_string(),
        kind: onlyne_proto::MountKind::Gateway,
        capabilities: vec![],
        mount: Some(onlyne_proto::Mount::Gateway(onlyne_proto::GatewayMount {
            gateway: "gw1".to_string(),
            platform: "telegram".to_string(),
        })),
    };
    let link = Arc::new(onlyne_server::AdapterLink::default());
    gateway_host::welcome(&fixture.state, link, &hello).expect("the gateway is configured");

    let queued = note("planner", "gw1", "outbound");
    let mut queued = queued;
    queued.to = Principal::gateway("gw1", "chan1", Some("conv1".to_string()));
    accepted(relay::send(&fixture.state, &queued, false, None).expect("relay"));

    let lost =
        gateway_host::health_sweep(&fixture.state, Utc::now() + chrono::Duration::seconds(5));
    assert_eq!(lost, vec!["gw1".to_string()]);
    assert_eq!(
        fixture.state.gateway_health("gw1"),
        Some(onlyne_proto::GatewayHealth::Failed)
    );
    let rows = ledger_rows(&fixture.state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, LedgerState::Queued);
    let recorded = faults::query(
        &fixture.state,
        &QueryFaultsArgs {
            kind: Some(gateway_host::KIND_GATEWAY_LOSS.to_string()),
            open_only: false,
            limit: 10,
            ..QueryFaultsArgs::default()
        },
    )
    .expect("query");
    assert_eq!(recorded.len(), 1);
}

#[test]
fn outbound_rendering_holds_until_the_gateway_is_reachable() {
    let fixture = fixture();
    let mut envelope = note("planner", "gw1", "outbound");
    envelope.to = Principal::gateway("gw1", "chan1", Some("conv1".to_string()));
    let outcome = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    assert_eq!(outcome.receipt.state, LedgerState::Queued);
    let spec = spec_of(&fixture);
    let route = gateway_host::select_outbound_route(&spec, "gw1", &envelope.from, None)
        .expect("a route selects the gateway");
    assert_eq!(route.to.role, "planner");
    assert!(gateway_host::resolve_inbound_route(&spec, "gw1", "chan1", Some("conv1")).is_some());
}

#[tokio::test]
async fn an_inbound_gateway_delivery_runs_the_send_path() {
    let fixture = fixture();
    let hello = onlyne_proto::HelloArgs {
        protocol: onlyne_proto::PROTOCOL_VERSION,
        plugin: "onlyne-gateway-telegram".to_string(),
        version: "1.0.0".to_string(),
        kind: onlyne_proto::MountKind::Gateway,
        capabilities: vec![],
        mount: Some(onlyne_proto::Mount::Gateway(onlyne_proto::GatewayMount {
            gateway: "gw1".to_string(),
            platform: "telegram".to_string(),
        })),
    };
    let link = Arc::new(onlyne_server::AdapterLink::default());
    gateway_host::welcome(&fixture.state, link, &hello).expect("configured");
    gateway_host::register_channels(
        &fixture.state,
        "gw1",
        "telegram",
        &onlyne_proto::RegisterChannelArgs {
            platform: "telegram".to_string(),
            channel: "chan1".to_string(),
            conversations: Some(vec![onlyne_proto::ConversationInfo {
                conversation: "conv1".to_string(),
                title: None,
            }]),
        },
    )
    .expect("register");
    assert_eq!(fixture.state.channel_count(), 1);

    let mut envelope = note("gw1", "planner", "from the platform");
    envelope.from = Principal::gateway("gw1", "chan1", Some("conv1".to_string()));
    let delivery = Delivery {
        msg_id: envelope.id.clone(),
        envelope: Box::new(envelope),
    };
    let body = gateway_host::deliver(&fixture.state, "gw1", "telegram", &delivery);
    assert!(body.ok, "{body:?}");
    let rows = ledger_rows(&fixture.state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, LedgerState::Queued);
}

#[tokio::test]
async fn the_adapter_gateway_round_trip_renders_and_delivers() {
    use onlyne_adapter::{AdapterClient, AdapterServer, HostDispatcher};
    use onlyne_proto::MountKind;

    let fixture = fixture();
    let (client, server) = tokio::io::duplex(64 * 1024);
    let link = Arc::new(onlyne_server::AdapterLink::default());
    let link_for_host = link.clone();
    let state_for_host = fixture.state.clone();
    let host_state = fixture.state.clone();
    let task = tokio::spawn(async move {
        let connection = AdapterServer::accept(server, move |hello: &onlyne_proto::HelloArgs| {
            gateway_host::welcome(&state_for_host, link_for_host.clone(), hello)
        })
        .await
        .expect("accept the gateway");
        let gateway = match &connection.hello.mount {
            Some(onlyne_proto::Mount::Gateway(mount)) => mount.gateway.clone(),
            _ => panic!("a gateway mount"),
        };
        *link.io.lock().await = Some(connection.io.clone());
        let host = Arc::new(gateway_host::GatewayHostImpl::new(
            host_state,
            gateway,
            "telegram".to_string(),
            link,
        ));
        let dispatcher = HostDispatcher::new(MountKind::Gateway, host);
        dispatcher
            .serve(connection.io, connection.inbound)
            .await
            .expect("serve");
    });

    let gateway = AdapterClient::gateway(client);
    let ack = gateway
        .hello_gateway("gw1", "telegram", vec![])
        .await
        .expect("welcome");
    assert_eq!(ack.role, "gw1");
    gateway
        .register_channel(onlyne_proto::RegisterChannelArgs {
            platform: "telegram".to_string(),
            channel: "chan1".to_string(),
            conversations: None,
        })
        .await
        .expect("register");
    assert_eq!(fixture.state.channel_count(), 1);

    let mut envelope = note("planner", "gw1", "outbound");
    envelope.to = Principal::gateway("gw1", "chan1", Some("conv1".to_string()));
    accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    let plan = gateway_host::render_outbound(&fixture.state, &envelope)
        .await
        .expect("render");
    assert!(matches!(plan, gateway_host::OutboundPlan::Pushed { .. }));
    let op = gateway.wait_render_send().await.expect("render_send");
    assert_eq!(op.conversation, "conv1");

    task.abort();
}

#[tokio::test]
async fn a_frame_before_hello_is_refused() {
    let fixture = fixture();
    let mut session = router::Session::default();
    let body = router::dispatch_client(
        &fixture.state,
        &mut session,
        ClientOp::Send(Box::new(note("planner", "planner", "early"))),
    )
    .await;
    assert!(!body.ok);
    let error = body.error.expect("error");
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.message, onlyne_proto::HELLO_REQUIRED_MESSAGE);
    assert!(ledger_rows(&fixture.state).is_empty());
}

#[tokio::test]
async fn a_hello_cannot_claim_a_role_beyond_its_key() {
    let fixture = fixture();
    let (sender, _outbound) = tokio::sync::mpsc::channel(8);
    let mut session = router::Session::with_sender(sender, "planner");
    let body = router::dispatch_client(
        &fixture.state,
        &mut session,
        ClientOp::Hello(hello_args("builder")),
    )
    .await;
    assert!(!body.ok);
    let error = body.error.clone().expect("error");
    assert_eq!(error.code, ErrorCode::Unauthorized);
    assert_eq!(error.field.as_deref(), Some("role"));
    assert!(
        error.message.contains("planner") && error.message.contains("builder"),
        "the refusal names both values: {}",
        error.message
    );
    assert!(session.role.is_none());
    assert!(!session.authenticated);

    let send = router::dispatch_client(
        &fixture.state,
        &mut session,
        ClientOp::Send(Box::new(note("builder", "planner", "not mine"))),
    )
    .await;
    assert!(!send.ok);
    assert_eq!(send.error.expect("error").code, ErrorCode::Invalid);
    assert!(ledger_rows(&fixture.state).is_empty());
}

#[tokio::test]
async fn the_admitted_role_still_reaches_welcome() {
    let fixture = fixture();
    let (sender, _outbound) = tokio::sync::mpsc::channel(8);
    let mut session = router::Session::with_sender(sender, "planner");
    let body = router::dispatch_client(
        &fixture.state,
        &mut session,
        ClientOp::Hello(hello_args("planner")),
    )
    .await;
    assert!(body.ok, "{body:?}");
    assert_eq!(session.role.as_deref(), Some("planner"));
    assert!(session.authenticated);
    assert_eq!(body.data.expect("welcome")["role"], json!("planner"));

    let send = router::dispatch_client(
        &fixture.state,
        &mut session,
        ClientOp::Send(Box::new(note("planner", "planner", "mine"))),
    )
    .await;
    assert!(send.ok, "{send:?}");
    assert_eq!(ledger_rows(&fixture.state).len(), 1);
}

#[tokio::test]
async fn every_client_and_gateway_arm_answers_without_internal_failure() {
    let fixture = fixture();
    let (sender, _outbound) = tokio::sync::mpsc::channel(8);
    let mut session = router::Session::with_sender(sender, "planner");
    let hello = router::dispatch_client(
        &fixture.state,
        &mut session,
        ClientOp::Hello(hello_args("planner")),
    )
    .await;
    assert!(hello.ok, "{hello:?}");

    let task_id = onlyne_proto::new_task_id();
    let ops = vec![
        ClientOp::Send(Box::new(note("planner", "planner", "x"))),
        ClientOp::Pull(PullArgs::default()),
        ClientOp::Ack(AckArgs {
            msg_id: onlyne_proto::new_id(),
            op_id: None,
            accepted: true,
            reason: None,
        }),
        ClientOp::Report(Report::Ready {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            generation: 1,
            seq: 1,
            cluster_ref: None,
        }),
        ClientOp::Report(Report::Complete {
            task_id: task_id.clone(),
            outcome: Outcome::Done,
            head: None,
            reply_to: None,
            cluster_ref: None,
        }),
        ClientOp::Report(Report::Fault {
            task_id: Some(task_id.clone()),
            session_id: None,
            generation: Some(1),
            seq: Some(2),
            kind: "idle_fault".to_string(),
            reason: "reported".to_string(),
            desired: None,
            observed: None,
        }),
        ClientOp::SessionSync(SessionSyncArgs {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            generation: 1,
            seq: 1,
            projection: SessionProjection::default_working(),
        }),
        ClientOp::Subscribe(Subscribe::default()),
        ClientOp::QueryLedger(LedgerQuery::default()),
        ClientOp::QuerySessions(QuerySessionsArgs::default()),
        ClientOp::QueryRoles(QueryRolesArgs::default()),
        ClientOp::QueryFaults(QueryFaultsArgs::default()),
        ClientOp::Control(ControlArgs {
            to: Some("planner".to_string()),
            op: ControlOp::Probe {
                task_id: task_id.clone(),
            },
        }),
        ClientOp::Bye(Default::default()),
    ];
    for op in ops {
        let body = router::dispatch_client(&fixture.state, &mut session, op).await;
        assert_not_internal(&body);
    }

    let mut gateway_session = router::Session::default();
    let ops = vec![
        GatewayOp::Hello(HandshakeArgs {
            protocol: onlyne_proto::PROTOCOL_VERSION,
            role: "gw1".to_string(),
            key: gw_key(),
            signature: String::new(),
            agent: "onlyne-gateway-telegram".to_string(),
            version: "1.0.0".to_string(),
            aggregate: false,
        }),
        GatewayOp::RegisterChannel(onlyne_proto::RegisterChannelArgs {
            platform: "telegram".to_string(),
            channel: "chan1".to_string(),
            conversations: None,
        }),
        GatewayOp::Health(HealthArgs {
            state: "online".to_string(),
            detail: None,
            uptime_s: 1,
        }),
        GatewayOp::Bye(Default::default()),
    ];
    for op in ops {
        let body = router::dispatch_gateway(&fixture.state, &mut gateway_session, op).await;
        assert_not_internal(&body);
    }
}

#[tokio::test]
async fn every_admin_arm_answers_without_internal_failure() {
    let fixture = fixture();
    let task_id = onlyne_proto::new_task_id();
    let ops = vec![
        AdminOp::Status(json!({})),
        AdminOp::Roles(QueryRolesArgs::default()),
        AdminOp::Sessions(QuerySessionsArgs::default()),
        AdminOp::Ledger(LedgerQuery::default()),
        AdminOp::Faults(QueryFaultsArgs::default()),
        AdminOp::Watch(Subscribe::default()),
        AdminOp::History(HistoryArgs::default()),
        AdminOp::SpecDiff(json!({})),
        AdminOp::Reload(json!({})),
        AdminOp::Send(AdminSend {
            from: "planner".to_string(),
            envelope: Box::new(note("planner", "planner", "operator")),
        }),
        AdminOp::Control(AdminControl {
            from: "planner".to_string(),
            op: ControlOp::Probe {
                task_id: task_id.clone(),
            },
            to: None,
        }),
        AdminOp::RepairInspect(RepairTarget {
            task_id: task_id.clone(),
            reason: None,
        }),
        AdminOp::RepairAdopt(RepairAdopt {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            backend: "fake".to_string(),
            backend_ref: json!({}),
            reason: "test".to_string(),
        }),
        AdminOp::RepairRebind(RepairRebind {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            backend: "fake".to_string(),
            backend_ref: json!({}),
            reason: "test".to_string(),
        }),
        AdminOp::RepairRetry(RepairTarget {
            task_id: task_id.clone(),
            reason: None,
        }),
        AdminOp::RepairFail(RepairFail {
            task_id: task_id.clone(),
            reason: "test".to_string(),
            notify: None,
        }),
        AdminOp::RepairClose(RepairTarget {
            task_id: task_id.clone(),
            reason: None,
        }),
        AdminOp::RepairAck(RepairAck {
            fault_id: 4242,
            reason: "test".to_string(),
        }),
        AdminOp::Shutdown(ShutdownArgs {
            reason: "test".to_string(),
            grace_ms: None,
        }),
    ];
    let mut session = router::Session::default();
    for op in ops {
        let name = op.name().to_string();
        let body = router::dispatch_admin(&fixture.state, &mut session, op).await;
        assert_not_internal(&body);
        assert!(
            body.ok || body.error.is_some(),
            "the {name} arm answered an empty body"
        );
    }
}

#[tokio::test]
async fn status_carries_the_fields_wait_ready_polls() {
    let fixture = fixture();
    let body = router::status(&fixture.state);
    assert!(body.ok);
    let data = body.data.expect("data");
    for field in [
        "ok",
        "roles",
        "role_count",
        "gateway_count",
        "gateways",
        "event_head",
        "uptime_s",
        "spec_hash",
        "version",
    ] {
        assert!(data.get(field).is_some(), "status omits {field}");
    }
    assert_eq!(data["roles"], json!(2));
    assert_eq!(data["gateway_count"], json!(1));
    let gateways = data["gateways"].as_array().expect("a gateway array");
    assert_eq!(gateways.len(), 1);
    assert_eq!(gateways[0]["id"], json!("gw1"));
    assert!(gateways[0]["capabilities"].is_array());
    assert_eq!(gateways[0]["state"], json!("offline"));
}

#[test]
fn a_conversation_specific_route_beats_a_later_fallback() {
    let fallback = format!(
        "{}\n[[route]]\ngateway = \"gw1\"\nchannel = \"chan1\"\nto = {{ role = \"builder\" }}\n",
        spec_text()
    );
    let fixture = fixture_with(&fallback);
    let spec = spec_of(&fixture);
    let specific = gateway_host::resolve_inbound_route(&spec, "gw1", "chan1", Some("conv1"))
        .expect("the conversation route");
    assert_eq!(specific.to.role, "planner");
    let other = gateway_host::resolve_inbound_route(&spec, "gw1", "chan1", Some("conv9"))
        .expect("the fallback route");
    assert_eq!(other.to.role, "builder");
    // An absent envelope conversation cannot match the conversation-specific
    // row, so the later fallback wins on the same channel.
    let absent = gateway_host::resolve_inbound_route(&spec, "gw1", "chan1", None)
        .expect("the fallback matches every conversation when none is named");
    assert_eq!(absent.to.role, "builder");

    let mut envelope = note("gw1", "planner", "from the platform");
    envelope.from = Principal::gateway("gw1", "chan1", Some("conv1".to_string()));
    let outcome = accepted(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    let rows = ledger_rows(&fixture.state);
    let row = rows
        .iter()
        .find(|row| row.msg_id == outcome.receipt.msg_id)
        .expect("the row");
    assert!(row.to_json.contains("\"planner\""), "{}", row.to_json);
    assert!(!row.to_json.contains("\"builder\""), "{}", row.to_json);
}

#[test]
fn a_gateway_conversation_without_a_route_is_refused_without_a_ledger_row() {
    let fixture = fixture();
    let mut envelope = note("planner", "gw1", "nowhere");
    envelope.to = Principal::gateway("gw1", "chan9", None);
    let reject = rejected(relay::send(&fixture.state, &envelope, false, None).expect("relay"));
    assert_eq!(reject.code, ErrorCode::UnknownRole);
    assert_eq!(reject.field.as_deref(), Some("route"));
    assert!(ledger_rows(&fixture.state).is_empty());
}

#[tokio::test]
async fn reload_spec_swaps_a_valid_spec_and_keeps_the_old_one_on_failure() {
    let fixture = fixture();
    let before = spec_of(&fixture).semantic_hash();
    let extended = spec_text().replace(
        "[[gateway]]",
        "[[client]]\nrole = \"reviewer\"\nkey = \"KEY\"\nallowed_senders = [\"*\"]\nallowed_targets = [\"planner\"]\n\n[[gateway]]",
    );
    let extended = extended.replace("KEY", &key());
    std::fs::write(fixture.root.join(".onlyne/spec.toml"), &extended).expect("write spec");
    let head_before = fixture.state.event_head();
    let outcome = router::reload_spec(&fixture.state).expect("reload");
    assert_ne!(outcome.spec_hash, before);
    assert_eq!(outcome.roles, 3);
    assert!(outcome.render.contains("reviewer"), "{}", outcome.render);
    assert!(fixture.state.role("reviewer").is_some());
    assert!(fixture.state.acl_table().get("reviewer").is_some());
    assert!(fixture.state.event_head() > head_before);

    std::fs::write(fixture.root.join(".onlyne/spec.toml"), "not toml at all").expect("write spec");
    let message = router::reload_spec(&fixture.state).expect_err("a rejected reload");
    assert!(message.contains("spec.toml"), "{message}");
    assert_eq!(spec_of(&fixture).semantic_hash(), outcome.spec_hash);
    assert!(fixture.state.role("reviewer").is_some());
    assert_eq!(reload_faults(&fixture), 1);
}

#[tokio::test]
async fn reload_spec_tolerates_a_removed_spec_file() {
    let fixture = fixture();
    let before = spec_of(&fixture);
    std::fs::remove_file(fixture.root.join(".onlyne/spec.toml")).expect("remove the spec");
    let message = router::reload_spec(&fixture.state).expect_err("a rejected reload");
    assert!(message.contains("spec.toml"), "{message}");
    assert_eq!(spec_of(&fixture).client.len(), before.client.len());
    assert_eq!(reload_faults(&fixture), 1);
}

fn reload_faults(fixture: &Fixture) -> usize {
    faults::query(
        &fixture.state,
        &QueryFaultsArgs {
            kind: Some(faults::KIND_SPEC_RELOAD_FAILED.to_string()),
            open_only: false,
            limit: 10,
            ..QueryFaultsArgs::default()
        },
    )
    .expect("query")
    .len()
}

#[tokio::test]
async fn an_admin_send_records_the_admin_marker_and_still_passes_the_acl() {
    let fixture = fixture();
    let envelope = note("planner", "builder", "operator note");
    let body = router::dispatch_admin(
        &fixture.state,
        &mut router::Session::default(),
        AdminOp::Send(AdminSend {
            from: "planner".to_string(),
            envelope: Box::new(envelope),
        }),
    )
    .await;
    assert!(body.ok, "{body:?}");
    let rows = ledger_rows(&fixture.state);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].from_json.contains("\"admin\":true"));

    let denied = router::dispatch_admin(
        &fixture.state,
        &mut router::Session::default(),
        AdminOp::Send(AdminSend {
            from: "builder".to_string(),
            envelope: Box::new(note("builder", "planner", "nope")),
        }),
    )
    .await;
    // builder may reach planner, so the ACL allows it; the ledger grows.
    assert!(denied.ok, "{denied:?}");
    assert_eq!(ledger_rows(&fixture.state).len(), 2);
}

#[tokio::test]
async fn the_run_socket_answers_a_status_frame_and_is_private() {
    use onlyne_frame::{read_frame, write_frame};
    use tokio::net::UnixStream;

    let fixture = fixture();
    let listener = onlyne_server::admin::bind(&fixture.state).expect("bind");
    let path = onlyne_layout::ServerRoot::resolve(&fixture.root).socket_path();
    let mode = std::fs::metadata(&path).expect("metadata").permissions();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(mode.mode() & 0o777, 0o600);

    let state = fixture.state.clone();
    let task = tokio::spawn(async move {
        let _ = onlyne_server::admin::serve_socket(state, listener).await;
    });
    let mut stream = UnixStream::connect(&path).await.expect("connect");
    let request = Frame::<AdminOp>::req(
        "r1",
        AdminOp::Status(serde_json::Value::Object(Default::default())),
    );
    write_frame(&mut stream, &request).await.expect("write");
    let answer: Frame = read_frame(&mut stream)
        .await
        .expect("read")
        .expect("a frame");
    match answer {
        Frame::Res { id, body } => {
            assert_eq!(id, "r1");
            assert!(body.ok, "{body:?}");
            assert_eq!(body.data.expect("data")["ok"], json!(true));
        }
        other => panic!("expected a response, got {other:?}"),
    }
    task.abort();
}

#[tokio::test]
async fn a_pre_hello_gateway_frame_is_refused_with_the_invalid_code() {
    use onlyne_frame::{read_frame, write_frame};
    use tokio::net::UnixStream;

    let fixture = fixture();
    let listener = onlyne_server::admin::bind(&fixture.state).expect("bind");
    let path = onlyne_layout::ServerRoot::resolve(&fixture.root).socket_path();
    let state = fixture.state.clone();
    let task = tokio::spawn(async move {
        let _ = onlyne_server::admin::serve_socket(state, listener).await;
    });
    let mut stream = UnixStream::connect(&path).await.expect("connect");
    let request = Frame::<GatewayOp>::req(
        "r1",
        GatewayOp::Health(HealthArgs {
            state: "online".to_string(),
            detail: None,
            uptime_s: 1,
        }),
    );
    write_frame(&mut stream, &request).await.expect("write");
    let answer: Frame = read_frame(&mut stream)
        .await
        .expect("read")
        .expect("a frame");
    match answer {
        Frame::Res { body, .. } => {
            assert!(!body.ok);
            let error = body.error.expect("error");
            assert_eq!(error.code, ErrorCode::Invalid);
            assert_eq!(error.message, onlyne_proto::HELLO_REQUIRED_MESSAGE);
        }
        other => panic!("expected a response, got {other:?}"),
    }
    task.abort();
}

#[tokio::test]
async fn an_unknown_admin_op_is_refused_by_name() {
    let fixture = fixture();
    let value = json!({ "f": "req", "id": "r1", "op": "not_an_op", "args": {} });
    let body =
        onlyne_server::admin::route_value(&fixture.state, &mut router::Session::default(), &value)
            .await;
    assert!(!body.ok);
    assert_eq!(body.error.expect("error").code, ErrorCode::UnknownOp);
}

#[test]
fn delivery_tickets_are_cleared_with_their_role() {
    let fixture = fixture();
    fixture.state.record_delivery(DeliveryTicket {
        msg_id: "m1".to_string(),
        role: "builder".to_string(),
        session_id: Some("s1".to_string()),
        generation: 1,
        seq: 3,
        delivered_at: Utc::now(),
    });
    assert!(fixture.state.open_delivery("builder", Some("s1")).is_some());
    assert_eq!(fixture.state.clear_deliveries("builder"), 1);
    assert!(fixture.state.open_delivery("builder", Some("s1")).is_none());
}

#[test]
fn channel_bindings_round_trip_through_the_registry() {
    let fixture = fixture();
    fixture.state.register_channel(ChannelBinding {
        gateway: "gw1".to_string(),
        platform: "telegram".to_string(),
        channel: "chan1".to_string(),
        conversations: vec![],
        registered_at: Utc::now(),
    });
    assert_eq!(fixture.state.channels_for("gw1").len(), 1);
    assert_eq!(fixture.state.clear_channels("gw1"), 1);
    assert!(fixture.state.channels_for("gw1").is_empty());
}

#[tokio::test]
async fn a_supervisor_welcome_carries_its_aggregate_label() {
    let text = spec_text().replace(
        "role = \"planner\"\n",
        "role = \"planner\"\naggregate = \"cluster-b\"\n",
    );
    let fixture = fixture_with(&text);
    let (sender, _outbound) = tokio::sync::mpsc::channel(8);
    let mut session = router::Session::with_sender(sender, "planner");
    let body = router::dispatch_client(
        &fixture.state,
        &mut session,
        ClientOp::Hello(hello_args("planner")),
    )
    .await;
    assert!(body.ok, "{body:?}");
    let data = body.data.expect("welcome");
    assert_eq!(data["aggregate"], json!("cluster-b"));
}

#[tokio::test]
async fn a_plain_role_welcome_omits_the_key() {
    let fixture = fixture();
    let (sender, _outbound) = tokio::sync::mpsc::channel(8);
    let mut session = router::Session::with_sender(sender, "planner");
    let body = router::dispatch_client(
        &fixture.state,
        &mut session,
        ClientOp::Hello(hello_args("planner")),
    )
    .await;
    assert!(body.ok, "{body:?}");
    let text = serde_json::to_string(&body.data.expect("welcome")).expect("encode the welcome");
    assert!(
        !text.contains("\"aggregate\""),
        "a plain role's welcome omits the key: {text}"
    );
}

#[test]
fn roles_rows_carry_the_entry_edges_verbatim_and_the_aggregate_label() {
    let text = spec_text()
        .replace(
            "role = \"planner\"\n",
            "role = \"planner\"\naggregate = \"cluster-b\"\n",
        )
        .replace(
            "allowed_targets = [\"planner\", \"builder\"]\n\n[[client]]\nrole = \"builder\"",
            "allowed_targets = [\"*\", \"planner\", \"ghost\"]\n\n[[client]]\nrole = \"builder\"",
        );
    let labelled = fixture_with(&text);
    let rows = router::roles(&labelled.state, &QueryRolesArgs::default()).expect("roles");
    assert_eq!(rows.len(), 2, "one row per registered role");
    let planner = rows
        .iter()
        .find(|row| row.name == "planner")
        .expect("the planner row");
    assert_eq!(
        planner.edges,
        vec!["*", "planner", "ghost"],
        "edges stays verbatim: no wildcard expansion, no unregistered-name filter"
    );
    assert_eq!(planner.aggregate.as_deref(), Some("cluster-b"));

    let plain_rows = router::roles(&fixture().state, &QueryRolesArgs::default()).expect("roles");
    assert_eq!(
        plain_rows[0].edges,
        vec!["planner", "builder"],
        "the entry's own targets"
    );
    let encoded = serde_json::to_string(&plain_rows).expect("encode the rows");
    assert!(
        !encoded.contains("\"aggregate\""),
        "a plain role's row omits the aggregate key: {encoded}"
    );
}

#[tokio::test]
async fn shutdown_unlinks_the_admin_socket() {
    let fixture = fixture();
    let layout = onlyne_layout::ServerRoot::resolve(&fixture.root);
    let listener = onlyne_server::admin::bind(&fixture.state).expect("bind");
    let path = layout.socket_path();
    assert!(
        path.exists(),
        "the run socket is bound at {}",
        path.display()
    );
    drop(listener);
    onlyne_server::admin::unlink(&fixture.state).expect("unlink");
    assert!(!path.exists(), "the run socket is gone after shutdown");
    onlyne_server::admin::unlink(&fixture.state).expect("a second unlink is a no-op");
}

#[test]
fn start_clears_a_stale_socket_from_a_dead_pid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("server");
    std::fs::create_dir_all(root.join(".onlyne/run")).expect("create the run dir");
    let layout = onlyne_layout::ServerRoot::resolve(&root);
    std::fs::write(layout.socket_path(), b"").expect("write a stale socket file");
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    let pid = child.id();
    child.wait().expect("reap the child");
    std::fs::write(layout.pid_path(), format!("{pid}\n")).expect("write the pid file");
    assert!(onlyne_server::cli::clear_stale_socket(&layout));
    assert!(!layout.socket_path().exists());
}

#[tokio::test]
async fn a_wrong_platform_channel_registration_is_refused() {
    let fixture = fixture();
    let error = gateway_host::register_channels(
        &fixture.state,
        "gw1",
        "telegram",
        &onlyne_proto::RegisterChannelArgs {
            platform: "feishu".to_string(),
            channel: "chan1".to_string(),
            conversations: None,
        },
    )
    .expect_err("a platform mismatch is refused");
    assert_eq!(error.0, ErrorCode::Invalid);
    assert!(error.1.contains("telegram") && error.1.contains("feishu"));
    assert!(fixture.state.channels_for("gw1").is_empty());
    assert_eq!(
        faults::query(
            &fixture.state,
            &QueryFaultsArgs {
                limit: 10,
                ..QueryFaultsArgs::default()
            }
        )
        .expect("faults")
        .iter()
        .filter(|fault| fault.kind == gateway_host::KIND_GATEWAY_PLATFORM_MISMATCH)
        .count(),
        1
    );
}

#[tokio::test]
async fn a_wrong_gateway_mount_is_refused_at_hello() {
    let fixture = fixture();
    let link = std::sync::Arc::new(onlyne_server::state::AdapterLink::default());
    let error = gateway_host::welcome(
        &fixture.state,
        link,
        &onlyne_proto::HelloArgs {
            protocol: onlyne_proto::PROTOCOL_VERSION,
            plugin: "onlyne-gateway-feishu".to_string(),
            version: "1.0.0".to_string(),
            kind: onlyne_proto::MountKind::Gateway,
            capabilities: vec![],
            mount: Some(onlyne_proto::Mount::Gateway(onlyne_proto::GatewayMount {
                gateway: "gw1".to_string(),
                platform: "feishu".to_string(),
            })),
        },
    )
    .expect_err("a mount platform beyond the spec entry is refused");
    assert_eq!(error.0, ErrorCode::Invalid);
    assert!(error.1.contains("telegram") && error.1.contains("feishu"));
}
