//! The adapter protocol (§7, decision D16).
//!
//! One protocol, mounted twice: an agent plugin connects to its role client's
//! socket, a platform gateway connects to the server's socket. The `hello`
//! handshake carries a [`MountKind`] and the [`Capability`] set, and every later
//! frame is answered by whichever side owns that op.

use crate::envelope::{Envelope, Outcome};
use crate::event::GatewayHealth;
use crate::frame::ResBody;
use crate::ops::{Delivery, HealthArgs, RegisterChannelArgs, Report, Welcome};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Seconds a connection gets to send its `hello` before the host drops it.
pub const HELLO_TIMEOUT_MS: u64 = 5_000;

/// The exact rejection a host returns when a frame arrives pre-handshake.
pub const HELLO_REQUIRED_MESSAGE: &str = "hello required first";

/// Which side of the split a process mounts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum MountKind {
    /// Agent plugin on a role client socket.
    #[default]
    Agent,
    /// Platform gateway on the server socket.
    Gateway,
    /// Local operator tooling on the server socket.
    Admin,
}

/// Optional plugin abilities. Absence of an entry is a declared gap: the host
/// degrades and records a fault rather than assuming support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Bind a live process to a task via `session_register`.
    Register,
    /// Push lifecycle `report` frames.
    Report,
    /// Accept an injected payload through `assign`.
    Inject,
    /// Honour `recycle` by tearing its own process down.
    Recycle,
    /// Answer `probe` with a fresh observation.
    Probe,
    /// Emit `typing` indicators on the platform.
    Typing,
    /// Enumerate real conversations during `register_channel`.
    Conversations,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Register => "register",
            Capability::Report => "report",
            Capability::Inject => "inject",
            Capability::Recycle => "recycle",
            Capability::Probe => "probe",
            Capability::Typing => "typing",
            Capability::Conversations => "conversations",
        }
    }

    pub const ALL: [Capability; 7] = [
        Capability::Register,
        Capability::Report,
        Capability::Inject,
        Capability::Recycle,
        Capability::Probe,
        Capability::Typing,
        Capability::Conversations,
    ];
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where an agent plugin attaches itself within the role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct AgentMount {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Process id, used for probe cross-checks on the local machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// Gateway-side mount data carried in the same `hello` frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct GatewayMount {
    pub gateway: String,
    pub platform: String,
}

/// Where a sub-cluster's supervisor claims it speaks for another cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ClusterMount {
    pub cluster: String,
    /// The aggregate role name registered in the parent spec.
    pub role: String,
}

/// Plugin and gateway mount data, discriminated by [`MountKind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum Mount {
    Agent(AgentMount),
    Gateway(GatewayMount),
    Cluster(ClusterMount),
    /// Admin tooling needs no mount data.
    Admin,
}

/// `hello` arguments from any adapter connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct HelloArgs {
    pub protocol: u16,
    /// Plugin or process name, e.g. `onlyne-agent-pi`.
    pub plugin: String,
    /// Its own semver, recorded for fault triage.
    pub version: String,
    pub kind: MountKind,
    pub capabilities: Vec<Capability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount: Option<Mount>,
}

impl HelloArgs {
    /// `true` when the plugin declared `capability`.
    pub fn has(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// The declared gaps against `expected`, in declaration order.
    pub fn missing(&self, expected: &[Capability]) -> Vec<Capability> {
        expected
            .iter()
            .copied()
            .filter(|c| !self.has(*c))
            .collect()
    }
}

/// Everything the host answers a successful `hello` with (§5, §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct HelloAck {
    pub protocol: u16,
    /// The role this connection is bound to.
    pub role: String,
    /// Session id assigned or confirmed by the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub generation: u64,
    pub prose: String,
    pub server: ServerInfo,
    /// Capabilities the host itself will exercise against this plugin.
    pub host_capabilities: Vec<Capability>,
}

/// Cluster identity handed out at handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ServerInfo {
    pub connected: bool,
    pub cluster: String,
    pub name: String,
}

/// Plugin and gateway frames, in the direction plugin to host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum PluginOp {
    /// First frame on the connection.
    Hello(HelloArgs),
    /// A lifecycle observation for the reducer (§6).
    Report(Report),
    /// Bind this process to a task and session.
    SessionRegister(SessionRegisterArgs),
    /// Answer to [`HostOp::Assign`].
    AssignAck(AssignAckArgs),
    /// Submit an envelope for routing.
    Send(Box<Envelope>),
    /// Inbound platform event (gateway mounts only).
    Deliver(Delivery),
    /// Channel and conversation declarations (gateway mounts only).
    RegisterChannel(RegisterChannelArgs),
    /// Platform health (gateway mounts only).
    Health(HealthArgs),
    /// Typing indicator for one conversation (gateway mounts only).
    Typing(TypingArgs),
    /// Stop receiving work; the plugin is leaving while its session continues.
    Detach(DetachArgs),
}

impl PluginOp {
    pub fn name(&self) -> &'static str {
        match self {
            PluginOp::Hello(_) => "hello",
            PluginOp::Report(_) => "report",
            PluginOp::SessionRegister(_) => "session_register",
            PluginOp::AssignAck(_) => "assign_ack",
            PluginOp::Send(_) => "send",
            PluginOp::Deliver(_) => "deliver",
            PluginOp::RegisterChannel(_) => "register_channel",
            PluginOp::Health(_) => "health",
            PluginOp::Typing(_) => "typing",
            PluginOp::Detach(_) => "detach",
        }
    }

    /// Frames the host accepts before `hello`.
    pub fn is_pre_auth(self) -> bool {
        matches!(self, PluginOp::Hello(_))
    }

    /// Frames reserved for a gateway mount.
    pub fn is_gateway(self) -> bool {
        matches!(
            self,
            PluginOp::Deliver(_) | PluginOp::RegisterChannel(_) | PluginOp::Health(_)
                | PluginOp::Typing(_)
        )
    }
}

/// Host frames, in the direction host to plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum HostOp {
    /// Answer to `hello`, carrying the role's spec slice.
    Welcome(HelloAck),
    /// Hand this payload to the agent for the task.
    Assign(AssignArgs),
    /// Render and send this envelope on the platform (gateway mounts).
    RenderSend(RenderSendArgs),
    /// Ask for a fresh observation right now.
    Probe(Value),
    /// Tear the session down and release its slot.
    Recycle(RecycleArgs),
    /// Read one host config key.
    ConfigGet(ConfigGetArgs),
    /// The host is going away.
    Bye(ByeNotice),
}

impl HostOp {
    pub fn name(&self) -> &'static str {
        match self {
            HostOp::Welcome(_) => "welcome",
            HostOp::Assign(_) => "assign",
            HostOp::RenderSend(_) => "render_send",
            HostOp::Probe(_) => "probe",
            HostOp::Recycle(_) => "recycle",
            HostOp::ConfigGet(_) => "config_get",
            HostOp::Bye(_) => "bye",
        }
    }
}

/// `assign` payload: the envelope plus the role prose that frames it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AssignArgs {
    pub envelope: Box<Envelope>,
    pub prose: String,
    pub task_id: String,
    pub generation: u64,
    /// Envelope of the task that caused this one, when downstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<Box<Envelope>>,
}

/// `assign_ack`: whether the plugin took the payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct AssignAckArgs {
    pub task_id: String,
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `session_register`: bind a live process to a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SessionRegisterArgs {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

/// `render_send`: one envelope for the platform, plus the local correlation the
/// gateway needs to answer it later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RenderSendArgs {
    pub envelope: Box<Envelope>,
    /// Platform target the gateway must address.
    pub conversation: String,
    /// Opaque handle stored in the gateway's own table (§10.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_ref: Option<String>,
}

/// `recycle`: reason is mandatory so the ledger records the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct RecycleArgs {
    pub task_id: String,
    pub reason: String,
    /// Settle the task with this outcome once the resource is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

/// `config_get`: dotted key into the role's local config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ConfigGetArgs {
    pub key: String,
}

/// `typing`: platform typing indicator for one conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct TypingArgs {
    pub conversation: String,
    /// Seconds the indicator should hold; the platform clamps it.
    pub seconds: u32,
}

/// `detach`: the plugin leaves; the reason is recorded verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct DetachArgs {
    pub reason: String,
}

/// `bye` from a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ByeNotice {
    pub reason: String,
}

/// Anything crossing an adapter socket in either direction.
///
/// Requests carry an `op`; replies carry `ok`. The variants are tried in this
/// order and their `op` vocabularies are disjoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AdapterMsg {
    Plugin(PluginOp),
    Host(HostOp),
    Res(ResBody),
}

impl AdapterMsg {
    pub fn direction(&self) -> MsgDirection {
        match self {
            AdapterMsg::Plugin(_) => MsgDirection::ToHost,
            AdapterMsg::Host(_) => MsgDirection::ToPlugin,
            AdapterMsg::Res(body) => {
                if body.ok {
                    MsgDirection::Response
                } else {
                    MsgDirection::ErrorResponse
                }
            }
        }
    }

    /// The op name for logging, when the message names one.
    pub fn op_name(&self) -> Option<&str> {
        match self {
            AdapterMsg::Plugin(op) => Some(op.name()),
            AdapterMsg::Host(op) => Some(op.name()),
            AdapterMsg::Res(_) => None,
        }
    }
}

/// Which way an adapter message flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MsgDirection {
    ToHost,
    ToPlugin,
    Response,
    ErrorResponse,
}

/// Gateway-side conversation binding announced during `register_channel`, kept
/// so the server can route a `render_send` back to a platform target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct GatewayBinding {
    pub gateway: String,
    pub channel: String,
    pub conversation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Health state helper for the gateway side, avoiding stringly comparison at
/// call sites that already have a [`GatewayHealth`].
impl From<GatewayHealth> for HealthArgs {
    fn from(state: GatewayHealth) -> Self {
        HealthArgs {
            state: state.as_str().to_string(),
            detail: None,
            uptime_s: 0,
        }
    }
}

/// The `welcome` a host replies with, lifted into the adapter vocabulary for
/// agents that need the whole spec slice.
pub type WelcomeSlice = Welcome;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Body, MsgKind, Principal, new_envelope, new_task_id};
    use crate::ops::SessionProjection;

    fn note(text: &str) -> Envelope {
        new_envelope(
            MsgKind::Note,
            Principal::role("planner"),
            Principal::gateway("tg1", "telegram", Some("42".into())),
            Body::text(text),
            None,
        )
        .expect("note")
    }

    #[test]
    fn hello_matches_the_documented_shape() {
        let hello = PluginOp::Hello(HelloArgs {
            protocol: crate::PROTOCOL_VERSION,
            plugin: "onlyne-agent-pi".into(),
            version: "1.0.0".into(),
            kind: MountKind::Agent,
            capabilities: vec![
                Capability::Register,
                Capability::Report,
                Capability::Inject,
                Capability::Recycle,
            ],
            mount: Some(Mount::Agent(AgentMount {
                role: "planner".into(),
                session: Some("8b1c".into()),
                task_id: None,
                pid: Some(4212),
            })),
        });
        let value = serde_json::to_value(&hello).expect("encode");
        assert_eq!(value["op"], "hello");
        assert_eq!(value["args"]["kind"], "agent");
        assert_eq!(value["args"]["capabilities"][0], "register");
        assert_eq!(value["args"]["mount"]["kind"], "agent");
        assert_eq!(value["args"]["mount"]["data"]["role"], "planner");
        let back: PluginOp = serde_json::from_value(value).expect("decode");
        assert_eq!(back, hello);
    }

    #[test]
    fn capability_gap_is_reported_in_declaration_order() {
        let hello = HelloArgs {
            protocol: crate::PROTOCOL_VERSION,
            plugin: "onlyne-cli-agent".into(),
            version: "1.0.0".into(),
            kind: MountKind::Agent,
            capabilities: vec![Capability::Report],
            mount: None,
        };
        assert_eq!(
            hello.missing(&[Capability::Report, Capability::Recycle, Capability::Inject]),
            vec![Capability::Recycle, Capability::Inject]
        );
        assert!(!hello.has(Capability::Probe));
    }

    #[test]
    fn untagged_adapter_msg_reads_both_directions() {
        let to_host = serde_json::json!({"op":"send","args":note("hi")});
        let msg: AdapterMsg = serde_json::from_value(to_host).expect("plugin op decodes");
        assert_eq!(msg.direction(), MsgDirection::ToHost);
        assert_eq!(msg.op_name(), Some("send"));

        let to_plugin =
            serde_json::json!({"op":"probe","args":null});
        let msg: AdapterMsg = serde_json::from_value(to_plugin).expect("host op decodes");
        assert_eq!(msg.direction(), MsgDirection::ToPlugin);
        assert_eq!(msg.op_name(), Some("probe"));

        let reply = serde_json::json!({"ok":false,"error":{"code":"invalid","message":"x"}});
        let msg: AdapterMsg = serde_json::from_value(reply).expect("res decodes");
        assert_eq!(msg.direction(), MsgDirection::ErrorResponse);
        assert_eq!(msg.op_name(), None);
    }

    #[test]
    fn a_frame_that_is_not_an_op_is_rejected() {
        let err = serde_json::from_value::<AdapterMsg>(serde_json::json!({"type":"noise"}))
            .expect_err("unrecognised");
        assert!(err.to_string().contains("data did not match"), "err = {err}");
    }

    #[test]
    fn gateway_only_ops_are_marked_and_preauth_is_hello_only() {
        assert!(PluginOp::Health(HealthArgs::default()).is_gateway());
        assert!(PluginOp::Deliver(Delivery {
            msg_id: "m".into(),
            envelope: Box::new(note("x")),
        })
        .is_gateway());
        assert!(!PluginOp::Send(Box::new(note("y"))).is_gateway());
        assert!(PluginOp::Hello(HelloArgs::default()).is_pre_auth());
        assert!(!PluginOp::Send(Box::new(note("y"))).is_pre_auth());
    }

    #[test]
    fn assign_carries_prose_alongside_the_envelope() {
        let args = AssignArgs {
            envelope: Box::new(note("do it")),
            prose: "Read the incoming task".into(),
            task_id: new_task_id(),
            generation: 1,
            parent: None,
        };
        let value = serde_json::to_value(HostOp::Assign(args.clone())).expect("encode");
        assert_eq!(value["op"], "assign");
        assert_eq!(value["args"]["prose"], "Read the incoming task");
        assert!(value["args"].get("parent").is_none());
        let back: HostOp = serde_json::from_value(value).expect("decode");
        assert_eq!(back, HostOp::Assign(args));
    }

    #[test]
    fn report_frames_use_the_lifecycle_kind() {
        let op = PluginOp::Report(Report::Heartbeat {
            task_id: new_task_id(),
            generation: 1,
            seq: 14,
            observed: serde_json::json!({"state": "running"}),
        });
        let value = serde_json::to_value(&op).expect("encode");
        assert_eq!(value["op"], "report");
        assert_eq!(value["args"]["kind"], "heartbeat");
        assert_eq!(value["args"]["data"]["seq"], 14);
        let back: PluginOp = serde_json::from_value(value).expect("decode");
        assert_eq!(back, op);
    }

    #[test]
    fn session_register_accepts_a_missing_task_binding() {
        let args = SessionRegisterArgs {
            session_id: "8b1c".into(),
            pid: Some(4212),
            generation: 1,
            title: Some("swarm:planner:8b1c".into()),
            task_id: None,
        };
        let value = serde_json::to_value(PluginOp::SessionRegister(args)).expect("encode");
        assert_eq!(value["op"], "session_register");
        assert!(value["args"].get("task_id").is_none());
    }

    #[test]
    fn detach_and_bye_record_their_reasons() {
        let detach = PluginOp::Detach(DetachArgs {
            reason: "operator".into(),
        });
        assert_eq!(
            serde_json::to_value(&detach).expect("encode")["args"]["reason"],
            "operator"
        );
        let bye = HostOp::Bye(ByeNotice {
            reason: "shutdown".into(),
        });
        assert_eq!(bye.name(), "bye");
        assert_eq!(
            serde_json::to_value(&bye).expect("encode")["args"]["reason"],
            "shutdown"
        );
    }

    #[test]
    fn health_maps_from_gateway_health() {
        let args: HealthArgs = GatewayHealth::Reconnecting.into();
        assert_eq!(args.state, "reconnecting");
        assert_eq!(args.uptime_s, 0);
        assert!(args.detail.is_none());
    }

    #[test]
    fn projection_travels_inside_the_welcome_slice() {
        let welcome = Welcome {
            cluster: "cluster-a".into(),
            server: "srv".into(),
            role: "planner".into(),
            admin: false,
            max_sessions: 3,
            reuse: true,
            prose: "Read the incoming task".into(),
            spec_hash: "abc".into(),
            allowed_targets: vec!["builder".into()],
            allowed_senders: vec!["*".into()],
            session_command: Some(vec!["pi".into(), "--session-id".into(), "{session}".into()]),
            timeout_ready_ms: Some(30_000),
            timeout_running_ms: Some(120_000),
            timeout_idle_ms: Some(60_000),
            intent_attempts: Some(3),
            intent_backoff_ms: Some(vec![1000, 2000, 4000]),
            seq: 41,
        };
        assert_eq!(welcome.role, "planner");
        assert_eq!(welcome.max_sessions, 3);
        assert_eq!(SessionProjection::default_working().lifecycle, LifecycleMarker::created());
    }

    #[test]
    fn untagged_adapter_msg_resolves_plugin_host_and_response() {
        let plugin = AdapterMsg::Plugin(PluginOp::Report(Report::Heartbeat {
            task_id: new_task_id(),
            generation: 1,
            seq: 14,
            observed: serde_json::json!({"state": "running"}),
        }));
        let value = serde_json::to_value(&plugin).expect("encode plugin op");
        assert_eq!(value["op"], "report");
        let back: AdapterMsg = serde_json::from_value(value).expect("decode plugin op");
        assert_eq!(back, plugin);
        assert_eq!(back.direction(), MsgDirection::ToHost);
        assert_eq!(back.op_name(), Some("report"));

        let host = AdapterMsg::Host(HostOp::Recycle(RecycleArgs {
            task_id: new_task_id(),
            reason: "retire".into(),
            outcome: Some(Outcome::Done),
        }));
        let value = serde_json::to_value(&host).expect("encode host op");
        assert_eq!(value["op"], "recycle");
        let back: AdapterMsg = serde_json::from_value(value).expect("decode host op");
        assert_eq!(back, host);
        assert_eq!(back.direction(), MsgDirection::ToPlugin);
        assert_eq!(back.op_name(), Some("recycle"));

        let res = AdapterMsg::Res(ResBody::ok(serde_json::json!({"state": "in_flight"})));
        let value = serde_json::to_value(&res).expect("encode res body");
        assert_eq!(value["ok"], true);
        let back: AdapterMsg = serde_json::from_value(value).expect("decode res body");
        assert_eq!(back, res);
        assert_eq!(back.direction(), MsgDirection::Response);
        assert_eq!(back.op_name(), None);
    }

    struct LifecycleMarker;
    impl LifecycleMarker {
        fn created() -> crate::event::Lifecycle {
            crate::event::Lifecycle::Created
        }
    }
}
