//! Black-box scenario test suite: spawn real server/client/agent processes,
//! drive them through the admin and adapter sockets, and read what they leave in
//! the ledger and the session table.
//!
//! The v2 contract is what these scenarios exercise (`AGENTS.md`, `docs/v2-PLAN.md`).
//! The suite is a fake-backend one — `ONLYNE_BACKEND=fake` per child — so each case
//! runs real binaries without a terminal, a pane manager, or a model.
//!
//! A fixture's script models a plugin, and a plugin that never reports a turn is a
//! plugin whose completions the host refuses (`settle_without_turn`), so every script
//! that completes reports a heartbeat first: see the capability note on scenario 2.

use onlyne_proto::{LedgerQuery, LedgerState, Lifecycle, QuerySessionsArgs};
use onlyne_testkit::harness::Cluster;
use onlyne_testkit::{AgentScript, ScriptHello};
use serde_json::json;
use std::time::Duration;

/// Default prose passed through onlyne-client init --prose.
const E2E_PROSE: &str = "v1 smoke prose";

/// Scenario 1: Delivery loop (send, claim, complete)
///
/// Asserts: task reaches in_flight then acked, ledger state transitions in order,
/// session lifecycle reaches working then exited with outcome done, echo-complete.json
/// script received correct assign.prose.
///
/// Supersedes: crates/onlyne-testkit/e2e/local-task.sh
#[tokio::test]
async fn scenario_01_delivery_loop() {
    let spec = r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#;

    let cluster = Cluster::start(spec).await.expect("start cluster");
    let planner_ws = cluster
        .register_role(
            "planner",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]"#,
            ),
        )
        .await
        .expect("register planner");

    cluster
        .start_client(&planner_ws)
        .await
        .expect("start client");

    let script = AgentScript {
        hello: ScriptHello {
            capabilities: vec![
                onlyne_proto::Capability::Register,
                onlyne_proto::Capability::Report,
                onlyne_proto::Capability::Inject,
                onlyne_proto::Capability::Recycle,
            ],
        },
        steps: vec![
            json!({"wait_assign": true}),
            json!({"assert_prose_equals": E2E_PROSE}),
            json!({"report": "ready"}),
            json!({"report": "heartbeat"}),
            json!({"complete": {"outcome": "done", "head_from": "assign_body"}}),
            json!({"echo_prose_to": "prose.log"}),
        ],
        repeat: false,
    };
    cluster
        .start_fake_agent(&planner_ws, &script)
        .await
        .expect("start fake agent");

    cluster
        .wait_role_online("planner")
        .await
        .expect("planner online");

    let send_result = cluster
        .admin_send("planner", "planner", "hello v1")
        .await
        .expect("admin send");
    let task_id = send_result["task"]
        .as_str()
        .expect("task id in send result");
    assert_eq!(send_result["state"], "in_flight");

    // Poll until acked
    let acked_rows = cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(task_id.to_string()),
                ..Default::default()
            },
            |rows| rows.iter().any(|r| r.state == LedgerState::Acked),
            Duration::from_secs(60),
        )
        .await
        .expect("poll ledger for acked");

    assert!(acked_rows.iter().any(|r| r.state == LedgerState::Acked));
    assert!(acked_rows.iter().any(|r| {
        r.out_head
            .as_ref()
            .map(|s| s.contains("hello v1"))
            .unwrap_or(false)
    }));

    // Poll until session exited with outcome done
    let exited_sessions = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(task_id.to_string()),
                ..Default::default()
            },
            |rows| rows.iter().any(|r| r.public_lifecycle == Lifecycle::Exited),
            Duration::from_secs(60),
        )
        .await
        .expect("poll sessions for exited");

    assert_eq!(exited_sessions.len(), 1);
    assert_eq!(exited_sessions[0].public_lifecycle, Lifecycle::Exited);
    assert_eq!(
        exited_sessions[0].outcome.as_ref().map(|s| s.as_str()),
        Some("done")
    );

    // Verify prose.log was written
    let prose_log = planner_ws.join("prose.log");
    assert!(prose_log.exists(), "prose.log must exist");
    let prose_content = tokio::fs::read_to_string(&prose_log)
        .await
        .expect("read prose.log");
    assert_eq!(prose_content.trim(), E2E_PROSE);

    println!("✓ Scenario 1: delivery loop [send→in_flight→acked, session working→exited done]");
}

/// Scenario 2: Handoff chain (family fields at every hop, and the child settles)
///
/// Asserts: alpha's task is handed to beta through the adapter protocol's own
/// `handoff` op; the child carries the family id, one hop deeper, and names the
/// root as its parent; beta's completion settles the child.
///
/// The hop budget is not asserted here, and no layer refuses a handoff for
/// spending it: `Causality::child_of` carries `hop_budget` to the child, and
/// whether a role keeps the task or passes it on is the plugin's own reading of
/// that number — which is what this fixture's own `max_hop` gate models. The
/// client-side check the v2 plan puts on the tool surface ("约束在 client 统一执行",
/// docs/v2-PLAN.md) is not in this tree yet.
///
/// A script whose first step is `wait_assign` must declare `inject`: that
/// capability is what makes a plugin reachable by `assign`, and without it the
/// host delivers the payload through the plugin's stdin instead
/// (`crates/onlyne-adapter/PROTOCOL.md`, "Mounts and capabilities").
///
/// Supersedes: Part of docs/v1-PLAN.md §8 handoff acceptance, e2e running-lights.sh
#[tokio::test]
async fn scenario_02_handoff_chain() {
    let spec = r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#;

    let cluster = Cluster::start(spec).await.expect("cluster start");
    let alpha_ws = cluster
        .register_role(
            "alpha",
            "alpha prose",
            Some(
                r#"allowed_senders = ["*", "alpha"]
allowed_targets = ["*"]"#,
            ),
        )
        .await
        .expect("register alpha");
    let beta_ws = cluster
        .register_role(
            "beta",
            "beta prose",
            Some(
                r#"allowed_senders = ["*", "beta"]
allowed_targets = ["*"]"#,
            ),
        )
        .await
        .expect("register beta");

    cluster
        .start_client(&alpha_ws)
        .await
        .expect("start alpha client");
    cluster
        .start_client(&beta_ws)
        .await
        .expect("start beta client");

    // Alpha hands off to beta after 1 hop
    let alpha_script = AgentScript {
        hello: ScriptHello {
            // `inject` is what makes a plugin reachable by `assign`
            // (PROTOCOL.md, "Mounts and capabilities"); the pi plugin declares
            // it, and a script whose first step is `wait_assign` has to as
            // well, or the host takes the stdin route and the assign never
            // comes.
            capabilities: vec![
                onlyne_proto::Capability::Register,
                onlyne_proto::Capability::Report,
                onlyne_proto::Capability::Inject,
            ],
        },
        steps: vec![
            json!({"wait_assign": true}),
            json!({"report": "ready"}),
            json!({"handoff": {"to": "beta", "text": "hop {next_hop}", "max_hop": 1}}),
        ],
        repeat: false,
    };

    // Beta completes
    let beta_script = AgentScript {
        hello: ScriptHello {
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
            json!({"complete": {"outcome": "done", "head": "beta done"}}),
        ],
        repeat: false,
    };

    cluster
        .start_fake_agent(&alpha_ws, &alpha_script)
        .await
        .expect("start alpha agent");
    cluster
        .start_fake_agent(&beta_ws, &beta_script)
        .await
        .expect("start beta agent");

    cluster
        .wait_role_online("alpha")
        .await
        .expect("alpha online");
    cluster.wait_role_online("beta").await.expect("beta online");

    let send_result = cluster
        .admin_send("alpha", "alpha", "start handoff chain")
        .await
        .expect("admin send");
    let root_task = send_result["task"].as_str().expect("root task id");

    // Poll until the family's root and its child are both visible. The query is
    // unfiltered on purpose: `LedgerQuery` has no family filter, and filtering by
    // the root's own task id could only ever return that one task's rows, never
    // the child's. The family is selected on the rows instead.
    let family_rows = cluster
        .poll_ledger(
            LedgerQuery::default(),
            |rows| {
                rows.iter()
                    .filter(|row| row.family.as_deref() == Some(root_task))
                    .count()
                    >= 2
            },
            Duration::from_secs(60),
        )
        .await
        .expect("poll ledger for family");
    let all_rows: Vec<_> = family_rows
        .into_iter()
        .filter(|row| row.family.as_deref() == Some(root_task))
        .collect();

    // Check family fields: every row in the family carries the same family id
    let families: std::collections::HashSet<_> =
        all_rows.iter().filter_map(|r| r.family.as_ref()).collect();
    assert_eq!(families.len(), 1, "all rows must share one family");
    let family_id = families.into_iter().next().unwrap();
    assert_eq!(family_id, root_task, "family id must equal root task");

    // Check hop progression: root has hop 0, child has hop 1. A hop owns two
    // rows once the child settles — the task itself and the completion receipt
    // its answer travelled on, which sits at the depth of the task it answers
    // and is no link in the chain — so the chain's own rows are the tasks.
    let hops: Vec<_> = all_rows.iter().map(|r| r.hop).collect();
    assert!(hops.contains(&0), "root task has hop 0");
    assert!(hops.contains(&1), "child task has hop 1");

    // Check parent_task linkage
    let child_row = all_rows
        .iter()
        .find(|r| r.kind == onlyne_proto::MsgKind::Task && r.hop == 1)
        .expect("child row");
    assert_eq!(
        child_row.parent_task.as_deref(),
        Some(root_task),
        "child parent_task must link to root"
    );

    // The chain's end: beta's completion settles the child it was handed, and its
    // answer travels back to alpha on a completion row of the same family, one
    // hop down. Note which column holds what: a task row's `out_head` is the
    // preview the server keeps of the *delivered body* (`head_preview` in
    // `onlyne-store/src/server.rs`), so the child's own row carries the relay's
    // text, while the head beta reported rides the receipt.
    let settled = cluster
        .poll_ledger(
            LedgerQuery::default(),
            |rows| {
                rows.iter().any(|row| {
                    row.kind == onlyne_proto::MsgKind::Task
                        && row.hop == 1
                        && row.state == LedgerState::Acked
                })
            },
            Duration::from_secs(60),
        )
        .await
        .expect("poll ledger for the settled child");
    let child = settled
        .iter()
        .find(|row| row.kind == onlyne_proto::MsgKind::Task && row.hop == 1)
        .expect("the child row");
    assert_eq!(child.family.as_deref(), Some(root_task));
    assert_eq!(
        child.out_head.as_deref(),
        Some("handoff: hop 1"),
        "the ledger keeps the relayed body's preview, prefix and all"
    );

    let receipts: Vec<&onlyne_proto::LedgerEntry> = settled
        .iter()
        .filter(|row| row.kind == onlyne_proto::MsgKind::Completion)
        .collect();
    let receipt = receipts
        .iter()
        .find(|row| row.task.as_deref() == child.task.as_deref())
        .unwrap_or_else(|| {
            panic!("no completion receipt for the child among {receipts:#?}");
        });
    assert_eq!(
        receipt.to.role_name(),
        Some("alpha"),
        "the receipt answers the role that handed the child on"
    );
    assert_eq!(
        receipt.out_head.as_deref(),
        Some("beta done"),
        "the receipt carries the head beta reported"
    );
    assert_eq!(
        receipt.hop, 1,
        "a receipt sits at the depth of the task it answers"
    );

    println!(
        "✓ Scenario 2: handoff chain [family fields correct, hop progression 0→1, parent_task link, child acked]"
    );
    println!(
        "  Note: the hop budget rides along in causality; whether a role keeps the task is its own reading of it"
    );
}

/// Scenario 3: ACL denial (denied send is refused, leaves no ledger row)
///
/// Asserts: send to denied target returns acl_denied with field to.role, ledger holds
/// zero rows, sender's intent table holds zero rows.
///
/// Supersedes: crates/onlyne-testkit/e2e/acl-reject.sh
#[tokio::test]
async fn scenario_03_acl_denial() {
    let spec = r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#;

    let cluster = Cluster::start(spec).await.expect("cluster start");
    let builder_ws = cluster
        .register_role(
            "builder",
            "builder prose",
            Some(
                r#"allowed_senders = ["*", "builder"]
allowed_targets = ["builder"]"#,
            ),
        )
        .await
        .expect("register builder");
    let _reviewer_ws = cluster
        .register_role(
            "reviewer",
            "reviewer prose",
            Some(
                r#"allowed_senders = ["*", "reviewer"]
allowed_targets = ["reviewer"]"#,
            ),
        )
        .await
        .expect("register reviewer");

    cluster
        .start_client(&builder_ws)
        .await
        .expect("start builder client");

    let script = AgentScript {
        hello: ScriptHello {
            // `wait_assign` needs `inject`: see scenario 2's note.
            capabilities: vec![
                onlyne_proto::Capability::Register,
                onlyne_proto::Capability::Report,
                onlyne_proto::Capability::Inject,
            ],
        },
        steps: vec![
            json!({"wait_assign": true}),
            json!({"report": "heartbeat"}),
            json!({"complete": {"outcome": "done", "head": "builder done"}}),
        ],
        repeat: false,
    };

    cluster
        .start_fake_agent(&builder_ws, &script)
        .await
        .expect("start builder agent");

    cluster
        .wait_role_online("builder")
        .await
        .expect("builder online");

    // Denied send: builder -> reviewer (not in builder's allowed_targets)
    let denied_result = cluster.admin_send("builder", "reviewer", "x").await;
    assert!(denied_result.is_err(), "send must be denied");

    // Ledger must be empty
    let ledger = cluster
        .query_ledger(LedgerQuery::default())
        .await
        .expect("query ledger");
    assert_eq!(ledger.len(), 0, "ledger must hold zero rows after denial");

    // Positive control: builder -> builder is allowed
    let allowed_result = cluster
        .admin_send("builder", "builder", "self edge")
        .await
        .expect("self send allowed");
    assert_eq!(allowed_result["state"], "in_flight");

    let ledger_after = cluster
        .query_ledger(LedgerQuery::default())
        .await
        .expect("query ledger after allowed");
    assert_eq!(
        ledger_after.len(),
        1,
        "ledger must hold exactly one row after allowed send"
    );

    println!(
        "✓ Scenario 3: ACL denial [denied send refused, no ledger row; allowed send creates row]"
    );
}

/// Scenario 4: Idempotency (same op_id same content returns same receipt; different content returns conflict)
///
/// Asserts: first send succeeds, second send with same op_id and body returns
/// duplicate with the original receipt in data, third send with same op_id but
/// different body returns conflict error.
///
/// Supersedes: crates/onlyne-testkit/e2e/idempotency.sh
#[tokio::test]
async fn scenario_04_idempotency() {
    let spec = r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#;

    let cluster = Cluster::start(spec).await.expect("start cluster");
    let _planner_ws = cluster
        .register_role(
            "planner",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]"#,
            ),
        )
        .await
        .expect("register planner");

    // First send with pinned op_id
    let op_id = "o-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let envelope1 = onlyne_proto::new_envelope(
        onlyne_proto::MsgKind::Task,
        onlyne_proto::Principal::role("planner"),
        onlyne_proto::Principal::role("planner"),
        onlyne_proto::Body::text("hello idem".to_string()),
        Some(onlyne_proto::Causality::root(onlyne_proto::new_task_id())),
    )
    .expect("build envelope");

    // Manually set op_id (envelope built with new_envelope auto-generates one)
    let mut env1_with_pinned = envelope1.clone();
    env1_with_pinned.op_id = Some(op_id.to_string());

    let socket = cluster.admin_socket().expect("resolve admin socket");
    let send1_op = onlyne_proto::AdminOp::Send(onlyne_proto::AdminSend {
        from: "planner".to_string(),
        envelope: Box::new(env1_with_pinned.clone()),
    });

    // Helper to send via admin socket
    let send_pinned = |op: onlyne_proto::AdminOp| async {
        let mut stream = onlyne_wire::socket::connect_local(&socket).await?;
        #[derive(Debug, serde::Serialize)]
        #[serde(rename_all = "snake_case", tag = "f")]
        enum AdminFrame {
            Req {
                id: String,
                #[serde(flatten)]
                op: onlyne_proto::AdminOp,
            },
        }
        let req = AdminFrame::Req {
            id: onlyne_proto::new_id(),
            op,
        };
        onlyne_wire::write_frame(&mut stream, &req).await?;
        let frame: onlyne_proto::Frame<onlyne_proto::AdminOp> =
            onlyne_wire::read_frame(&mut stream)
                .await?
                .ok_or_else(|| anyhow::anyhow!("socket closed"))?;
        match frame {
            onlyne_proto::Frame::Res { body, .. } => Ok(body),
            _ => anyhow::bail!("unexpected frame"),
        }
    };

    let body1 = send_pinned(send1_op).await.expect("first send");
    assert!(body1.ok, "first send must succeed");
    let receipt1 = body1.data.clone().expect("first send has data");

    // Second send: same op_id, same body
    let send2_op = onlyne_proto::AdminOp::Send(onlyne_proto::AdminSend {
        from: "planner".to_string(),
        envelope: Box::new(env1_with_pinned.clone()),
    });
    let body2 = send_pinned(send2_op).await.expect("second send");
    assert!(!body2.ok, "duplicate send must return ok=false");
    assert_eq!(
        body2.error.as_ref().unwrap().code,
        onlyne_proto::ErrorCode::Duplicate
    );
    let receipt2 = body2.data.clone().expect("duplicate has data");
    assert_eq!(receipt1, receipt2, "duplicate must return original receipt");

    // Third send: same op_id, different body
    let mut env_changed = env1_with_pinned.clone();
    env_changed.body = onlyne_proto::Body::text("hello changed".to_string());
    let send3_op = onlyne_proto::AdminOp::Send(onlyne_proto::AdminSend {
        from: "planner".to_string(),
        envelope: Box::new(env_changed),
    });
    let body3 = send_pinned(send3_op).await.expect("third send");
    assert!(!body3.ok, "conflict send must return ok=false");
    assert_eq!(
        body3.error.as_ref().unwrap().code,
        onlyne_proto::ErrorCode::Conflict
    );
    assert_eq!(
        body3.error.as_ref().unwrap().message,
        "op_id conflict: request differs from durable receipt"
    );

    // Ledger must hold exactly one row
    let ledger = cluster
        .query_ledger(LedgerQuery {
            role: Some("planner".to_string()),
            ..Default::default()
        })
        .await
        .expect("query ledger");
    assert_eq!(
        ledger.len(),
        1,
        "ledger must hold exactly one row across all attempts"
    );

    println!(
        "✓ Scenario 4: idempotency [duplicate returns original receipt, conflict refused, one ledger row]"
    );
}

/// Scenario 5: Disconnect recovery (a client that is gone when work arrives
/// serves it once, in the order it was sent, when it comes back)
///
/// The plan's disconnect rule (docs/v2-PLAN.md §6): a delivery to a role whose
/// client is not connected is not lost and not refused — the row stays in flight
/// on the server, and the client's own store carries what it already held. The
/// client is taken away *between* tasks rather than mid-frame: a kill timed
/// against a frame in flight is a race this harness cannot make deterministic,
/// and what the case is for is the queue's fate, not the framing.
///
/// Asserts: the three tasks sent while the client is gone wait `queued` and
/// serve no session; when the client returns each is served exactly once, the
/// three settle in the order they were sent, and each task owns exactly one
/// session row.
///
/// Supersedes: crates/onlyne-testkit/e2e/reconnect-requeue.sh
#[tokio::test]
async fn scenario_05_disconnect_recovery() {
    let spec = r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#;

    let cluster = Cluster::start(spec).await.expect("start cluster");
    let planner_ws = cluster
        .register_role(
            "planner",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]"#,
            ),
        )
        .await
        .expect("register planner");

    // One script per task: `onlyne-client init` writes `max_sessions = 1` into
    // the role's fragment, so this role runs one session at a time and each
    // queued row is staged only once the session before it has retired. A
    // plugin serves the one session it is bound to, which is why the case mounts
    // one agent per task.
    let serve_one = || AgentScript {
        hello: ScriptHello {
            // `wait_assign` needs `inject`: see scenario 2's note.
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
            json!({"complete": {"outcome": "done", "head_from": "assign_body"}}),
        ],
        repeat: false,
    };

    cluster
        .start_client(&planner_ws)
        .await
        .expect("start client");
    cluster
        .start_fake_agent(&planner_ws, &serve_one())
        .await
        .expect("start agent");
    cluster
        .wait_role_online("planner")
        .await
        .expect("planner online");

    // Warm-up: the role is known to serve work before the drop, so a later
    // failure is the drop's and not a cluster that never worked.
    let warm_up = cluster
        .admin_send("planner", "planner", "before the drop")
        .await
        .expect("send the warm-up task");
    let warm_up_task = warm_up["task"].as_str().expect("task id").to_string();
    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(warm_up_task),
                ..Default::default()
            },
            |rows| rows.iter().any(|r| r.state == LedgerState::Acked),
            Duration::from_secs(60),
        )
        .await
        .expect("the warm-up task must be acked");

    cluster
        .kill_client(&planner_ws)
        .await
        .expect("kill the client");

    let missed = ["first while gone", "second while gone", "third while gone"];
    let mut queued = Vec::new();
    for text in missed {
        let sent = cluster
            .admin_send("planner", "planner", text)
            .await
            .expect("send while the client is down");
        assert_eq!(
            sent["state"], "queued",
            "work for a role whose client is gone waits in the queue, neither refused nor \
             handed to a session that does not exist: {sent}"
        );
        queued.push(sent["task"].as_str().expect("task id").to_string());
    }

    // Queued means unattempted: nothing has a session while the local half is
    // gone, because a session is a fact only the client can report.
    let before_return = cluster
        .query_sessions(QuerySessionsArgs::default())
        .await
        .expect("query sessions before the client returns");
    for task in &queued {
        assert!(
            !before_return
                .iter()
                .any(|row| row.task_id.as_deref() == Some(task)),
            "task {task} has no session before its client returns: {before_return:?}"
        );
    }

    // The client comes back in the same workspace, so it resumes from the store
    // it left behind rather than from nothing.
    cluster
        .start_client(&planner_ws)
        .await
        .expect("restart the client");
    cluster
        .wait_role_online("planner")
        .await
        .expect("planner online again");

    // All three agents are mounted before the queue drains, so which agent takes
    // which row is the queue's decision and not the case's: the role stages the
    // rows in the order the server offers them, and the parks are served oldest
    // first.
    for _ in 0..queued.len() {
        cluster
            .start_fake_agent(&planner_ws, &serve_one())
            .await
            .expect("start an agent for the queue");
    }

    let drained = cluster
        .poll_ledger(
            LedgerQuery::default(),
            |rows| {
                queued.iter().all(|task| {
                    rows.iter().any(|row| {
                        row.kind == onlyne_proto::MsgKind::Task
                            && row.task.as_deref() == Some(task.as_str())
                            && row.state == LedgerState::Acked
                    })
                })
            },
            Duration::from_secs(120),
        )
        .await
        .expect("every queued task must settle");

    // Order kept: the acks follow the send order. The stamps are read off the
    // same ledger, so one format compares as one order.
    let acked_at: Vec<String> = queued
        .iter()
        .map(|task| {
            drained
                .iter()
                .find(|row| {
                    row.kind == onlyne_proto::MsgKind::Task
                        && row.task.as_deref() == Some(task.as_str())
                })
                .and_then(|row| row.acked_at.as_ref())
                .map(|stamp| stamp.to_rfc3339())
                .unwrap_or_else(|| panic!("task {task} settled without a stamp: {drained:?}"))
        })
        .collect();
    assert!(
        acked_at.windows(2).all(|pair| pair[0] <= pair[1]),
        "the queue drains in the order it was filled: {acked_at:?}"
    );

    // Served once: one session row per task, and no second row for any of them.
    let served = cluster
        .query_sessions(QuerySessionsArgs::default())
        .await
        .expect("query sessions after the queue drained");
    for task in &queued {
        let count = served
            .iter()
            .filter(|row| row.task_id.as_deref() == Some(task))
            .count();
        assert_eq!(
            count, 1,
            "task {task} ran in exactly one session, not {count}: {served:?}"
        );
    }

    println!(
        "✓ Scenario 5: disconnect recovery [{} queued rows served once, in order]",
        queued.len()
    );
}

/// Scenario 6: Server restart (the durable half survives the process)
///
/// The server's truth is a file: `spec.toml` names the roles, the key material
/// is on disk, and the ledger and session rows live in the server's store. A
/// daemon that is killed and started again on the same root must therefore come
/// back as the same cluster — the settled work still reads settled, the roles
/// are registered again from the spec, and the client that was connected before
/// the gap reconnects and serves the next task.
///
/// What this scenario does not assert is event-stream continuity: a subscriber
/// resuming from a cursor with no gap is a property of the run-events feed, and
/// this harness observes the admin surface (ledger, sessions, roles) only. The
/// assertions below are the durable half of the case, not a stand-in for the
/// other.
#[tokio::test]
async fn scenario_06_server_restart() {
    let spec = r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#;

    let cluster = Cluster::start(spec).await.expect("start cluster");
    let planner_ws = cluster
        .register_role(
            "planner",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]"#,
            ),
        )
        .await
        .expect("register planner");

    cluster
        .start_client(&planner_ws)
        .await
        .expect("start client");

    let script = AgentScript {
        hello: ScriptHello {
            // `wait_assign` needs `inject`: see scenario 2's note.
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
            json!({"complete": {"outcome": "done", "head_from": "assign_body"}}),
        ],
        repeat: false,
    };
    cluster
        .start_fake_agent(&planner_ws, &script)
        .await
        .expect("start agent");

    cluster
        .wait_role_online("planner")
        .await
        .expect("planner online");

    let before = cluster
        .admin_send("planner", "planner", "settled before the restart")
        .await
        .expect("send before the restart");
    let first_task = before["task"].as_str().expect("task id").to_string();
    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(first_task.clone()),
                ..Default::default()
            },
            |rows| rows.iter().any(|r| r.state == LedgerState::Acked),
            Duration::from_secs(60),
        )
        .await
        .expect("the first task must be acked before the restart");

    cluster.restart_server().await.expect("restart the server");

    // The ledger is the server's durable record, so what the first process
    // wrote is what the second one reads. A settled task leaves two rows: the
    // task itself and the completion receipt its answer travelled on.
    let survived = cluster
        .query_ledger(LedgerQuery {
            task: Some(first_task.clone()),
            ..Default::default()
        })
        .await
        .expect("query the ledger after the restart");
    let task_rows: Vec<_> = survived
        .iter()
        .filter(|row| row.kind == onlyne_proto::MsgKind::Task)
        .collect();
    assert_eq!(
        task_rows.len(),
        1,
        "exactly the one task row the first process wrote: {survived:?}"
    );
    let settled = task_rows[0];
    assert_eq!(
        settled.state,
        LedgerState::Acked,
        "a settled row stays settled across the restart"
    );
    assert!(
        settled
            .out_head
            .as_ref()
            .is_some_and(|head| head.contains("settled before the restart")),
        "the task row still carries the body preview the first process wrote: {:?}",
        settled.out_head
    );
    let receipt = survived
        .iter()
        .find(|row| row.kind == onlyne_proto::MsgKind::Completion)
        .unwrap_or_else(|| panic!("the completion receipt must survive too: {survived:?}"));
    assert!(
        receipt
            .out_head
            .as_ref()
            .is_some_and(|head| head.contains("settled before the restart")),
        "the receipt still carries the answer the first process settled with: {:?}",
        receipt.out_head
    );

    // The client holds the link, so the cluster needs no help coming back; the
    // role has to read online against the *new* process before its work resumes.
    cluster
        .wait_role_online("planner")
        .await
        .expect("planner online after the restart");

    // Work that arrives after the restart is dispatched as it was before it. The
    // agent that served the first task is bound to the session that retired with
    // it, so the role mounts a plugin for the new one.
    let after_script = AgentScript {
        hello: ScriptHello {
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
            json!({"complete": {"outcome": "done", "head_from": "assign_body"}}),
        ],
        repeat: false,
    };
    cluster
        .start_fake_agent(&planner_ws, &after_script)
        .await
        .expect("start the agent for the task after the restart");

    let after = cluster
        .admin_send("planner", "planner", "settled after the restart")
        .await
        .expect("send after the restart");
    let second_task = after["task"].as_str().expect("task id").to_string();
    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(second_task.clone()),
                ..Default::default()
            },
            |rows| rows.iter().any(|r| r.state == LedgerState::Acked),
            Duration::from_secs(60),
        )
        .await
        .expect("the second task must be acked after the restart");

    println!(
        "✓ Scenario 6: server restart [settled row survived, role re-registered, new work acked]"
    );
}

/// Scenario 7: Large frames interleaved (the link stays up under a large frame,
/// heartbeats, and outbound frames)
///
/// This is the plan's acceptance line "大帧交错：大帧、心跳与出站帧交错时连接不断"
/// (v2-PLAN.md, verification table) and the regression guard for the cancel-safety
/// defect of group 1 item 1: `run_session`'s `select!` built a fresh `read_frame`
/// future every round, and a frame large enough to need several reads lost the
/// bytes it had already buffered when the outbound or heartbeat branch won —
/// the next read then took a body for a length header, and the connection died
/// with a decode error. The larger the frame and the busier the link, the likelier
/// the loss.
///
/// The size is what makes it a guard: a body at half of `BODY_TEXT_MAX_BYTES`
/// cannot be read in one pass, so the delivery crosses the TLS link while the
/// client is writing the ready and heartbeat frames of the same session. The
/// assertions are the two facts a desynchronised stream cannot produce — the
/// plugin reads the body back byte for byte, and a second task sent afterwards
/// still arrives, in order.
#[tokio::test]
async fn scenario_07_large_frames_interleaved() {
    let spec = r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#;

    let cluster = Cluster::start(spec).await.expect("start cluster");
    let worker_ws = cluster
        .register_role(
            "worker",
            "large frame prose",
            Some(
                r#"allowed_senders = ["*", "worker"]
allowed_targets = ["worker"]"#,
            ),
        )
        .await
        .expect("register worker");

    cluster
        .start_client(&worker_ws)
        .await
        .expect("start client");

    // Half the text ceiling: several reads per frame on the link, and still a
    // legal `body.text` the product is expected to carry whole.
    let big_body: String = (0..onlyne_proto::BODY_TEXT_MAX_BYTES / 2)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();

    let big_script = AgentScript {
        hello: ScriptHello {
            // `wait_assign` needs `inject`: see scenario 2's note.
            capabilities: vec![
                onlyne_proto::Capability::Register,
                onlyne_proto::Capability::Report,
                onlyne_proto::Capability::Inject,
            ],
        },
        steps: vec![
            json!({"wait_assign": true}),
            json!({"report": "heartbeat"}),
            json!({"echo_field_to": {"path": "assign.envelope.body.text", "file": "frames.log"}}),
            json!({"complete": {"outcome": "done", "head": "large frame read back"}}),
        ],
        repeat: false,
    };

    cluster
        .start_fake_agent(&worker_ws, &big_script)
        .await
        .expect("start agent");

    cluster
        .wait_role_online("worker")
        .await
        .expect("worker online");

    let big_send = cluster
        .admin_send("worker", "worker", &big_body)
        .await
        .expect("send the large task");
    let big_task = big_send["task"].as_str().expect("task id").to_string();

    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(big_task.clone()),
                ..Default::default()
            },
            |rows| rows.iter().any(|r| r.state == LedgerState::Acked),
            Duration::from_secs(60),
        )
        .await
        .expect("the large task must be acked");

    let frames_log = worker_ws.join("frames.log");
    let read_back = tokio::fs::read_to_string(&frames_log)
        .await
        .expect("the plugin must have written the body it read");
    assert_eq!(
        read_back.trim_end_matches('\n'),
        big_body,
        "the plugin must read the {} byte body back byte for byte",
        big_body.len()
    );

    // The link's next delivery is the other half of the claim: a stream that lost
    // bytes inside the large frame cannot carry an ordered frame after it. The
    // first agent's connection stays bound to the session it served, so the
    // second task is taken by a plugin mounting for it.
    let follow_script = AgentScript {
        hello: ScriptHello {
            capabilities: vec![
                onlyne_proto::Capability::Register,
                onlyne_proto::Capability::Report,
                onlyne_proto::Capability::Inject,
            ],
        },
        steps: vec![
            json!({"wait_assign": true}),
            json!({"report": "heartbeat"}),
            json!({"echo_field_to": {"path": "assign.envelope.body.text", "file": "frames.log"}}),
            json!({"complete": {"outcome": "done", "head": "follow-up read back"}}),
        ],
        repeat: false,
    };
    cluster
        .start_fake_agent(&worker_ws, &follow_script)
        .await
        .expect("start the follow-up agent");

    let follow_send = cluster
        .admin_send("worker", "worker", "after the large frame")
        .await
        .expect("send the follow-up task");
    let follow_task = follow_send["task"].as_str().expect("task id").to_string();
    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(follow_task.clone()),
                ..Default::default()
            },
            |rows| rows.iter().any(|r| r.state == LedgerState::Acked),
            Duration::from_secs(60),
        )
        .await
        .expect("the follow-up task must be acked");

    let both = tokio::fs::read_to_string(&frames_log)
        .await
        .expect("read the frames log");
    let lines: Vec<&str> = both.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "the log holds the large body and the follow-up, and nothing else"
    );
    assert_eq!(lines[0], big_body, "the large body arrives first");
    assert_eq!(
        lines[1], "after the large frame",
        "the frame after it arrives intact and in order"
    );

    println!(
        "✓ Scenario 7: large frames interleaved [{} byte body read back whole, next frame ordered]",
        big_body.len()
    );
}

/// Scenario 8: Refusal on a workspace from an older layout
///
/// Asserts: onlyne-client init on a workspace with channels/ directory or state.db
/// containing legacy markers exits with the migration code, and the refusal names
/// both the marker that decided it and the way forward. The wording itself is not
/// the contract: the sentence used to name a product version and tell the reader
/// nothing they could act on.
///
/// Supersedes: crates/onlyne-testkit/e2e/legacy-layout.sh
#[tokio::test]
async fn scenario_08_legacy_layout_refusal() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let legacy_ws = temp_dir.path().join("legacy");
    tokio::fs::create_dir_all(legacy_ws.join(".onlyne/channels"))
        .await
        .expect("create channels dir");
    tokio::fs::write(legacy_ws.join(".onlyne/state.db"), b"io_cursors")
        .await
        .expect("write legacy marker");

    let client_bin = Cluster::bin_path("onlyne-client").expect("resolve onlyne-client");

    let output = tokio::process::Command::new(&client_bin)
        .args([
            "init",
            "--workspace",
            legacy_ws.to_str().unwrap(),
            "--role",
            "planner",
            "--server-root",
            temp_dir.path().join("no-server").to_str().unwrap(),
        ])
        .output()
        .await
        .expect("run init");

    assert_eq!(
        output.status.code(),
        Some(onlyne_proto::EXIT_NEEDS_MIGRATION),
        "init must exit with the migration code on a workspace from an older layout"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("channels directory"),
        "the refusal must name the marker that decided it: {stderr}"
    );
    assert!(
        stderr.contains("onlyne-client init"),
        "the refusal must name the remedy: {stderr}"
    );

    println!("✓ Scenario 8: older-layout refusal [exit 6, names marker and remedy]");
}

/// Scenario 9: Heartbeat watch (role connected, session silent, fault recorded)
///
/// Asserts: session reports ready + one heartbeat, then goes silent. Server's stale
/// watch records heartbeat_missing fault, sessions answer has heartbeat_stale=true,
/// lifecycle stays working.
///
/// Supersedes: crates/onlyne-testkit/e2e/heartbeat-watch.sh
#[tokio::test]
async fn scenario_09_heartbeat_watch() {
    // Spec with short watch intervals
    let spec = r#"
[server]
name = "test"
listen = "127.0.0.1:0"
stale_watch_secs = 2
heartbeat_grace_secs = 4
"#;

    let cluster = Cluster::start(spec).await.expect("start cluster");
    let planner_ws = cluster
        .register_role(
            "planner",
            "planner prose",
            Some(
                r#"allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]"#,
            ),
        )
        .await
        .expect("register planner");

    cluster
        .start_client(&planner_ws)
        .await
        .expect("start client");

    // Script: report ready, one heartbeat, then sleep (go silent)
    let script = AgentScript {
        hello: ScriptHello {
            // `wait_assign` needs `inject`: see scenario 2's note.
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
            json!({"sleep_ms": 10000}), // sleep 10s, exceeding grace
        ],
        repeat: false,
    };

    cluster
        .start_fake_agent(&planner_ws, &script)
        .await
        .expect("start agent");

    cluster
        .wait_role_online("planner")
        .await
        .expect("planner online");

    let send_result = cluster
        .admin_send("planner", "planner", "beat then go quiet")
        .await
        .expect("admin send");
    let task_id = send_result["task"].as_str().expect("task id");

    // Phase 1: session is working and fresh (heartbeat flowing)
    let fresh_sessions = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(task_id.to_string()),
                ..Default::default()
            },
            |rows| {
                rows.iter()
                    .any(|r| r.public_lifecycle == Lifecycle::Working && !r.heartbeat_stale)
            },
            Duration::from_secs(10),
        )
        .await
        .expect("poll for fresh session");
    assert_eq!(fresh_sessions.len(), 1);

    // Phase 2: beats stop, sweep records heartbeat_missing fault
    tokio::time::sleep(Duration::from_secs(6)).await; // exceed grace window

    let faults_op = onlyne_proto::AdminOp::Faults(onlyne_proto::QueryFaultsArgs {
        task_id: Some(task_id.to_string()),
        ..Default::default()
    });
    let socket = cluster.admin_socket().expect("resolve admin socket");
    let mut stream = onlyne_wire::socket::connect_local(&socket)
        .await
        .expect("connect admin");
    #[derive(Debug, serde::Serialize)]
    #[serde(rename_all = "snake_case", tag = "f")]
    enum AdminFrame {
        Req {
            id: String,
            #[serde(flatten)]
            op: onlyne_proto::AdminOp,
        },
    }
    let req = AdminFrame::Req {
        id: onlyne_proto::new_id(),
        op: faults_op,
    };
    onlyne_wire::write_frame(&mut stream, &req)
        .await
        .expect("write faults query");
    let frame: onlyne_proto::Frame<onlyne_proto::AdminOp> = onlyne_wire::read_frame(&mut stream)
        .await
        .expect("read frame")
        .expect("frame");
    let body = match frame {
        onlyne_proto::Frame::Res { body, .. } => body,
        _ => panic!("expected res"),
    };
    assert!(body.ok, "faults query must succeed");
    let faults_data = body.data.expect("faults data");
    let faults: Vec<serde_json::Value> =
        serde_json::from_value(faults_data.get("faults").cloned().unwrap_or(json!([])))
            .expect("parse faults");

    let has_missing = faults.iter().any(|f| {
        f.get("task_id").and_then(|v| v.as_str()) == Some(task_id)
            && f.get("kind").and_then(|v| v.as_str()) == Some("heartbeat_missing")
    });
    assert!(has_missing, "server must record heartbeat_missing fault");

    // Phase 3: session is stale but still working
    let stale_sessions = cluster
        .query_sessions(QuerySessionsArgs {
            task_id: Some(task_id.to_string()),
            ..Default::default()
        })
        .await
        .expect("query sessions");
    assert_eq!(stale_sessions.len(), 1);
    assert_eq!(stale_sessions[0].public_lifecycle, Lifecycle::Working);
    assert!(stale_sessions[0].heartbeat_stale);

    println!(
        "✓ Scenario 9: heartbeat watch [session fresh→stale, heartbeat_missing fault, lifecycle stays working]"
    );
}

/// Scenario 10: Plugin conformance (fake plugin: hello → assign → report → detach)
///
/// Asserts: fake agent mounts with capabilities, receives assign, reports ready and
/// complete, exits cleanly.
///
/// Partially covered by existing conformance.rs tests. This scenario is the end-to-end
/// integration over real processes.
#[tokio::test]
async fn scenario_10_plugin_conformance() {
    let spec = r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#;

    let cluster = Cluster::start(spec).await.expect("start cluster");
    let worker_ws = cluster
        .register_role(
            "worker",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "worker"]
allowed_targets = ["worker"]"#,
            ),
        )
        .await
        .expect("register worker");

    cluster
        .start_client(&worker_ws)
        .await
        .expect("start client");

    let script = AgentScript {
        hello: ScriptHello {
            capabilities: vec![
                onlyne_proto::Capability::Register,
                onlyne_proto::Capability::Report,
                onlyne_proto::Capability::Inject,
                onlyne_proto::Capability::Recycle,
            ],
        },
        steps: vec![
            json!({"wait_assign": true}),
            json!({"report": "ready"}),
            json!({"report": "heartbeat"}),
            json!({"complete": {"outcome": "done", "head": "plugin done"}}),
            json!({"exit": "script finished"}),
        ],
        repeat: false,
    };

    cluster
        .start_fake_agent(&worker_ws, &script)
        .await
        .expect("start agent");

    cluster
        .wait_role_online("worker")
        .await
        .expect("worker online");

    let send_result = cluster
        .admin_send("worker", "worker", "plugin test")
        .await
        .expect("admin send");
    let task_id = send_result["task"].as_str().expect("task id");

    let acked_rows = cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(task_id.to_string()),
                ..Default::default()
            },
            |rows| rows.iter().any(|r| r.state == LedgerState::Acked),
            Duration::from_secs(30),
        )
        .await
        .expect("poll for acked");

    assert!(acked_rows.iter().any(|r| r.state == LedgerState::Acked));

    println!("✓ Scenario 10: plugin conformance [hello→assign→report→detach, task acked]");
}

/// Scenario 11: Federation (two clusters, aggregate role, parent ledger isolation)
///
/// Not ported to this harness. An aggregate role is an ordinary `[[client]]`
/// entry whose client the *upper* supervisor launches (docs/v2-PLAN.md D14, §5:
/// the protocol carries zero federation code), so the case needs two server roots
/// whose specs each name a role the other root's client serves. `Cluster` models
/// one server: `register_role` appends a fragment to its own root's spec and
/// `start_client` serves the roles that root names, so a second root's spec has
/// no seam here.
///
/// The behaviour is implemented and covered end to end by
/// crates/onlyne-testkit/e2e/two-cluster.sh, which
/// drives both roots and asserts the boundary this scenario names: the parent
/// ledger settles under the aggregate name and carries no child-layer role name
/// or prose.
#[tokio::test]
#[ignore = "needs a two-server harness; the case runs in e2e/two-cluster.sh"]
async fn scenario_11_federation() {
    println!(
        "⊗ Scenario 11: federation [ignored: this harness models one server; \
         the two-root case is e2e/two-cluster.sh]"
    );
}

/// Scenario 12: Generate and relocate (a rendered workspace survives a move to
/// another absolute path)
///
/// `onlyne generate` renders the workspace of every role `spec.toml` names from
/// the template tree under the server root, and prints the `[[client]]` fragment
/// whose key the rendered workspace owns; the operator puts that fragment into
/// the spec. The plan's relocation rule (docs/v2-PLAN.md §11, D20): the output
/// carries no absolute path of the place it was generated, which is what lets the
/// whole directory be moved elsewhere and still reach its server.
///
/// Asserts: the render lands under `<out>/<topology>/<role>` with a config and a
/// role key and no `run/` state; no file in it names the server root or the
/// output tree; the role serves a task from where it was generated; and the same
/// tree, moved to another absolute path and started there, serves the next one.
///
/// Supersedes: crates/onlyne-testkit/e2e/generate-relocate.sh
#[tokio::test]
async fn scenario_12_generate_relocate() {
    let spec = r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#;

    let cluster = Cluster::start(spec).await.expect("start cluster");

    // The template tree the render reads, shipped in the repository: `generate`
    // looks under `<server-root>/.onlyne/templates`.
    let templates = Cluster::repo_root()
        .expect("repo root")
        .join(".onlyne.example/templates");
    copy_tree(&templates, &cluster.server_root().join(".onlyne/templates"))
        .expect("copy the template tree into the server root");

    // `generate` renders the roles the spec names, so the role is seeded through
    // the ordinary init fragment first. Its seed entry leaves the spec below,
    // replaced by the fragment the render prints — that fragment carries the key
    // the rendered workspace just wrote, which is the identity it connects with.
    let seed_len = std::fs::metadata(cluster.spec_path())
        .expect("spec metadata")
        .len();
    let _seed = cluster
        .register_role(
            "builder",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "builder"]
allowed_targets = ["builder"]"#,
            ),
        )
        .await
        .expect("seed the builder entry");

    let scratch = tempfile::tempdir().expect("scratch dir");
    let out = scratch.path().join("gen");
    let fragment = cluster
        .cli(&[
            "generate",
            "--role",
            "builder",
            "--out",
            out.to_str().unwrap(),
        ])
        .await
        .expect("generate the builder workspace");

    // `<out>/<topology>/<role>`: the template tree's own shape is what the render
    // copies, and `dev/builder` is where the repository's templates put it.
    let rendered = out.join("dev/builder");
    assert!(
        rendered.join(".onlyne/config.toml").is_file(),
        "the render must write the role's config: {}",
        rendered.display()
    );
    assert!(
        rendered.join(".onlyne/keys/role.key").is_file(),
        "the render must write the role's key"
    );
    assert!(
        !rendered.join(".onlyne/run").exists(),
        "a render is not a run: it must leave no run directory"
    );

    // The relocation rule itself: no file in the output names the server root or
    // the tree it was generated into.
    for prefix in [cluster.server_root(), out.as_path()] {
        let prefix = prefix.display().to_string();
        let naming: Vec<String> = files_under(&rendered)
            .into_iter()
            .filter(|path| {
                std::fs::read(path)
                    .map(|bytes| String::from_utf8_lossy(&bytes).contains(&prefix))
                    .unwrap_or(false)
            })
            .map(|path| path.display().to_string())
            .collect();
        assert!(
            naming.is_empty(),
            "the render must carry no generation-time path ({prefix} appears in {naming:?})"
        );
    }

    let mut seeded = std::fs::read(cluster.spec_path()).expect("read the spec");
    seeded.truncate(seed_len as usize);
    let mut rebuilt = String::from_utf8(seeded).expect("the spec is utf8");
    rebuilt.push_str(&fragment);
    std::fs::write(cluster.spec_path(), rebuilt).expect("write the spec with the rendered entry");
    cluster.reload().await.expect("reload the spec");

    let serve_one = || AgentScript {
        hello: ScriptHello {
            // `wait_assign` needs `inject`: see scenario 2's note.
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
            json!({"complete": {"outcome": "done", "head_from": "assign_body"}}),
        ],
        repeat: false,
    };

    // Where it was rendered: the role serves from the generated tree. Nothing
    // waits for the role to read `online` first — work for a role whose client is
    // not up yet waits in the queue (scenario 5), so the ledger poll is the
    // assertion.
    cluster
        .start_client(&rendered)
        .await
        .expect("start the rendered client");
    cluster
        .start_fake_agent(&rendered, &serve_one())
        .await
        .expect("start the rendered agent");
    let native = cluster
        .admin_send("builder", "builder", "from where it was rendered")
        .await
        .expect("send to the rendered role");
    let native_task = native["task"].as_str().expect("task id").to_string();
    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(native_task),
                ..Default::default()
            },
            |rows| rows.iter().any(|r| r.state == LedgerState::Acked),
            Duration::from_secs(60),
        )
        .await
        .expect("the rendered workspace must serve its task");

    // The move: another absolute path, nothing inside the tree edited.
    cluster
        .kill_client(&rendered)
        .await
        .expect("stop the rendered client");
    cluster
        .kill_agent(&rendered)
        .await
        .expect("stop the rendered agent");
    let moved = scratch.path().join("elsewhere/builder");
    std::fs::create_dir_all(moved.parent().expect("destination parent"))
        .expect("create the destination");
    std::fs::rename(&rendered, &moved).expect("move the workspace");

    cluster
        .start_client(&moved)
        .await
        .expect("start the moved client");
    cluster
        .start_fake_agent(&moved, &serve_one())
        .await
        .expect("start the moved agent");
    let relocated = cluster
        .admin_send("builder", "builder", "from where it was moved")
        .await
        .expect("send to the moved role");
    let relocated_task = relocated["task"].as_str().expect("task id").to_string();
    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(relocated_task),
                ..Default::default()
            },
            |rows| rows.iter().any(|r| r.state == LedgerState::Acked),
            Duration::from_secs(60),
        )
        .await
        .expect("the moved workspace must still reach its server");

    println!("✓ Scenario 12: generate+relocate [rendered role served, moved tree served]");
}

/// Scenario 13: `oneshot` scope gives every delivery its own session.
#[tokio::test]
async fn scenario_13_oneshot_gives_each_delivery_its_own_session() {
    let cluster = Cluster::start(
        r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#,
    )
    .await
    .expect("start cluster");
    let worker_ws = cluster
        .register_role(
            "worker",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "worker"]
allowed_targets = ["worker"]"#,
            ),
        )
        .await
        .expect("register worker");
    cluster
        .start_client(&worker_ws)
        .await
        .expect("start client");
    cluster
        .start_fake_agent(&worker_ws, &serve_once(false))
        .await
        .expect("start first agent");
    cluster
        .wait_role_online("worker")
        .await
        .expect("worker online");

    let first = cluster
        .admin_send("worker", "worker", "oneshot one")
        .await
        .expect("send first");
    let first_task = task_of(&first);
    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(first_task.clone()),
                ..Default::default()
            },
            |rows| rows.iter().any(|row| row.state == LedgerState::Acked),
            Duration::from_secs(30),
        )
        .await
        .expect("first task acked");
    let first_row = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(first_task.clone()),
                ..Default::default()
            },
            |rows| !rows.is_empty(),
            Duration::from_secs(30),
        )
        .await
        .expect("first session row");
    let first_session = first_row[0].session_id.clone();

    cluster
        .start_fake_agent(&worker_ws, &serve_once(false))
        .await
        .expect("start second agent");
    let second = cluster
        .admin_send("worker", "worker", "oneshot two")
        .await
        .expect("send second");
    let second_task = task_of(&second);
    let second_rows = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(second_task.clone()),
                ..Default::default()
            },
            |rows| {
                rows.iter()
                    .any(|row| row.task_id.as_deref() == Some(second_task.as_str()))
            },
            Duration::from_secs(30),
        )
        .await
        .expect("second session row");
    assert_ne!(first_task, second_task);
    assert_ne!(first_session, second_rows[0].session_id);
    println!("✓ Scenario 13: oneshot scope [two deliveries, two session ids]");
}

/// Scenario 14: `task` scope keys a session by causality family.
#[tokio::test]
async fn scenario_14_task_scope_keeps_a_family_in_one_session() {
    let cluster = Cluster::start(
        r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#,
    )
    .await
    .expect("start cluster");
    let worker_ws = cluster
        .register_role(
            "worker",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "worker"]
allowed_targets = ["worker"]
max_sessions = 2"#,
            ),
        )
        .await
        .expect("register worker");
    append_client_session_config(&worker_ws, "task", "0s");
    let family_a = onlyne_proto::new_task_id();
    let family_b = onlyne_proto::new_task_id();
    cluster
        .start_client(&worker_ws)
        .await
        .expect("start client");
    cluster
        .start_fake_agent(&worker_ws, &serve_repeated(false, 2))
        .await
        .expect("start first agent");
    cluster
        .wait_role_online("worker")
        .await
        .expect("worker online");

    let first = cluster
        .admin_send_with_family("worker", "worker", "family one", &family_a)
        .await
        .expect("send first family delivery");
    let first_task = task_of(&first);
    let first_row = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(first_task.clone()),
                ..Default::default()
            },
            |rows| {
                rows.iter()
                    .any(|row| row.task_id.as_deref() == Some(first_task.as_str()))
            },
            Duration::from_secs(30),
        )
        .await
        .expect("first family session");
    let session_id = first_row[0].session_id.clone();
    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(first_task),
                ..Default::default()
            },
            |rows| rows.iter().any(|row| row.state == LedgerState::Acked),
            Duration::from_secs(30),
        )
        .await
        .expect("first family delivery acked");

    let second = cluster
        .admin_send_with_family("worker", "worker", "family two", &family_a)
        .await
        .expect("send second family delivery");
    let second_task = task_of(&second);
    let second_row = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(second_task.clone()),
                ..Default::default()
            },
            |rows| !rows.is_empty(),
            Duration::from_secs(30),
        )
        .await
        .expect("second family session");
    assert_eq!(second_row[0].session_id, session_id);

    cluster
        .start_fake_agent(&worker_ws, &serve_repeated(false, 1))
        .await
        .expect("start other-family agent");
    let other = cluster
        .admin_send_with_family("worker", "worker", "other family", &family_b)
        .await
        .expect("send other family delivery");
    let other_task = task_of(&other);
    let other_row = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(other_task.clone()),
                ..Default::default()
            },
            |rows| !rows.is_empty(),
            Duration::from_secs(30),
        )
        .await
        .expect("other family session");
    assert_ne!(other_row[0].session_id, session_id);
    println!("✓ Scenario 14: task scope [same family reuses, different family separates]");
}

/// Scenario 15: `role` scope pools sessions up to `max_sessions`.
#[tokio::test]
async fn scenario_15_role_scope_pools_up_to_max_sessions() {
    let cluster = Cluster::start(
        r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#,
    )
    .await
    .expect("start cluster");
    let worker_ws = cluster
        .register_role(
            "worker",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "worker"]
allowed_targets = ["worker"]
max_sessions = 2"#,
            ),
        )
        .await
        .expect("register worker");
    append_client_session_config(&worker_ws, "role", "0s");
    cluster
        .start_client(&worker_ws)
        .await
        .expect("start client");
    let script = serve_repeated_with_delay(true, 2, 5_000);
    cluster
        .start_fake_agent(&worker_ws, &script)
        .await
        .expect("start first agent");
    cluster
        .start_fake_agent(&worker_ws, &script)
        .await
        .expect("start second agent");
    cluster
        .wait_role_online("worker")
        .await
        .expect("worker online");

    let first = cluster
        .admin_send("worker", "worker", "role one")
        .await
        .expect("send first");
    let first_task = task_of(&first);
    cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(first_task.clone()),
                ..Default::default()
            },
            |rows| {
                rows.iter()
                    .any(|row| row.task_id.as_deref() == Some(first_task.as_str()))
            },
            Duration::from_secs(10),
        )
        .await
        .expect("first role session");
    let second = cluster
        .admin_send("worker", "worker", "role two")
        .await
        .expect("send second");
    let second_task = task_of(&second);
    let active = cluster
        .poll_sessions(
            QuerySessionsArgs::default(),
            |rows| {
                rows.iter()
                    .filter(|row| {
                        row.task_id.as_deref() == Some(first_task.as_str())
                            || row.task_id.as_deref() == Some(second_task.as_str())
                    })
                    .count()
                    == 2
            },
            Duration::from_secs(30),
        )
        .await
        .expect("two role sessions");
    let first_session = active
        .iter()
        .find(|row| row.task_id.as_deref() == Some(first_task.as_str()))
        .expect("first role session")
        .session_id
        .clone();
    let second_session = active
        .iter()
        .find(|row| row.task_id.as_deref() == Some(second_task.as_str()))
        .expect("second role session")
        .session_id
        .clone();
    assert_ne!(first_session, second_session);

    let third = cluster
        .admin_send("worker", "worker", "role three")
        .await
        .expect("send third");
    let third_task = task_of(&third);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let waiting = cluster
        .query_sessions(QuerySessionsArgs {
            task_id: Some(third_task.clone()),
            ..Default::default()
        })
        .await
        .expect("query waiting third task");
    assert!(
        waiting.is_empty(),
        "third task must wait while both slots are busy"
    );

    let settled = cluster
        .poll_ledger(
            LedgerQuery::default(),
            |rows| {
                rows.iter().any(|row| {
                    row.task.as_deref() == Some(first_task.as_str())
                        && row.state == LedgerState::Acked
                }) && rows.iter().any(|row| {
                    row.task.as_deref() == Some(second_task.as_str())
                        && row.state == LedgerState::Acked
                })
            },
            Duration::from_secs(60),
        )
        .await
        .expect("first two role tasks acked");
    assert!(
        settled
            .iter()
            .any(|row| row.task.as_deref() == Some(first_task.as_str()))
    );
    let third_row = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(third_task.clone()),
                ..Default::default()
            },
            |rows| {
                rows.iter()
                    .any(|row| row.task_id.as_deref() == Some(third_task.as_str()))
            },
            Duration::from_secs(30),
        )
        .await
        .expect("third role task eventually dispatched");
    assert!(third_row[0].session_id == first_session || third_row[0].session_id == second_session);
    println!("✓ Scenario 15: role scope [two active sessions, third waits then reuses]");
}

/// Scenario 16: suspension frees a slot and the same family resumes its session.
#[tokio::test]
async fn scenario_16_suspend_frees_the_slot_and_the_family_resumes_it() {
    let cluster = Cluster::start(
        r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#,
    )
    .await
    .expect("start cluster");
    let worker_ws = cluster
        .register_role(
            "worker",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "worker"]
allowed_targets = ["worker"]"#,
            ),
        )
        .await
        .expect("register worker");
    append_client_session_config(&worker_ws, "task", "1s");
    cluster
        .start_client(&worker_ws)
        .await
        .expect("start client");
    cluster
        .start_fake_agent(&worker_ws, &serve_repeated(true, 2))
        .await
        .expect("start resumable agent");
    cluster
        .wait_role_online("worker")
        .await
        .expect("worker online");

    let first = cluster
        .admin_send_with_family("worker", "worker", "suspend me", "family-suspend")
        .await
        .expect("send first");
    let first_task = task_of(&first);
    let first_rows = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(first_task.clone()),
                ..Default::default()
            },
            |rows| {
                rows.iter()
                    .any(|row| row.task_id.as_deref() == Some(first_task.as_str()))
            },
            Duration::from_secs(30),
        )
        .await
        .expect("first session row");
    let session_id = first_rows[0].session_id.clone();
    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(first_task),
                ..Default::default()
            },
            |rows| rows.iter().any(|row| row.state == LedgerState::Acked),
            Duration::from_secs(30),
        )
        .await
        .expect("first task acked");
    cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(session_id.clone()),
                ..Default::default()
            },
            // `resource == Closed` alone is not the precondition this scenario
            // needs. Two different paths close a resource: the sweep's `Suspend`,
            // which keeps the generation live and so projects `idle` — the
            // session is suspended and the family's next delivery resumes it —
            // and an agent's own exit, which kills the generation and projects
            // `exited`, after which the only honest answer is a new session.
            //
            // Waiting on the resource alone accepted either, so a run that lost
            // the race proceeded to a new conversation and failed 30 seconds
            // later on a symptom two steps from its cause. The lifecycle is what
            // says which of the two happened.
            |rows| {
                rows.iter().any(|row| {
                    row.projection.resource == onlyne_proto::ResourcePhase::Closed
                        && row.public_lifecycle == onlyne_proto::Lifecycle::Idle
                })
            },
            Duration::from_secs(15),
        )
        .await
        .expect("session suspended rather than exited");

    cluster
        .start_fake_agent(&worker_ws, &serve_repeated(true, 1))
        .await
        .expect("start agent for resume");

    let second = cluster
        .admin_send_with_family("worker", "worker", "resume me", "family-suspend")
        .await
        .expect("send family continuation");
    let second_task = task_of(&second);
    let resumed = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(second_task.clone()),
                ..Default::default()
            },
            |rows| {
                rows.iter()
                    .any(|row| row.task_id.as_deref() == Some(second_task.as_str()))
            },
            Duration::from_secs(30),
        )
        .await
        .expect("resumed session row");
    assert_eq!(resumed[0].session_id, session_id);
    assert_eq!(
        resumed[0].projection.resource,
        onlyne_proto::ResourcePhase::Attached
    );
    println!("✓ Scenario 16: suspend/resume [suspended slot is reusable, family keeps session id]");
}

/// Scenario 17: without `resume`, an idle scoped session keeps its process.
#[tokio::test]
async fn scenario_17_no_resume_capability_keeps_the_process_and_the_session() {
    let cluster = Cluster::start(
        r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#,
    )
    .await
    .expect("start cluster");
    let worker_ws = cluster
        .register_role(
            "worker",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "worker"]
allowed_targets = ["worker"]
max_sessions = 2"#,
            ),
        )
        .await
        .expect("register worker");
    append_client_session_config(&worker_ws, "task", "1s");
    cluster
        .start_client(&worker_ws)
        .await
        .expect("start client");
    cluster
        .start_fake_agent(&worker_ws, &serve_repeated(false, 2))
        .await
        .expect("start non-resumable agent");
    cluster
        .wait_role_online("worker")
        .await
        .expect("worker online");

    let first = cluster
        .admin_send_with_family("worker", "worker", "stay alive", "family-live")
        .await
        .expect("send first");
    let first_task = task_of(&first);
    let first_rows = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(first_task.clone()),
                ..Default::default()
            },
            |rows| {
                rows.iter()
                    .any(|row| row.task_id.as_deref() == Some(first_task.as_str()))
            },
            Duration::from_secs(30),
        )
        .await
        .expect("first row");
    let session_id = first_rows[0].session_id.clone();
    cluster
        .poll_ledger(
            LedgerQuery {
                task: Some(first_task),
                ..Default::default()
            },
            |rows| rows.iter().any(|row| row.state == LedgerState::Acked),
            Duration::from_secs(30),
        )
        .await
        .expect("first task acked");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let idle = cluster
        .query_sessions(QuerySessionsArgs {
            task_id: Some(session_id.clone()),
            ..Default::default()
        })
        .await
        .expect("query idle session");
    assert_eq!(idle.len(), 1);
    assert_eq!(
        idle[0].projection.resource,
        onlyne_proto::ResourcePhase::Attached,
        "without resume the process must stay alive"
    );

    let second = cluster
        .admin_send_with_family("worker", "worker", "still here", "family-live")
        .await
        .expect("send family continuation");
    let second_task = task_of(&second);
    let resumed = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(second_task.clone()),
                ..Default::default()
            },
            |rows| {
                rows.iter()
                    .any(|row| row.task_id.as_deref() == Some(second_task.as_str()))
            },
            Duration::from_secs(30),
        )
        .await
        .expect("family continuation row");
    assert_eq!(resumed[0].session_id, session_id);
    println!("✓ Scenario 17: no resume [idle process stays, family reuses session]");
}

/// Scenario 18: a heartbeat that only refreshes `last_seen` is not a projection write.
#[tokio::test]
async fn scenario_18_a_last_seen_only_heartbeat_writes_no_updated_at_and_no_event() {
    let cluster = Cluster::start(
        r#"
[server]
name = "test"
listen = "127.0.0.1:0"
"#,
    )
    .await
    .expect("start cluster");
    let worker_ws = cluster
        .register_role(
            "worker",
            E2E_PROSE,
            Some(
                r#"allowed_senders = ["*", "worker"]
allowed_targets = ["worker"]"#,
            ),
        )
        .await
        .expect("register worker");
    cluster
        .start_client(&worker_ws)
        .await
        .expect("start client");
    cluster
        .start_fake_agent(
            &worker_ws,
            &AgentScript {
                hello: ScriptHello {
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
                    json!({"sleep_ms": 3000}),
                    json!({"report": "heartbeat"}),
                    json!({"sleep_ms": 1000}),
                ],
                repeat: false,
            },
        )
        .await
        .expect("start heartbeat agent");
    cluster
        .wait_role_online("worker")
        .await
        .expect("worker online");
    let sent = cluster
        .admin_send("worker", "worker", "heartbeat freshness")
        .await
        .expect("send heartbeat task");
    let task_id = task_of(&sent);
    let before = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(task_id.clone()),
                ..Default::default()
            },
            |rows| {
                rows.iter().any(|row| {
                    row.task_id.as_deref() == Some(task_id.as_str())
                        && row.updated_at.is_some()
                        && row.last_seen.is_some()
                })
            },
            Duration::from_secs(30),
        )
        .await
        .expect("initial projection row");
    let before = before[0].clone();
    let before_events = cluster
        .history(0, Some(&task_id))
        .await
        .expect("history before beat");
    let after = cluster
        .poll_sessions(
            QuerySessionsArgs {
                task_id: Some(task_id.clone()),
                ..Default::default()
            },
            |rows| {
                rows.iter()
                    .any(|row| row.last_seen.is_some() && row.last_seen != before.last_seen)
            },
            Duration::from_secs(10),
        )
        .await
        .expect("query after heartbeat");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].updated_at, before.updated_at);
    assert_ne!(after[0].last_seen, before.last_seen);
    let after_events = cluster
        .history(0, Some(&task_id))
        .await
        .expect("history after beat");
    assert_eq!(after_events.len(), before_events.len());
    println!("✓ Scenario 18: last-seen heartbeat [updated_at and event stream unchanged]");
}

fn task_of(receipt: &serde_json::Value) -> String {
    receipt["task"].as_str().expect("task id").to_string()
}

fn append_client_session_config(workspace: &std::path::Path, scope: &str, idle_close: &str) {
    let config = workspace.join(".onlyne/config.toml");
    let mut text = std::fs::read_to_string(&config).expect("read client config");
    text.push_str(&format!(
        "\n[client.session]\nscope = {scope:?}\nidle_close = {idle_close:?}\n"
    ));
    std::fs::write(config, text).expect("write client session config");
}

fn serve_once(resume: bool) -> AgentScript {
    let mut capabilities = vec![
        onlyne_proto::Capability::Register,
        onlyne_proto::Capability::Report,
        onlyne_proto::Capability::Inject,
    ];
    if resume {
        capabilities.push(onlyne_proto::Capability::Resume);
    }
    AgentScript {
        hello: ScriptHello { capabilities },
        steps: vec![
            json!({"wait_assign": true}),
            json!({"report": "ready"}),
            json!({"report": "heartbeat"}),
            json!({"sleep_ms": 3000}),
            json!({"complete": {"outcome": "done", "head_from": "assign_body"}}),
        ],
        repeat: false,
    }
}

fn serve_repeated(resume: bool, turns: u64) -> AgentScript {
    serve_repeated_with_delay(resume, turns, 0)
}

fn serve_repeated_with_delay(resume: bool, turns: u64, delay_ms: u64) -> AgentScript {
    let capabilities = serve_once(resume).hello.capabilities;
    let mut steps = Vec::new();
    for _ in 0..turns {
        steps.extend([
            json!({"wait_assign": true}),
            json!({"report": "ready"}),
            json!({"report": "heartbeat"}),
        ]);
        if delay_ms > 0 {
            steps.push(json!({"sleep_ms": delay_ms}));
        }
        steps.push(json!({"report": "idle"}));
        steps.push(json!({
            "complete": {"outcome": "done", "head_from": "assign_body"}
        }));
    }
    AgentScript {
        hello: ScriptHello { capabilities },
        steps,
        repeat: false,
    }
}

/// Every regular file under `root`, in no particular order.
fn files_under(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// Copy one tree of files, directories and all.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
