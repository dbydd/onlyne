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

/// Milliseconds a connection gets to send its `hello` before the host drops it;
/// §7 line 310 fixes the budget at five seconds.
///
/// A connection that sends any other frame first is answered
/// `error{code:"invalid",message:"hello required first"}` and closed, which is
/// the same line's wording and the code the adapter surface uses.
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct GatewayMount {
    pub gateway: String,
    pub platform: String,
}

/// Where a sub-cluster's supervisor claims it speaks for another cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ClusterMount {
    pub cluster: String,
    /// The aggregate role name registered in the parent spec.
    pub role: String,
}

/// Plugin and gateway mount data, discriminated by [`MountKind`].
///
/// The mount names the configured instance the connection serves, and that is the
/// identity the ACL and the routing use: `gateway` holds a `[[gateway]]` id and
/// `role` holds a spec role name, both taken from the server's spec, because one
/// plugin crate can back several configured instances with their own credentials,
/// channels, and `[[route]]` bindings.
///
/// The program name travels in its own field. `HelloArgs::plugin` on this surface
/// and `HandshakeArgs::agent` on a role connection name the crate that connected,
/// which is the thing a protocol or version mismatch is about. Deriving the
/// instance from the program would collapse the several-instances case.
///
/// The enum is untagged, so the wire form is the plan's own `hello` example at
/// §7 line 298 — `"mount":{"role":"planner","session":"8b1c..."}` — with `kind`
/// beside it in the same args object rather than nested under a tag.
///
/// Untagged matching is first-match-wins, so the variant order below is the
/// disambiguation rule: `Agent`, then `Gateway`, then `Cluster`, then `Admin`.
/// Each payload denies unknown fields, and that guard is what makes the order
/// safe to read rather than a silent mis-decode: `AgentMount` and `ClusterMount`
/// both carry `role`, so without it a cluster mount would decode as an agent
/// mount. A new field belongs to the earliest variant that owns it, and making
/// one collide with an earlier variant's set is a wire change this list must be
/// updated for.
///
/// A bare `Admin` mount serializes as JSON `null`; the sibling `kind` field is
/// what identifies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
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
        expected.iter().copied().filter(|c| !self.has(*c)).collect()
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
            PluginOp::Deliver(_)
                | PluginOp::RegisterChannel(_)
                | PluginOp::Health(_)
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
    /// Platform-side id of the message being replied to, opaque to the host and
    /// decoded by the plugin that minted it. §7 line 304 is the inbound
    /// `deliver` frame whose envelope's `causality.reply_to` is the only place
    /// the id can travel, and the host copies it out while rendering, so a
    /// plugin never parses an envelope to thread a reply. Pairs with
    /// [`Self::gateway_ref`], which names the conversation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
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

/// `typing`: platform typing indicator for one conversation. `on` starts the
/// indicator and `false` stops it, which is the pair a platform API exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct TypingArgs {
    pub conversation: String,
    pub on: bool,
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
        assert_eq!(value["args"]["mount"]["role"], "planner");
        assert_eq!(value["args"]["mount"]["session"], "8b1c");
        assert!(
            value["args"]["mount"].get("kind").is_none(),
            "the mount object is flat; `kind` sits beside it in args"
        );
        let back: PluginOp = serde_json::from_value(value).expect("decode");
        assert_eq!(back, hello);
    }

    #[test]
    fn mounts_disambiguate_by_field_set_in_declaration_order() {
        let cluster = Mount::Cluster(ClusterMount {
            cluster: "cluster-b".into(),
            role: "cluster-b".into(),
        });
        let value = serde_json::to_value(&cluster).expect("encode cluster mount");
        assert_eq!(
            value,
            serde_json::json!({"cluster": "cluster-b", "role": "cluster-b"})
        );
        let back: Mount = serde_json::from_value(value).expect("decode cluster mount");
        assert_eq!(
            back, cluster,
            "a cluster mount must not decode as an agent mount"
        );

        let gateway = Mount::Gateway(GatewayMount {
            gateway: "gw1".into(),
            platform: "telegram".into(),
        });
        let value = serde_json::to_value(&gateway).expect("encode gateway mount");
        assert_eq!(
            value,
            serde_json::json!({"gateway": "gw1", "platform": "telegram"})
        );
        assert_eq!(
            serde_json::from_value::<Mount>(value).expect("decode gateway mount"),
            gateway
        );

        let admin: Mount =
            serde_json::from_value(serde_json::json!(null)).expect("decode admin mount");
        assert_eq!(admin, Mount::Admin);
        // A unit variant in an untagged enum writes `null`, which `Option<Mount>`
        // reads back as `None`: a receiver that needs the admin identity reads
        // `HelloArgs::kind` (a `MountKind`), never the absence of a mount.
        assert_eq!(
            serde_json::to_value(Mount::Admin).expect("encode admin mount"),
            serde_json::Value::Null
        );

        serde_json::from_value::<Mount>(serde_json::json!({"cluster": "b"}))
            .expect_err("a mount missing its identifying fields matches no variant");
        serde_json::from_value::<Mount>(serde_json::json!({}))
            .expect_err("an empty mount names nothing and is refused");
    }

    #[test]
    fn every_capability_the_plan_declares_round_trips() {
        // §7 line 298 declares `register`, `report`, `inject`, `recycle`; line
        // 310 turns a missing `report` into an `idle_fault`; line 322 makes
        // `typing` optional and `probe` the fallback for a missing `recycle`. An
        // external TypeScript plugin declares these exact strings against the
        // exported schema, so a string the plan uses must never be refused here.
        for name in ["register", "report", "inject", "recycle", "probe", "typing"] {
            let capability: Capability = serde_json::from_value(serde_json::json!(name))
                .unwrap_or_else(|e| panic!("{name} is a declared capability but refused: {e}"));
            assert_eq!(capability.as_str(), name, "{name} does not round-trip");
        }
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

        let to_plugin = serde_json::json!({"op":"probe","args":null});
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
        assert!(
            err.to_string().contains("data did not match"),
            "err = {err}"
        );
    }

    #[test]
    fn gateway_only_ops_are_marked_and_preauth_is_hello_only() {
        assert!(PluginOp::Health(HealthArgs::default()).is_gateway());
        assert!(
            PluginOp::Deliver(Delivery {
                msg_id: "m".into(),
                envelope: Box::new(note("x")),
            })
            .is_gateway()
        );
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
            cluster_ref: None,
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
            aggregate: Some("cluster-b".into()),
            allowed_targets: vec!["builder".into()],
            allowed_senders: vec!["*".into()],
            session_command: Some(vec!["pi".into(), "--session-id".into(), "{session}".into()]),
            timeout_ready_ms: Some(30_000),
            timeout_running_ms: Some(120_000),
            timeout_idle_ms: Some(60_000),
            intent_attempts: Some(3),
            intent_backoff_ms: Some(vec![1000, 2000, 4000]),
            relay_required: Some(vec!["writer".into()]),
            relay_count: None,
            seq: 41,
        };
        assert_eq!(welcome.role, "planner");
        assert_eq!(welcome.max_sessions, 3);
        assert_eq!(
            SessionProjection::default_working().lifecycle,
            LifecycleMarker::created()
        );
    }

    #[test]
    fn untagged_adapter_msg_resolves_plugin_host_and_response() {
        let plugin = AdapterMsg::Plugin(PluginOp::Report(Report::Heartbeat {
            task_id: new_task_id(),
            generation: 1,
            seq: 14,
            observed: serde_json::json!({"state": "running"}),
            cluster_ref: None,
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
