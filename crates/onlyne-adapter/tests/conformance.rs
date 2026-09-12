//! Host-side dispatch conformance for the adapter SDK.
//!
//! Two behaviors sit outside the happy path: a host that never implements a
//! `typing` arm, and a host whose declared capability fails in its handler.
//! Both must answer through the response body instead of breaking dispatch.

use onlyne_adapter::{Host, HostDispatcher};
use onlyne_proto::{
    Capability, ErrorCode, HealthArgs, HelloAck, HelloArgs, Mount, MountKind, PROTOCOL_VERSION,
    PluginOp, ServerInfo, SessionRegisterArgs, TypingArgs,
};
use std::sync::Arc;

fn ack(host_capabilities: Vec<Capability>) -> HelloAck {
    HelloAck {
        protocol: PROTOCOL_VERSION,
        role: "planner".to_string(),
        session_id: None,
        generation: 1,
        prose: "prose".to_string(),
        server: ServerInfo {
            connected: true,
            cluster: "local".to_string(),
            name: "server".to_string(),
        },
        host_capabilities,
    }
}

/// A host that serves `health` and leaves every other arm at its default.
struct HealthOnlyHost;

#[async_trait::async_trait]
impl Host for HealthOnlyHost {
    async fn hello(&self, _args: &HelloArgs) -> std::result::Result<HelloAck, (ErrorCode, String)> {
        Ok(ack(vec![]))
    }

    async fn health(&self, _args: &HealthArgs) -> std::result::Result<(), (ErrorCode, String)> {
        Ok(())
    }
}

/// A host that declares `typing` and fails while serving it.
struct FailingTypingHost;

#[async_trait::async_trait]
impl Host for FailingTypingHost {
    async fn hello(&self, _args: &HelloArgs) -> std::result::Result<HelloAck, (ErrorCode, String)> {
        Ok(ack(vec![Capability::Typing]))
    }

    async fn typing(&self, _args: &TypingArgs) -> std::result::Result<(), (ErrorCode, String)> {
        Err((
            ErrorCode::Unauthorized,
            "typing rejected for this conversation".to_string(),
        ))
    }
}

#[tokio::test]
async fn host_without_a_typing_arm_still_dispatches() {
    let dispatcher = HostDispatcher::new(MountKind::Gateway, Arc::new(HealthOnlyHost));

    let typing = dispatcher
        .dispatch(PluginOp::Typing(TypingArgs {
            conversation: "c1".to_string(),
            on: true,
        }))
        .await;
    assert!(!typing.ok);
    let error = typing.error.expect("absent arm answers with an error");
    assert_eq!(error.code, ErrorCode::UnknownOp);
    assert_eq!(error.message, "typing is unsupported");

    let health = dispatcher
        .dispatch(PluginOp::Health(HealthArgs::default()))
        .await;
    assert!(health.ok, "the dispatcher keeps serving implemented arms");

    let wrong_mount = dispatcher
        .dispatch(PluginOp::SessionRegister(SessionRegisterArgs::default()))
        .await;
    assert!(
        !wrong_mount.ok,
        "an agent-only op stays refused on a gateway mount"
    );
    assert_eq!(
        wrong_mount.error.expect("refusal carries an error").code,
        ErrorCode::Forbidden
    );
}

#[tokio::test]
async fn declared_capability_error_surfaces_through_the_response_body() {
    let dispatcher = HostDispatcher::new(MountKind::Gateway, Arc::new(FailingTypingHost));

    let host = FailingTypingHost;
    let welcome = host
        .hello(&HelloArgs {
            protocol: PROTOCOL_VERSION,
            plugin: "onlyne-gateway-telegram".to_string(),
            version: "1.0.0".to_string(),
            kind: MountKind::Gateway,
            capabilities: vec![Capability::Typing],
            mount: None,
        })
        .await
        .expect("hello answers with a welcome");
    assert_eq!(
        welcome.host_capabilities,
        vec![Capability::Typing],
        "the host declares the capability it will exercise"
    );

    let typing = dispatcher
        .dispatch(PluginOp::Typing(TypingArgs {
            conversation: "c1".to_string(),
            on: true,
        }))
        .await;
    assert!(!typing.ok);
    let error = typing.error.expect("handler failure carries an error");
    assert_eq!(error.code, ErrorCode::Unauthorized);
    assert_eq!(error.message, "typing rejected for this conversation");
}

#[test]
fn admin_mount_does_not_survive_the_wire() {
    // `Mount` is untagged and `Mount::Admin` is a unit variant, so it writes
    // JSON null and cannot be told apart from an absent mount. Hosts read
    // `MountKind` for an admin or agent hello; only `Mount::Agent`,
    // `Mount::Gateway`, and `Mount::Cluster` carry payloads.
    let wire = serde_json::to_value(Some(Mount::Admin)).expect("admin mount encodes");
    assert_eq!(wire, serde_json::Value::Null);
    assert_eq!(serde_json::to_value(Option::<Mount>::None).unwrap(), wire);
    let decoded: Option<Mount> = serde_json::from_value(wire).expect("null decodes");
    assert_eq!(decoded, None, "an admin mount cannot round-trip");
    let agent = serde_json::to_value(Some(Mount::Agent(onlyne_proto::AgentMount {
        role: "planner".to_string(),
        session: None,
        task_id: None,
        pid: None,
    })))
    .expect("agent mount encodes");
    assert_eq!(
        agent["role"], "planner",
        "an untagged mount carries its payload inline, with no kind wrapper"
    );
}
