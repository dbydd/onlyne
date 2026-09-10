use std::time::Duration;

use onlyne_adapter::{AdapterServer, Host, HostDispatcher, MountKind};
use onlyne_proto::{
    AdapterMsg, AssignArgs, Capability, ErrorCode, HelloAck, HelloArgs, HostOp, Mount, PluginOp,
    PROTOCOL_VERSION, Report, ServerInfo,
};
use onlyne_testkit::{
    HostSim, HostSimSpec, empty_body_envelope, oversized_image_envelope, sample_assign,
    session_backend_choice,
};
use serde_json::json;

fn ack() -> HelloAck {
    HelloAck {
        protocol: PROTOCOL_VERSION,
        role: "planner".to_string(),
        session_id: Some("s1".to_string()),
        generation: 1,
        prose: "prose".to_string(),
        server: ServerInfo {
            connected: true,
            cluster: "test".to_string(),
            name: "sim".to_string(),
        },
        host_capabilities: vec![],
    }
}

fn hello(caps: Vec<Capability>) -> HelloArgs {
    HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: "test-plugin".to_string(),
        version: "1.0.0".to_string(),
        kind: MountKind::Agent,
        capabilities: caps,
        mount: Some(Mount::Agent(onlyne_proto::AgentMount {
            role: "planner".to_string(),
            session: None,
            task_id: None,
            pid: None,
        })),
    }
}

#[tokio::test]
async fn frame_before_hello_is_rejected_and_connection_closes() {
    let (client, server) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move { AdapterServer::accept(server, |_| Ok(ack())).await });
    let io = onlyne_adapter::AdapterIo::new(client, Duration::from_secs(1), Duration::from_secs(1));
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Detach(onlyne_proto::DetachArgs {
            reason: "early".to_string(),
        })))
        .await
        .expect("error response");
    let error = body.error.expect("error payload");
    assert_eq!(error.code, ErrorCode::Unauthorized);
    assert_eq!(error.message, "hello required first");
    let server_error = match task.await.expect("server task") {
        Ok(_) => panic!("server accepted pre-hello frame"),
        Err(error) => error,
    };
    assert_eq!(server_error.code(), Some(ErrorCode::Unauthorized));
}

#[tokio::test]
async fn missing_recycle_uses_probe_resource_loss_path() {
    let host = HostSim::new(HostSimSpec::agent("planner", "prose", vec![Capability::Recycle]));
    let (agent, task) = host.clone().connect_agent();
    agent.hello(hello(vec![])).await.expect("hello");
    host.handle_missing_recycle("task-1", Duration::from_millis(1))
        .await
        .expect("probe path");
    let op = tokio::time::timeout(Duration::from_secs(1), agent.next_host_op())
        .await
        .expect("probe arrives")
        .expect("host op");
    assert!(matches!(op, HostOp::Probe(_)));
    task.abort();
}

#[tokio::test]
async fn missing_report_records_idle_fault() {
    let host = HostSim::new(HostSimSpec::agent("planner", "prose", vec![Capability::Report]));
    let result = host.hello(&hello(vec![])).await.expect("hello");
    assert_eq!(result.role, "planner");
    assert_eq!(host.recovery().await.as_deref(), Some("idle_fault"));
    assert_eq!(host.faults().await.len(), 1);
}

#[tokio::test]
async fn stale_generation_is_conflict_and_watermark_is_unchanged() {
    let host = HostSim::new(HostSimSpec::agent("planner", "prose", vec![]));
    host.set_watermark((4, 9)).await;
    let stale = Report::Heartbeat {
        task_id: "task-1".to_string(),
        generation: 4,
        seq: 9,
        observed: json!({"state":"old"}),
    };
    let error = host.report(&stale).await.expect_err("stale report rejected");
    assert_eq!(error.0, ErrorCode::Conflict);
    assert_eq!(host.watermark().await, (4, 9));
}

#[tokio::test]
async fn oversized_image_is_rejected_with_wire_field() {
    let host = HostSim::new(HostSimSpec::agent("planner", "prose", vec![]));
    let dispatcher = HostDispatcher::new(MountKind::Agent, host);
    let body = dispatcher
        .dispatch(PluginOp::Send(Box::new(oversized_image_envelope(3 * 1024 * 1024))))
        .await;
    let error = body.error.expect("image error");
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.message, "image exceeds 2097152 bytes");
    assert_eq!(error.field.as_deref(), Some("body.image.data_base64"));
}

#[tokio::test]
async fn empty_body_is_rejected_naming_body() {
    let host = HostSim::new(HostSimSpec::agent("planner", "prose", vec![]));
    let dispatcher = HostDispatcher::new(MountKind::Agent, host);
    let body = dispatcher
        .dispatch(PluginOp::Send(Box::new(empty_body_envelope())))
        .await;
    let error = body.error.expect("body error");
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.field.as_deref(), Some("body"));
}

#[tokio::test]
async fn duplicate_send_returns_original_receipt_unchanged() {
    let host = HostSim::new(HostSimSpec::agent("planner", "prose", vec![]));
    let envelope = onlyne_testkit::sample_task_envelope("same");
    let first = host.send(&envelope).await.expect("first send");
    let second = host.send(&envelope).await.expect("duplicate send");
    assert_eq!(first, second);
    assert!(!second.duplicate);
}

#[tokio::test]
async fn dropped_assign_reconnect_redelivers_same_op_id_with_original_receipt() {
    let host = HostSim::new(HostSimSpec::agent("planner", "prose", vec![]));
    let (first_agent, first_task) = host.clone().connect_agent();
    first_agent
        .hello(hello(vec![Capability::Register, Capability::Report, Capability::Inject, Capability::Recycle]))
        .await
        .expect("first hello");
    let assign = sample_assign("important work", "prose");
    let assign_task_id = assign.task_id.clone();
    let op_id = assign.envelope.op_id.clone().expect("assign carries op_id");
    host.queue_assign(assign).await.expect("queue assign");
    let accepted = tokio::time::timeout(Duration::from_secs(5), async {
        first_agent
            .report_ready(assign_task_id.clone(), "sim-session")
            .await
            .expect("ready");
        first_agent.wait_assign().await.expect("first assign")
    })
    .await
    .expect("assign emitted after ready");
    drop(first_agent);
    first_task.abort();
    let (second_agent, second_task) = host.clone().connect_agent();
    second_agent
        .hello(hello(vec![Capability::Register, Capability::Report, Capability::Inject, Capability::Recycle]))
        .await
        .expect("second hello");
    let replayed = second_agent.wait_assign().await.expect("second assign");
    assert_eq!(replayed.envelope.op_id.as_deref(), Some(op_id.as_str()));
    assert_eq!(replayed, accepted);
    second_task.abort();
}

#[test]
fn fake_backend_three_way_fixture_is_selected_when_available() {
    assert_eq!(
        session_backend_choice(),
        "hostsim-stub:onlyne-session is unavailable without a sibling dependency"
    );
    let assign = sample_assign("task body", "prose");
    assert_eq!(assign.envelope.body.text.as_deref(), Some("task body"));
}

#[tokio::test]
async fn fake_agent_over_hostsim_completes_scripted_task() {
    use onlyne_testkit::{AgentScript, FakeAgent};

    let assign = sample_assign("scripted prose", "scripted prose");
    let mut spec = HostSimSpec::agent("planner", "prose", vec![]);
    spec.scripted = vec![onlyne_proto::HostOp::Assign(assign)];
    let host = HostSim::new(spec);
    let (agent, task) = host.clone().connect_agent();
    let script = AgentScript::from_json_str(
        r#"{"hello": {"capabilities": ["register","report","inject","recycle"]},
            "steps": [{"wait_assign": true},
                      {"report": "ready"},
                      {"complete": {"outcome": "done", "head_from": "assign_body"}},
                      {"echo_prose_to": "prose.log"}]}"#,
    )
    .expect("parse script");
    let workspace = tempfile::tempdir().expect("tempdir");
    let fake = FakeAgent::new(
        "planner",
        vec![Capability::Register, Capability::Report, Capability::Inject, Capability::Recycle],
        script,
        workspace.path(),
    );
    fake.run(&agent).await.expect("fake agent runs");
    assert_eq!(agent.report_sender().generation(), 1);
    task.abort();
}

#[test]
fn platform_payload_stays_on_gateway_side() {
    use onlyne_proto::RenderSendArgs;

    let render = RenderSendArgs {
        envelope: Box::new(onlyne_testkit::sample_task_envelope("render me")),
        conversation: "conv-1".to_string(),
        gateway_ref: None,
    };
    let handed = serde_json::json!({
        "conversation": render.conversation,
        "text": render.envelope.body.text,
        "image": render.envelope.body.image,
    });
    assert!(handed.get("platform_metadata").is_none(), "rendered payload must not carry platform_metadata: {handed}");
    assert!(handed.get("raw").is_none(), "rendered payload must not carry raw: {handed}");
    assert!(handed.get("channel_id").is_none(), "rendered payload must not carry channel_id: {handed}");

    let assign = sample_assign("agent work", "agent prose");
    let viewed = serde_json::to_value(&assign).expect("serialize assign");
    let rendered = serde_json::to_string(&viewed).expect("render assign");
    for forbidden in ["platform_metadata", "raw", "channel_id"] {
        assert!(
            !rendered.contains(&format!("\"{forbidden}\"")),
            "assign delivered {rendered}"
        );
    }

    let gateway = onlyne_testkit::FakeGateway::new("fake", "fg1");
    let inbound = gateway.inbound_delivery("conv-1", "hello").expect("inbound delivery");
    let propagated = AssignArgs {
        envelope: inbound.envelope,
        prose: "agent prose".to_string(),
        task_id: "task-1".to_string(),
        generation: 1,
        parent: None,
    };
    let serialized = serde_json::to_value(&propagated).expect("serialize propagated");
    let shown = serde_json::to_string(&serialized).expect("render propagated");
    for forbidden in ["platform_metadata", "raw", "channel_id"] {
        assert!(
            !shown.contains(&format!("\"{forbidden}\"")),
            "gateway inbound produced {shown}"
        );
    }
}
