//! The adapter protocol (§7, decision D16).
//!
//! One protocol, mounted twice: an agent plugin connects to its role client's
//! socket, a platform gateway connects to the server's socket. The `hello`
//! handshake carries a [`MountKind`] and the [`Capability`] set, and every later
//! frame is answered by whichever side owns that op.

use crate::envelope::{Envelope, ImagePart, Outcome};
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
    /// ACP tool bridge on a role client socket.
    Tools,
    /// Local operator tooling on the server socket.
    Admin,
    /// Cluster-level tooling on the server socket.
    ///
    /// `Mount::Cluster` has existed since v1 and carried `role`, which is also
    /// what `AgentMount` carries — so under untagged decoding a cluster mount
    /// was told apart from an agent mount only by a field the earlier variant
    /// happened to deny. Naming the kind is what makes that unnecessary.
    Cluster,
    /// The protocol name for a platform bridge, kept after the IM gateway
    /// crates were frozen (`AGENTS.md` §6). It carries a `GatewayMount`: the
    /// wire shape did not change, only the name the kind goes by.
    Bridge,
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
    /// Keep session conversation in the runtime store so its process can be
    /// released and resumed.
    ///
    /// This is a statement about a **spawned** runtime: the client started the
    /// process, the conversation outlives it, and resuming means starting the
    /// same argv again with the session's own key. It is not what a hosting
    /// runtime does — a runtime that was already resident never released the
    /// conversation, so it has nothing to resume. The three below are what
    /// separates the two.
    Resume,
    /// Open a session on request: the host asks this runtime for a conversation
    /// and the runtime names one back.
    ///
    /// A runtime declaring it is **hosting**: it owns its sessions rather than
    /// serving the one the client spawned it for, and the connection it holds is
    /// this role's standing transport instead of one session's. A runtime
    /// declaring none of the three is spawned, and everything the host does with
    /// it is exactly what it did before.
    Open,
    /// Keep a session's process released while its conversation stays where the
    /// runtime keeps it.
    Suspend,
    /// End a session, for the same reason [`Capability::Suspend`] exists and with
    /// the same answer when it cannot: a refusal, never silence. A dropped
    /// `close` reads to the host as a session that will not let go.
    Close,
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
            Capability::Resume => "resume",
            Capability::Open => "open",
            Capability::Suspend => "suspend",
            Capability::Close => "close",
        }
    }

    pub const ALL: [Capability; 11] = [
        Capability::Register,
        Capability::Report,
        Capability::Inject,
        Capability::Recycle,
        Capability::Probe,
        Capability::Typing,
        Capability::Conversations,
        Capability::Resume,
        Capability::Open,
        Capability::Suspend,
        Capability::Close,
    ];

    /// The capabilities that make a runtime **hosting** rather than spawned.
    ///
    /// One of these three is the whole difference between a runtime that serves
    /// the session the client started it for and one that owns its sessions and
    /// is asked for them. The test is a declaration rather than a setting,
    /// because the runtime is the only party that knows: a client that assumed
    /// it could ask a pi process for a second conversation would find out from
    /// silence.
    pub const HOSTING: [Capability; 3] = [Capability::Open, Capability::Suspend, Capability::Close];

    /// Whether this runtime declared that the host may ask it for a session.
    pub fn is_hosting(capabilities: &[Capability]) -> bool {
        Self::HOSTING
            .iter()
            .any(|wanted| capabilities.contains(wanted))
    }
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

/// ACP tool bridge mount data carried in the same `hello` frame.
///
/// The token is the binding: the client mints it for one `(role, session,
/// generation)` and records that association, so the bridge never supplies a
/// role of its own. A caller that could name a role here could speak for a
/// session it never held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolsMount {
    pub token: String,
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
/// beside it in the same args object rather than nested under a tag. `kind` is
/// not decoration: it is what names the variant, so the two are read together by
/// [`HelloArgs`]'s own decoder rather than the enum guessing.
///
/// [`Mount::decode`] is the one place that pairing lives, and it is what every
/// connection on the wire goes through. The `untagged` derive stays for a caller
/// that holds a mount on its own, and it is first-match-wins there: the variant
/// order above is the disambiguation rule for that path, and each payload denies
/// unknown fields, which is what keeps the order a decision rather than a silent
/// mis-decode. `AgentMount` and `ClusterMount` both carry `role`, so without
/// that guard they would be told apart by nothing.
///
/// A bare `Admin` mount serializes as JSON `null`; the sibling `kind` field is
/// what identifies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Mount {
    Agent(AgentMount),
    Gateway(GatewayMount),
    Cluster(ClusterMount),
    Tools(ToolsMount),
    /// Admin tooling needs no mount data.
    Admin,
}

impl Mount {
    /// Read a mount payload as the variant its `kind` names.
    ///
    /// The kind and the payload are one fact split across two fields of the same
    /// args object, and a receiver that needs the kind — which every dispatch
    /// does — cannot get it from the payload. Each payload already denies the
    /// fields its siblings own, so guessing by shape is *safe*: a cluster mount
    /// that also carried a `session` would be refused rather than read as an
    /// agent mount. What shape-guessing cannot do is express a kind at all. A
    /// mount with no name in [`MountKind`] has no wire spelling, and a peer
    /// holding one has to declare a kind it is not.
    ///
    /// An `admin` mount carries no data, so a payload beside it is a peer that
    /// says one thing and means another, and that is worth failing on.
    pub fn decode(kind: MountKind, value: serde_json::Value) -> Result<Mount, serde_json::Error> {
        use serde::de::Error as _;
        use serde_json::Value;
        match (kind, value) {
            (MountKind::Admin, Value::Null) => Ok(Mount::Admin),
            (MountKind::Admin, other) => Err(serde_json::Error::custom(format!(
                "an admin mount carries no payload, and this one carries {other}"
            ))),
            (MountKind::Agent, value) => Ok(Mount::Agent(serde_json::from_value(value)?)),
            (MountKind::Tools, value) => Ok(Mount::Tools(serde_json::from_value(value)?)),
            (MountKind::Cluster, value) => Ok(Mount::Cluster(serde_json::from_value(value)?)),
            // `bridge` is the wire name for what the enum still calls a gateway.
            // The gateway crates are frozen and were not renamed, and renaming
            // the payload with them would be a wire change nobody asked for.
            (MountKind::Gateway | MountKind::Bridge, value) => {
                Ok(Mount::Gateway(serde_json::from_value(value)?))
            }
        }
    }
}

/// `hello` arguments from any adapter connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema, Default)]
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

/// The wire shape, read with the mount left undecoded.
///
/// `kind` has to be read before `mount` can be, and a derived `Deserialize`
/// would decode the fields in the order the derive chooses. The `mount` field
/// therefore stays a `Value` here and is read by the variant its `kind` names.
#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
struct HelloArgsWire {
    protocol: u16,
    plugin: String,
    version: String,
    kind: MountKind,
    capabilities: Vec<Capability>,
    mount: Option<serde_json::Value>,
}

impl<'de> Deserialize<'de> for HelloArgs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        let wire = HelloArgsWire::deserialize(deserializer)?;
        // An absent mount and a null one read the same, which is what an admin
        // probe sends. A non-null payload beside an admin kind is refused by
        // `decode` rather than dropped: a peer that says one thing and means
        // another is worth failing on.
        let mount = match wire.mount {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(Mount::decode(wire.kind, value).map_err(D::Error::custom)?),
        };
        Ok(HelloArgs {
            protocol: wire.protocol,
            plugin: wire.plugin,
            version: wire.version,
            kind: wire.kind,
            capabilities: wire.capabilities,
            mount,
        })
    }
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
    /// Tasks the host has already dispatched to a session of this role.
    ///
    /// A plugin that says `hello` after a restart seeds its own delivery
    /// bookkeeping from this list, so a task the host already handed out is
    /// never injected twice. The list names task ids; it is always an array
    /// and empty when the host holds nothing to report.
    pub delivered_tasks: Vec<String>,
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
    /// Hand the session's task on: the host mints one child of the family the session
    /// serves, queues it for `to`, and answers the child's task id and hop.
    Handoff(HandoffArgs),
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
            PluginOp::Handoff(_) => "handoff",
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
    /// Ask the plugin to hand one client-composed sentence to its agent input.
    ///
    /// This is sent only to a plugin that declared `inject`; the client decides
    /// whether a drive can be nudged. The text is verbatim, and the plugin keeps
    /// no copy, composes none, and does not reset its turn bookkeeping. Its
    /// response claims only that the sentence was handed over.
    Nudge { task_id: String, text: String },
    /// Open a session on a runtime that owns its own.
    ///
    /// Sent only to a connection whose runtime declared `open`, and only when
    /// the client needs a session and would otherwise start a process for it. A
    /// hosting runtime is already resident: asking it is the whole difference
    /// between one process serving a role's sessions and the client spawning
    /// one process per session, which is the shape this capability exists to
    /// avoid.
    ///
    /// The answer carries the `session_id` the runtime knows the conversation
    /// by, and an optional `resume_handle` — whatever *this* runtime needs to
    /// find that conversation again, which the client stores without reading.
    /// A runtime that cannot resume says so by omitting the handle, and the
    /// client opens a fresh conversation rather than composing a history summary
    /// to stand in for one.
    Open(OpenArgs),
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
            HostOp::Nudge { .. } => "nudge",
            HostOp::Open(_) => "open",
            HostOp::Bye(_) => "bye",
        }
    }
}

/// `open`: the host asking a hosting runtime for one session.
///
/// The `scope` travels because it is the runtime's own decision what a session
/// *is* on its side — a fresh conversation, or a family it already holds — and
/// because `max_sessions` is the host's count while the runtime may be serving
/// this role from a pool of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct OpenArgs {
    /// The session the host is opening. The runtime echoes it back, so a
    /// connection carrying several sessions has one name for each.
    pub session_id: String,
    /// The delivery the session is being opened for.
    pub task_id: String,
    /// The scope the role runs in: `oneshot`, `task` or `role`.
    pub scope: String,
    /// The task family, so a `task` scope can hand back the conversation it
    /// already holds rather than opening a second one for one chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// The role's own instruction text, for a runtime that has no instruction
    /// file of its own to put it in.
    #[serde(default)]
    pub prose: String,
    /// The handle this client stored for the family's previous session, so a
    /// `task` scope can hand back the conversation the runtime already holds
    /// instead of opening a second one for one chain. Opaque in both directions:
    /// the host stores what the runtime said and gives it back unread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_handle: Option<String>,
}

/// The answer to [`HostOp::Open`]: the session the runtime opened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub struct OpenedArgs {
    /// The session, as the host named it. A runtime that opened a different one
    /// answers with its own, and the host binds to that.
    pub session_id: String,
    /// Whatever this runtime needs to find the conversation again. Opaque to the
    /// host: it is stored beside the session and handed back on the next
    /// `open` for the same family, and nothing reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_handle: Option<String>,
}

/// `assign` payload: the envelope plus the role prose that frames it.
/// One session handing its task on, as the plugin asks for it.
///
/// The family's own rules stay here: the host reads the task it holds, mints the child
/// through [`Causality::child_of`], and lets the family id, the hop budget, the origin,
/// the deadline, and the labels ride along. A plugin that built the child itself would
/// have to reproduce every one of those rules.
///
/// The answer carries the child: `{ "task_id": "<uuid>", "hop": 3, "queued": true,
/// "op_id": "<uuid>" }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct HandoffArgs {
    /// The task the session is handing on; the child is minted under it.
    pub task_id: String,
    /// Recipient role.
    pub to: String,
    /// Handoff text.
    pub text: String,
    /// One inline image, the same shape a task body carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImagePart>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AssignArgs {
    pub envelope: Box<Envelope>,
    pub prose: String,
    /// The delivery text, rendered by the client's one template
    /// (`onlyne-client`'s `delivery` module).
    ///
    /// A plugin injects these bytes and renders nothing of its own: the wording
    /// of a delivery is a contract (`AGENTS.md` §12), and v1 built it once per
    /// plugin — JavaScript here, Rust in the ACP backend — so the text a model
    /// read depended on which drive delivered it. The envelope still travels
    /// beside it, for the fields a plugin reports on rather than injects.
    pub text: String,
    /// Absolute paths of the files this delivery carries, in the order `text`
    /// names them. The client writes them before the assignment leaves, so
    /// every path here names a file that exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    pub task_id: String,
    pub generation: u64,
    /// The session this delivery is for.
    ///
    /// A runtime that declared `open` opens one conversation and may keep it
    /// across several deliveries, so a task id is not enough to say where the
    /// work goes: the same task retried, or a family served by one conversation
    /// over several turns, both land in one session. The session id is the key
    /// every other lookup uses, and a mount serving several sessions has no other
    /// way to route.
    ///
    /// Absent from a frame written by a host that predates it, and a runtime
    /// reading such a frame serves it as the only session it has — one
    /// connection, one conversation, which is what every runtime did before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The role's `[client.session] scope`, the same lowercase word the config
    /// and `OpenArgs` carry.
    ///
    /// A runtime needs it because the two scopes want opposite things from the
    /// moment a delivery lands. `oneshot` is finished: the runtime should end its
    /// own process, which is also the only way its own store gets flushed —
    /// killing it instead loses whatever the runtime had not yet written. `task`
    /// and `role` want the opposite: the session outlives this delivery, so the
    /// process stays and the conversation is what the next delivery lands in.
    ///
    /// The client cannot decide this for the runtime either way. Retiring a
    /// session keeps its resource while the agent is still reachable, so a
    /// runtime that stays mounted keeps its pane open forever; and closing the
    /// pane from here kills a runtime before it has written its own last words.
    /// So the scope rides the assignment and the runtime acts on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
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
