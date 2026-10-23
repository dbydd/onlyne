//! Host-side dispatch for the adapter: capability accounting, the `Host`
//! contract a plugin mounts, and the dispatcher that routes plugin ops to it.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use onlyne_proto::adapter::HandoffArgs;
use onlyne_proto::{
    AdapterMsg, AssignAckArgs, Delivery, DetachArgs, Envelope, ErrorCode, HealthArgs, HostOp,
    MountKind, OpenArgs, PluginOp, Receipt, RegisterChannelArgs, Report, ResBody,
    SessionRegisterArgs, TypingArgs,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{AdapterIo, Capability, HelloAck, HelloArgs, IncomingFrame, Result};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilitySet {
    caps: Vec<Capability>,
}

impl CapabilitySet {
    pub fn new(caps: impl Into<Vec<Capability>>) -> Self {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for cap in caps.into() {
            if seen.insert(cap) {
                out.push(cap);
            }
        }
        CapabilitySet { caps: out }
    }

    pub fn as_slice(&self) -> &[Capability] {
        &self.caps
    }

    pub fn has(&self, capability: Capability) -> bool {
        self.caps.contains(&capability)
    }

    pub fn missing(&self, expected: &[Capability]) -> Vec<Capability> {
        expected
            .iter()
            .copied()
            .filter(|cap| !self.has(*cap))
            .collect()
    }

    pub fn require(&self, expected: &[Capability]) -> std::result::Result<(), (ErrorCode, String)> {
        let missing = self.missing(expected);
        if missing.is_empty() {
            Ok(())
        } else {
            let names = missing
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            Err((ErrorCode::Forbidden, format!("missing capability: {names}")))
        }
    }
}

impl From<Vec<Capability>> for CapabilitySet {
    fn from(caps: Vec<Capability>) -> Self {
        CapabilitySet::new(caps)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostGap {
    pub capability: Capability,
    pub action: &'static str,
}

pub fn degrade_for(missing: &[Capability]) -> Vec<HostGap> {
    missing
        .iter()
        .filter_map(|capability| match capability {
            Capability::Recycle => Some(HostGap {
                capability: *capability,
                action: "recycle missing means the host judges resource loss through probe",
            }),
            Capability::Report => Some(HostGap {
                capability: *capability,
                action: "report missing means the affected session moves to idle_fault with a recorded fault",
            }),
            Capability::Inject => Some(HostGap {
                capability: *capability,
                action: "assign missing means the payload travels through process stdin or argv and the terminal state comes from the exit code plus the last output line",
            }),
            _ => None,
        })
        .collect()
}

#[async_trait]
pub trait Host: Send + Sync {
    async fn hello(&self, args: &HelloArgs) -> std::result::Result<HelloAck, (ErrorCode, String)>;

    async fn report(&self, _report: &Report) -> std::result::Result<(), (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "report is unsupported".to_string()))
    }

    async fn session_register(
        &self,
        _args: &SessionRegisterArgs,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        Err((
            ErrorCode::UnknownOp,
            "session_register is unsupported".to_string(),
        ))
    }

    async fn assign_ack(
        &self,
        _ack: &AssignAckArgs,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        Err((
            ErrorCode::UnknownOp,
            "assign_ack is unsupported".to_string(),
        ))
    }

    async fn send(
        &self,
        _envelope: &Envelope,
    ) -> std::result::Result<Receipt, (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "send is unsupported".to_string()))
    }

    /// Mint one child of a session's task family and queue it for `args.to`, as
    /// the session's `handoff` frame asks.
    ///
    /// The family's rules stay on the host side: it reads the causality of the
    /// task the session serves and mints the child through `Causality::child_of`,
    /// so the family id, the hop budget, the origin, the deadline, and the labels
    /// ride along. The answer carries the child as
    /// `{"task_id":"<uuid>","hop":3,"queued":true,"op_id":"<uuid>"}`.
    async fn handoff(
        &self,
        _args: &HandoffArgs,
    ) -> std::result::Result<Value, (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "handoff is unsupported".to_string()))
    }

    async fn deliver(&self, _delivery: &Delivery) -> std::result::Result<(), (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "deliver is unsupported".to_string()))
    }

    async fn register_channel(
        &self,
        _args: &RegisterChannelArgs,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        Err((
            ErrorCode::UnknownOp,
            "register_channel is unsupported".to_string(),
        ))
    }

    async fn health(&self, _args: &HealthArgs) -> std::result::Result<(), (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "health is unsupported".to_string()))
    }

    async fn typing(&self, _args: &TypingArgs) -> std::result::Result<(), (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "typing is unsupported".to_string()))
    }

    async fn detach(&self, _args: &DetachArgs) -> std::result::Result<(), (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "detach is unsupported".to_string()))
    }

    /// Answer an op the host originated, for a runtime that owns its own
    /// sessions.
    ///
    /// Most of these travel as notifications, but the ones a host needs an answer
    /// to are requests: a client that asked for a session waits for one, and a
    /// dropped frame is a delivery that never runs. `open` is the first of them.
    /// The default refuses rather than drops, so a runtime that does not
    /// implement one says so instead of leaving the host to time out.
    async fn host_op(&self, op: &HostOp) -> std::result::Result<Value, (ErrorCode, String)> {
        match op {
            HostOp::Open(args) => self.open_session(args).await,
            other => Err((
                ErrorCode::UnknownOp,
                format!("{} is unsupported on this runtime", other.name()),
            )),
        }
    }

    /// Open a session, for a runtime that declared `open` and owns its sessions.
    ///
    /// The host asks rather than spawning because the runtime is already
    /// resident. It names the session it wants; the answer carries the name the
    /// runtime knows the conversation by, and an opaque handle for finding it
    /// again. Nothing composes a history summary for a runtime that cannot
    /// resume — a summary the host wrote is context the model never produced.
    async fn open_session(
        &self,
        _args: &OpenArgs,
    ) -> std::result::Result<Value, (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "open is unsupported".to_string()))
    }
}

pub struct HostDispatcher<H> {
    mount: MountKind,
    host: Arc<H>,
}

impl<H> HostDispatcher<H>
where
    H: Host,
{
    pub fn new(mount: MountKind, host: Arc<H>) -> Self {
        HostDispatcher { mount, host }
    }

    pub async fn dispatch(&self, op: PluginOp) -> ResBody {
        if let Err(message) = self.enforce(&op) {
            return ResBody::err(ErrorCode::Forbidden, message, Some("op".to_string()));
        }
        result_to_body(match op {
            PluginOp::Hello(args) => self.host.hello(&args).await.map(|ack| json!(ack)),
            PluginOp::Report(report) => self.host.report(&report).await.map(|_| Value::Null),
            PluginOp::SessionRegister(args) => {
                self.host.session_register(&args).await.map(|_| Value::Null)
            }
            PluginOp::AssignAck(ack) => self.host.assign_ack(&ack).await.map(|_| Value::Null),
            PluginOp::Send(envelope) => match envelope.validate() {
                Ok(()) => self
                    .host
                    .send(&envelope)
                    .await
                    .map(|receipt| json!(receipt)),
                Err(err) => {
                    return ResBody::err(
                        ErrorCode::Invalid,
                        err.message().to_string(),
                        Some(err.field().to_string()),
                    );
                }
            },
            PluginOp::Handoff(args) => self.host.handoff(&args).await,
            PluginOp::Deliver(delivery) => self.host.deliver(&delivery).await.map(|_| Value::Null),
            PluginOp::RegisterChannel(args) => {
                self.host.register_channel(&args).await.map(|_| Value::Null)
            }
            PluginOp::Health(args) => self.host.health(&args).await.map(|_| Value::Null),
            PluginOp::Typing(args) => self.host.typing(&args).await.map(|_| Value::Null),
            PluginOp::Detach(args) => self.host.detach(&args).await.map(|_| Value::Null),
        })
    }

    pub async fn serve(
        &self,
        io: AdapterIo,
        mut inbound: mpsc::Receiver<IncomingFrame>,
    ) -> Result<()> {
        while let Some(frame) = inbound.recv().await {
            if let AdapterMsg::Host(op) = frame.msg {
                let Some(id) = frame.id else { continue };
                let body = result_to_body(self.host.host_op(&op).await);
                if io.respond(id, body).await.is_err() {
                    break;
                }
            } else if let AdapterMsg::Plugin(op) = frame.msg {
                let body = self.dispatch(op).await;
                if let Some(id) = frame.id {
                    io.respond(id, body).await?;
                } else {
                    io.notify(AdapterMsg::Res(body)).await?;
                }
            }
        }
        Ok(())
    }

    fn enforce(&self, op: &PluginOp) -> std::result::Result<(), String> {
        let allowed = match self.mount {
            MountKind::Agent => matches!(
                op,
                PluginOp::Report(_)
                    | PluginOp::SessionRegister(_)
                    | PluginOp::AssignAck(_)
                    | PluginOp::Send(_)
                    | PluginOp::Handoff(_)
                    | PluginOp::Detach(_)
            ),
            // `bridge` is the wire name for the gateway surface, so it is the
            // gateway's op set under the name the frozen crates speak.
            MountKind::Gateway | MountKind::Bridge => matches!(
                op,
                PluginOp::Deliver(_)
                    | PluginOp::RegisterChannel(_)
                    | PluginOp::Health(_)
                    | PluginOp::Typing(_)
                    | PluginOp::Detach(_)
            ),
            MountKind::Tools => matches!(
                op,
                PluginOp::Send(_)
                    | PluginOp::Handoff(_)
                    | PluginOp::Report(_)
                    | PluginOp::Detach(_)
            ),
            // A cluster mount names a surface that grants nothing on this
            // socket, and an admin probe is not a plugin at all. Both are refused
            // by name rather than left to a default arm, so a new kind cannot
            // inherit the wrong set by being added to the enum and forgotten
            // here.
            MountKind::Cluster | MountKind::Admin => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(format!(
                "op {} forbidden on {:?} mount",
                op.name(),
                self.mount
            ))
        }
    }
}

pub(crate) fn result_to_body<T>(result: std::result::Result<T, (ErrorCode, String)>) -> ResBody
where
    T: serde::Serialize,
{
    match result {
        Ok(value) => ResBody::ok(serde_json::to_value(value).unwrap_or(Value::Null)),
        Err((code, message)) => ResBody::err(code, message, None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceGap {
    Unsupported(&'static str),
    Failed {
        member: &'static str,
        reason: String,
    },
}

impl fmt::Display for SurfaceGap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SurfaceGap::Unsupported(member) => write!(f, "unsupported surface member: {member}"),
            SurfaceGap::Failed { member, reason } => {
                write!(f, "surface member {member} failed: {reason}")
            }
        }
    }
}

impl std::error::Error for SurfaceGap {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeUser {
    pub text: String,
    pub deliver_as: Option<String>,
}

#[async_trait]
pub trait AgentSurface: Send + Sync {
    async fn config_path(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("config_path"))
    }

    async fn register_tool(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("register_tool"))
    }

    async fn register_command(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("register_command"))
    }

    async fn wake_user(&self, _wake: WakeUser) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("wake_user"))
    }

    async fn send_custom_entry(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("send_custom_entry"))
    }

    async fn on_turn_lifecycle(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("on_turn_lifecycle"))
    }

    async fn exit(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("exit"))
    }

    async fn wrap_result(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("wrap_result"))
    }

    async fn set_active_tools(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("set_active_tools"))
    }

    async fn set_status(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("set_status"))
    }

    async fn set_title(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("set_title"))
    }

    async fn set_model(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("set_model"))
    }

    async fn set_thinking_level(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("set_thinking_level"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SurfaceGaps {
    unsupported: Vec<&'static str>,
}

impl SurfaceGaps {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, gap: SurfaceGap) {
        if let SurfaceGap::Unsupported(member) = gap {
            if !self.unsupported.contains(&member) {
                self.unsupported.push(member);
            }
        }
    }

    pub fn unsupported(&self) -> &[&'static str] {
        &self.unsupported
    }

    pub fn report(&self) -> Vec<String> {
        self.unsupported
            .iter()
            .map(|member| format!("agent surface gap: {member}"))
            .collect()
    }
}
