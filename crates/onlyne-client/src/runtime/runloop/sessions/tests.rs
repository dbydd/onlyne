use super::{accept_delivery, outcome_loop};
use crate::runtime::intent::{IntentMachine, op_for_intent};
use crate::runtime::runloop::config::{DEFAULT_INTENT_ATTEMPTS, RunState, default_intent_backoff};
use crate::runtime::runloop::link::pending_intent_ops;
use crate::runtime::runloop::test_support::test_state;
use crate::session::dispatch::{self, DispatchState};
use anyhow::Result;
use onlyne_layout::RoleWorkspace;
use onlyne_net::NetError;
use onlyne_proto::{
    Body, Causality, ClientOp, Delivery, MsgKind, Outcome, Principal, Report, ResBody,
    new_envelope, new_task_id,
};
use onlyne_session::{AcpBackend, AcpOptions, SessionLedger};
use onlyne_store::ClientStore;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::Mutex;
use tokio::time::sleep;

/// Real ACP v1 peer used by the client-level delivery test below. The ready
/// marker is written by the test outbox when the Ready report leaves; the
/// child checks it at the instant it receives the prompt, making the causal
/// order observable across the process boundary.
const CLIENT_ACP_FAKE: &str = r##"import json, os, sys

TRACE = sys.argv[1]
READY = sys.argv[2]


def trace(line):
    with open(TRACE, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")
        fh.flush()


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def result(rid, value):
    send({"jsonrpc": "2.0", "id": rid, "result": value})


def failure(rid, code, message):
    send({"jsonrpc": "2.0", "id": rid,
          "error": {"code": code, "message": message}})


def read_message():
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line)


def wait_for(rid):
    while True:
        message = read_message()
        if message is None:
            return None
        if message.get("id") == rid and ("result" in message or "error" in message):
            return message


trace("start pid %d" % os.getpid())
while True:
    message = read_message()
    if message is None:
        trace("eof")
        break
    method = message.get("method")
    rid = message.get("id")
    params = message.get("params") or {}
    if method == "initialize":
        result(rid, {"protocolVersion": 1,
                     "agentInfo": {"name": "onlyne-client-test", "version": "0"},
                     "authMethods": [],
                     "agentCapabilities": {"sessionCapabilities": {"close": {}}}})
    elif method == "session/new":
        result(rid, {"sessionId": "client-e2e-session",
                     "modes": {"currentModeId": "default"},
                     "models": {"currentModelId": "fast"},
                     "configOptions": []})
    elif method == "session/prompt":
        prompt = "".join(block.get("text", "") for block in params.get("prompt") or [])
        session = params.get("sessionId")
        trace("prompt " + prompt)
        trace("ready-before-prompt %s" % os.path.exists(READY))
        send({"jsonrpc": "2.0", "id": "permission-1",
              "method": "session/request_permission",
              "params": {"sessionId": session,
                         "toolCall": {"toolCallId": "call-1", "title": "Edit file",
                                      "kind": "edit", "status": "pending"},
                         "options": [{"optionId": "once", "kind": "allow_once",
                                      "name": "Allow once"},
                                     {"optionId": "no", "kind": "reject_once",
                                      "name": "Reject once"}]}})
        reply = wait_for("permission-1") or {}
        chosen = ((reply.get("result") or {}).get("outcome") or {}).get("optionId", "none")
        trace("permission " + chosen)
        send({"jsonrpc": "2.0", "method": "session/update",
              "params": {"sessionId": session,
                         "sessionUpdate": "agent_message_chunk",
                         "content": {"type": "text", "text": "permission denied\n"}}})
        result(rid, {"stopReason": "refusal"})
    elif method == "session/close":
        trace("close " + str(params.get("sessionId")))
        result(rid, {})
    elif method == "session/cancel":
        trace("cancel")
    elif rid is not None:
        failure(rid, -32601, "unsupported " + str(method))
"##;

#[derive(Clone)]
struct ReadyMarkerOutbox {
    marker: PathBuf,
    frames: Arc<parking_lot::Mutex<Vec<ClientOp>>>,
}

impl crate::session::dispatch::Outbox for ReadyMarkerOutbox {
    fn send(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>> {
        let marker = self.marker.clone();
        let frames = Arc::clone(&self.frames);
        Box::pin(async move {
            if matches!(&op, ClientOp::Report(Report::Ready { .. })) {
                std::fs::write(marker, b"ready").expect("write the ready marker");
            }
            frames.lock().push(op);
            Ok(())
        })
    }

    fn request(
        &self,
        _op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<ResBody, NetError>> + Send + '_>> {
        Box::pin(async { Ok(ResBody::ok(serde_json::Value::Null)) })
    }
}

/// A real ACP child takes a pulled task without an adapter mount, observes
/// the payload only after Ready left, and reports its refusal through the
/// ordinary client fault, settlement, head, and delivery-ack paths.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_acp_delivery_reaches_the_agent_and_settles_through_the_client() {
    let dir = tempdir().expect("ACP client workspace");
    let workspace = RoleWorkspace::resolve(dir.path());
    workspace.bootstrap().expect("bootstrap workspace");
    let script = dir.path().join("onlyne_client_acp_fake_agent.py");
    let trace_path = dir.path().join("agent.trace");
    let ready_marker = dir.path().join("ready.reported");
    std::fs::write(&script, CLIENT_ACP_FAKE).expect("write ACP fake agent");

    let store = ClientStore::open(workspace.client_db_path()).expect("client store");
    store
        .put_prose("planner", "Act as the planner.", "spec-hash")
        .expect("cache role prose");
    let backend = Arc::new(AcpBackend::new(AcpOptions::default()));
    let dispatch = DispatchState::new(
        "planner",
        dir.path(),
        vec![
            "python3".into(),
            "-u".into(),
            script.to_string_lossy().into_owned(),
            trace_path.to_string_lossy().into_owned(),
            ready_marker.to_string_lossy().into_owned(),
        ],
        1,
        backend,
        store.clone(),
    );
    let frames = Arc::new(parking_lot::Mutex::new(Vec::new()));
    dispatch.attach_outbox(Arc::new(ReadyMarkerOutbox {
        marker: ready_marker,
        frames: Arc::clone(&frames),
    }));
    let state = RunState {
        accept_new: dispatch.accept_new(),
        store: store.clone(),
        intents: Arc::new(parking_lot::Mutex::new(IntentMachine::new(
            store.clone(),
            DEFAULT_INTENT_ATTEMPTS,
            default_intent_backoff(),
        ))),
        dispatch,
        welcome: Arc::new(Mutex::new(None)),
        stall_report_secs: 0,
        // The reconnect sweep is not what this pump exercises, and its
        // window would retire sessions this case holds open on purpose.
        reconnect_grace_secs: 0,
    };
    let pump = tokio::spawn(outcome_loop(state.clone()));

    let task_id = new_task_id();
    let delivery = Delivery {
        msg_id: "msg-acp-client-e2e".into(),
        envelope: Box::new(
            new_envelope(
                MsgKind::Task,
                Principal::role("sender"),
                Principal::role("planner"),
                Body::text("repair the failing widget"),
                Some(Causality::root(task_id.clone())),
            )
            .expect("task envelope"),
        ),
    };
    accept_delivery(&state, &delivery).await;

    let settled = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            // The task's own record is the account of the verdict; the session
            // row beside it says what the session proved about its agent.
            if let Some(record) = store.task(&task_id).expect("read task record")
                && record.task_state == onlyne_session::TaskState::Failed
            {
                break store.get_session(&task_id).expect("read session").unwrap();
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let row = match settled {
        Ok(row) => row,
        Err(error) => {
            crate::session::dispatch::close_all(
                &state.dispatch,
                onlyne_session::CloseReason::Shutdown,
                Duration::from_secs(1),
            );
            pump.abort();
            panic!(
                "ACP task did not settle: {error}; trace={:?}",
                std::fs::read_to_string(&trace_path)
            );
        }
    };

    // `on_out` removes the slot and asks the ACP backend to close. Wait for
    // the child to observe EOF before asserting, so even a failed assertion
    // below cannot leave the fake agent behind.
    let trace = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let trace = std::fs::read_to_string(&trace_path).unwrap_or_default();
            if trace.contains("eof") {
                break trace;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the ACP fake agent exits after settlement");
    pump.abort();
    let _ = pump.await;

    assert!(
        trace.contains("prompt repair the failing widget"),
        "the task payload crossed the real ACP pipe: {trace}"
    );
    assert!(
        trace.contains("ready-before-prompt True"),
        "Ready must leave before the agent sees the payload: {trace}"
    );
    assert!(trace.contains("permission no"), "{trace}");
    assert!(trace.contains("close client-e2e-session"), "{trace}");
    assert!(!state.dispatch.has_mounted_adapter());
    assert_eq!(state.dispatch.session_count(), 0);

    let record = store.task(&task_id).expect("read task record");
    assert_eq!(
        record.map(|record| record.task_state),
        Some(onlyne_session::TaskState::Failed),
        "the ACP refusal settled the task failed, in the task's own record"
    );
    let observed: serde_json::Value =
        serde_json::from_str(&row.observed_json).expect("stored observation JSON");
    assert!(
        observed.get("outcome").is_none(),
        "the session tuple reports no verdict: {observed}"
    );
    assert_eq!(
        store
            .out_head(&task_id)
            .expect("read completion head")
            .as_deref(),
        Some("permission denied")
    );
    let faults = store.list_faults(&task_id).expect("read ACP faults");
    assert_eq!(
        faults
            .iter()
            .map(|fault| fault.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["permission", "acp"],
        "{faults:?}"
    );
    assert!(faults[0].reason.contains("permission ask(s) refused"));
    assert!(faults[1].reason.contains("agent stopped the turn: refusal"));

    assert!(
        frames.lock().iter().any(|op| {
            matches!(
                op,
                ClientOp::Report(Report::Ready { task_id: ready, .. })
                    if ready == &task_id
            )
        }),
        "the existing Ready report path was used"
    );
    let intents = store
        .due_intents(chrono::Utc::now() + chrono::Duration::seconds(1), 100)
        .expect("read durable intents");
    assert!(
        intents.iter().any(|row| {
            matches!(
                serde_json::from_value::<ClientOp>(row.env_json.clone()),
                Ok(ClientOp::Ack(ack)) if ack.msg_id == "msg-acp-client-e2e" && ack.accepted
            )
        }),
        "the delivery ack was queued through the existing settlement path: {intents:?}"
    );
}

#[tokio::test]
async fn startup_residual_report_uses_durable_report_path() {
    let (state, store) = test_state(1, Vec::new());
    let convergence = crate::session::stale::Convergence {
        task_id: "task-dead".into(),
    };
    dispatch::send_frame(&state.dispatch, ClientOp::Report(convergence.report()))
        .await
        .expect("report queues without a link");
    let rows = store.flush_order().expect("pending intents");
    let ops = rows
        .iter()
        .map(op_for_intent)
        .collect::<Result<Vec<_>>>()
        .unwrap();
    assert!(ops.iter().any(|op| matches!(
        op,
        ClientOp::Report(Report::Complete {
            task_id,
            outcome: Outcome::Failed,
            head: Some(reason),
            ..
        }) if task_id == "task-dead" && reason == crate::session::stale::SESSION_DEAD
    )));
}

/// A row re-offered for a task this role already finished is acked and runs
/// nowhere. The server requeues an unacknowledged row after a link flap, and
/// a completion still in flight when the link dropped lands after that
/// requeue, so the same task arrives twice. Untreated, the second delivery
/// read as new work: the dispatcher staged its payload on whichever session
/// sat idle, which is one chain's task running inside another conversation
/// with a second answer aimed at the ledger row the first answer settled.
/// The row is acked rather than left in flight, because an unacked row is
/// offered again forever.
#[tokio::test]
async fn a_redelivered_finished_task_is_acked_and_runs_nowhere() {
    let (state, _store) = test_state(2, vec!["echo".into()]);
    let task_id = new_task_id();
    let delivery = |msg_id: &str| Delivery {
        msg_id: msg_id.into(),
        envelope: Box::new(
            new_envelope(
                MsgKind::Task,
                Principal::role("sender"),
                Principal::role("planner"),
                Body::text("work"),
                Some(Causality::root(task_id.clone())),
            )
            .expect("task envelope"),
        ),
    };

    accept_delivery(&state, &delivery("msg-first")).await;
    assert!(
        state.dispatch.hello_live_tasks().contains(&task_id),
        "the first delivery takes a session for the task"
    );

    crate::session::dispatch::on_out(
        &state.dispatch,
        &task_id,
        Outcome::Done,
        Some("done".into()),
        None,
        &[],
    )
    .await
    .expect("the completion files");
    assert!(
        state.dispatch.task_completed_here(&task_id),
        "the finished task is readable as finished in this role's store"
    );

    accept_delivery(&state, &delivery("msg-again")).await;

    // `accept_new` is false by now — `on_out` queued its report with no link
    // attached and `send_frame` drops the flag while outbound work waits in
    // the intent table — so this ack also proves the guard sits ahead of
    // that gate: a finished row is answered whatever the link state.
    assert!(
        !state.dispatch.hello_live_tasks().contains(&task_id),
        "the redelivery stages no session on the role's idle slot"
    );
    let acked = pending_intent_ops(&state)
        .expect("pending intents")
        .into_iter()
        .find(|op| matches!(op, ClientOp::Ack(args) if args.msg_id == "msg-again"));
    match acked {
        Some(ClientOp::Ack(args)) => assert!(
            args.accepted,
            "a row for work this role did is refused on the ledger: {args:?}"
        ),
        other => panic!("the redelivered row answers with an ack, got {other:?}"),
    }
}

/// A task whose session died mid-flight stays open for the retry the server
/// means. Killing is the difference this guard turns on: `requeue`,
/// `repair_retry`, and `control retry` all re-offer exactly such a row, and
/// closing the door on them would strand work the role never finished.
#[tokio::test]
async fn a_task_ended_without_a_completion_stays_eligible_for_its_retry() {
    let (state, _store) = test_state(2, vec!["echo".into()]);
    let task_id = new_task_id();
    accept_delivery(
        &state,
        &Delivery {
            msg_id: "msg-lost".into(),
            envelope: Box::new(
                new_envelope(
                    MsgKind::Task,
                    Principal::role("sender"),
                    Principal::role("planner"),
                    Body::text("work"),
                    Some(Causality::root(task_id.clone())),
                )
                .expect("task envelope"),
            ),
        },
    )
    .await;

    crate::session::dispatch::on_out(
        &state.dispatch,
        &task_id,
        Outcome::Failed,
        Some("crashed".into()),
        None,
        &[],
    )
    .await
    .expect("the failure files");
    assert!(
        !state.dispatch.task_completed_here(&task_id),
        "a failed turn is not a finished task"
    );

    // The retry is asserted at the dispatcher, one step below
    // `accept_delivery`: `on_out` above queued its report with no link
    // attached, and `send_frame` drops `accept_new` while the outbound work
    // waits in the intent table (§6 line 289). That gate is older than this
    // guard and belongs to the reconnect, so it would answer here for a
    // reason unrelated to the one under test.
    let retried = new_envelope(
        MsgKind::Task,
        Principal::role("sender"),
        Principal::role("planner"),
        Body::text("work again"),
        Some(Causality::root(task_id.clone())),
    )
    .expect("task envelope");
    crate::session::dispatch::dispatch(&state.dispatch, &retried)
        .expect("a failed task is retryable");

    assert!(
        state.dispatch.hello_live_tasks().contains(&task_id),
        "the retried task takes a session again"
    );
}
