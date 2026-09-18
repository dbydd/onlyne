//! Host-side dispatch conformance for the adapter SDK.
//!
//! Three behaviors sit outside the happy path: a host that never implements a
//! `typing` arm, a host whose declared capability fails in its handler, and the
//! mount rule for a local viewer's `watch_content` subscription — allowed on an
//! admin mount, refused everywhere else, with the pushed `content` frames
//! travelling back over the same connection.
//!
//! `admin_mount_does_not_survive_the_wire` pins the reason the rule reads
//! `kind` and not `mount`: an admin `hello` cannot carry a marker at all.

use futures_util::StreamExt;
use onlyne_adapter::{AdapterClient, AdapterServer, Host, HostDispatcher};
use onlyne_proto::{
    AdapterMsg, Capability, ContentFrame, DetachArgs, ErrorCode, HealthArgs, HelloAck, HelloArgs,
    HostOp, Mount, MountKind, PROTOCOL_VERSION, PluginOp, ServerInfo, SessionRegisterArgs,
    TypingArgs, WatchContentArgs,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

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

/// A host that takes a viewer's subscription and remembers what was asked.
#[derive(Default)]
struct WatchHost {
    subscription: tokio::sync::Mutex<Option<WatchContentArgs>>,
}

#[async_trait::async_trait]
impl Host for WatchHost {
    async fn hello(&self, _args: &HelloArgs) -> std::result::Result<HelloAck, (ErrorCode, String)> {
        Ok(ack(vec![]))
    }

    async fn watch_content(
        &self,
        args: &WatchContentArgs,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        *self.subscription.lock().await = Some(args.clone());
        Ok(())
    }
}

fn subscription() -> WatchContentArgs {
    WatchContentArgs {
        task_id: Some("task-1".to_string()),
        since: Some(41),
    }
}

fn content(seq: u64, task_id: &str, record: serde_json::Value) -> ContentFrame {
    ContentFrame {
        seq,
        task_id: task_id.to_string(),
        session_id: Some("s1".to_string()),
        at: "2026-09-18T12:00:03Z".to_string(),
        record,
    }
}

#[tokio::test]
async fn an_admin_mount_may_subscribe_to_session_content() {
    let host = Arc::new(WatchHost::default());
    let dispatcher = HostDispatcher::new(MountKind::Admin, host.clone());
    let args = subscription();

    let body = dispatcher
        .dispatch(PluginOp::WatchContent(args.clone()))
        .await;
    assert!(
        body.ok,
        "watch_content is the op an admin mount exists to send: {body:?}"
    );
    assert_eq!(
        host.subscription.lock().await.as_ref(),
        Some(&args),
        "the host arm sees the task and the cursor the viewer asked for"
    );
}

#[tokio::test]
async fn watch_content_is_the_only_op_an_admin_mount_may_send() {
    let host = Arc::new(WatchHost::default());

    for (mount, dispatcher) in [
        (
            MountKind::Agent,
            HostDispatcher::new(MountKind::Agent, host.clone()),
        ),
        (
            MountKind::Gateway,
            HostDispatcher::new(MountKind::Gateway, host.clone()),
        ),
    ] {
        let refused = dispatcher
            .dispatch(PluginOp::WatchContent(subscription()))
            .await;
        assert!(!refused.ok, "a {mount:?} mount may not subscribe this way");
        let error = refused.error.expect("a wrong-mount frame is refused");
        assert_eq!(error.code, ErrorCode::Forbidden);
        assert!(
            error.message.contains("watch_content"),
            "the refusal names the operation: {error:?}"
        );
        assert!(
            error.message.contains(&format!("{mount:?}")),
            "and the mount that barred it: {error:?}"
        );
    }
    assert!(
        host.subscription.lock().await.is_none(),
        "a refused subscription never reaches the host arm"
    );

    // The admin arm lists the one operation rather than trusting the mount, so a
    // viewer that finishes its work hangs up instead of sending `detach`.
    let admin = HostDispatcher::new(MountKind::Admin, host);
    let leaving = admin
        .dispatch(PluginOp::Detach(DetachArgs {
            reason: "viewer done".to_string(),
        }))
        .await;
    let error = leaving.error.expect("detach is not a viewer operation");
    assert_eq!(error.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn a_host_with_no_content_face_declines_rather_than_invents_a_stream() {
    // The mount rule and the host's own gap answer with different codes: a
    // viewer on the wrong mount is refused, and a viewer the host cannot serve
    // is told so. Collapsing the two would hide a host that never wired content.
    let dispatcher = HostDispatcher::new(MountKind::Admin, Arc::new(HealthOnlyHost));
    let body = dispatcher
        .dispatch(PluginOp::WatchContent(subscription()))
        .await;
    let error = body
        .error
        .expect("an unimplemented arm answers through the body");
    assert_eq!(error.code, ErrorCode::UnknownOp);
    assert_eq!(error.message, "watch_content is unsupported");
}

#[tokio::test]
async fn a_viewer_subscribes_and_receives_content_over_one_connection() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let accept = tokio::spawn(async move {
        AdapterServer::accept_with_timeouts(
            server,
            Duration::from_secs(2),
            Duration::from_secs(2),
            None,
            |hello| {
                assert_eq!(
                    hello.kind,
                    MountKind::Admin,
                    "a viewer mounts admin and declares nothing else"
                );
                assert!(
                    hello.mount.is_none(),
                    "an admin hello carries no mount marker: {:?}",
                    hello.mount
                );
                Ok(ack(vec![]))
            },
        )
        .await
    });
    let viewer =
        AdapterClient::admin_with_timeouts(client, Duration::from_secs(2), Duration::from_secs(2));
    let welcome = viewer.hello_admin("onlyne-view").await.expect("welcome");
    assert_eq!(welcome.role, "planner");
    let connection = accept
        .await
        .expect("accept task")
        .expect("the host took the admin mount");

    let host = Arc::new(WatchHost::default());
    let dispatcher = HostDispatcher::new(MountKind::Admin, host.clone());
    let push = connection.io.clone();
    let serve =
        tokio::spawn(async move { dispatcher.serve(connection.io, connection.inbound).await });

    viewer
        .watch(subscription())
        .await
        .expect("the subscription is accepted on an admin mount");
    assert_eq!(
        *host.subscription.lock().await,
        Some(subscription()),
        "the request travelled as watch_content, fields intact"
    );

    // The host answers by pushing on the connection it registered, and `record`
    // is another program's JSON. A nested object carrying a 2^53-scale integer
    // and non-ASCII text is the payload that any silent normalization would
    // break, and that a TypeScript viewer reads field by field.
    let arbitrary = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "call-1",
        "bytes": 9_007_199_254_740_993u64,
        "raw": [{ "text": "读取任务 — done" }],
    });
    let first = content(42, "task-1", arbitrary);
    push.notify(AdapterMsg::Host(HostOp::Content(Box::new(first.clone()))))
        .await
        .expect("push the record");
    assert_eq!(
        viewer.wait_content().await.expect("the pushed record"),
        first,
        "the frame arrives as it was written"
    );

    let second = content(
        43,
        "task-2",
        json!({"onlyne": {"kind": "turn", "stop_reason": "end_turn"}}),
    );
    push.notify(AdapterMsg::Host(HostOp::Content(Box::new(second.clone()))))
        .await
        .expect("push the next record");
    let streamed = viewer
        .content_stream()
        .next()
        .await
        .expect("the stream route yields the same frames");
    assert_eq!(streamed, second);

    serve.abort();
}
