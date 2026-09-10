//! The unified message format (§3).
//!
//! One envelope travels every edge of the system: role to role, human IM to
//! role, admin to role. A body carries text plus at most one inline image, and
//! every other attachment shape stays outside the core (decision D4).

use crate::{OP_ID_PREFIX, PROTOCOL_VERSION};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use uuid::Uuid;

/// UTF-8 ceiling for [`Body::text`].
pub const BODY_TEXT_MAX_BYTES: usize = 1024 * 1024;

/// Decoded ceiling for [`ImagePart::data_base64`].
pub const IMAGE_DATA_MAX_BYTES: usize = 2 * 1024 * 1024;

/// The image types the core accepts, in stable order.
pub const IMAGE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Process-local identity of a message sender.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Principal {
    /// A role in the cluster, optionally narrowed to one of its sessions.
    Role {
        role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// An external IM conversation reached through a gateway.
    Gateway {
        gateway: String,
        channel: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<String>,
    },
    /// A subordinate cluster, used by admin and aggregate reporting only.
    Cluster { cluster: String },
}

impl Principal {
    pub fn role(role: impl Into<String>) -> Self {
        Principal::Role {
            role: role.into(),
            session: None,
        }
    }

    pub fn role_session(role: impl Into<String>, session: impl Into<String>) -> Self {
        Principal::Role {
            role: role.into(),
            session: Some(session.into()),
        }
    }

    pub fn gateway(
        gateway: impl Into<String>,
        channel: impl Into<String>,
        conversation: Option<String>,
    ) -> Self {
        Principal::Gateway {
            gateway: gateway.into(),
            channel: channel.into(),
            conversation,
        }
    }

    /// The role name, when this principal names one.
    pub fn role_name(&self) -> Option<&str> {
        match self {
            Principal::Role { role, .. } => Some(role),
            Principal::Gateway { .. } | Principal::Cluster { .. } => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Principal::Role { .. } => "role",
            Principal::Gateway { .. } => "gateway",
            Principal::Cluster { .. } => "cluster",
        }
    }
}

impl fmt::Display for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Principal::Role { role, session } => match session {
                Some(s) => write!(f, "{role}#{s}"),
                None => write!(f, "{role}"),
            },
            Principal::Gateway {
                gateway,
                channel,
                conversation,
            } => match conversation {
                Some(c) => write!(f, "gw:{gateway}:{channel}:{c}"),
                None => write!(f, "gw:{gateway}:{channel}"),
            },
            Principal::Cluster { cluster } => write!(f, "cluster:{cluster}"),
        }
    }
}

/// Delivery intent and queueing class (decision D11, D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MsgKind {
    /// Hand work to a role, creating or reusing a session.
    Task,
    /// Terminal receipt for a task.
    Completion,
    /// Free text. Never queued, never creates a session.
    Note,
    /// Lifecycle command over a task: recycle, probe, snapshot, cancel.
    Control,
}

impl MsgKind {
    /// Kinds carrying a causality chain.
    pub fn is_control_plane(self) -> bool {
        matches!(self, MsgKind::Task | MsgKind::Completion | MsgKind::Control)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MsgKind::Task => "task",
            MsgKind::Completion => "completion",
            MsgKind::Note => "note",
            MsgKind::Control => "control",
        }
    }
}

impl fmt::Display for MsgKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A command over an existing task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum ControlOp {
    /// Tear the session down and release its slot.
    Recycle { task_id: String, reason: String },
    /// Ask the owning client for a fresh probe of the resource.
    Probe { task_id: String },
    /// Ask for the full lifecycle projection right now.
    Snapshot { task_id: String },
    /// Abandon the task and settle it as cancelled.
    Cancel { task_id: String, reason: String },
}

impl ControlOp {
    pub fn task_id(&self) -> &str {
        match self {
            ControlOp::Recycle { task_id, .. }
            | ControlOp::Probe { task_id }
            | ControlOp::Snapshot { task_id }
            | ControlOp::Cancel { task_id, .. } => task_id,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ControlOp::Recycle { .. } => "recycle",
            ControlOp::Probe { .. } => "probe",
            ControlOp::Snapshot { .. } => "snapshot",
            ControlOp::Cancel { .. } => "cancel",
        }
    }
}

/// Task result dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Failed,
    Cancelled,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Failed => "failed",
            Outcome::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One inline image, carried base64 inside the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ImagePart {
    pub data_base64: String,
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ImagePart {
    /// Decode without re-encoding, used by validators and renderers.
    pub fn decode(&self) -> Result<Vec<u8>> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.data_base64)
            .map_err(|e| Error::invalid("body.image.data_base64", format!("bad base64: {e}")))?;
        Ok(bytes)
    }
}

/// Envelope payload: text plus at most one inline image.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default)]
pub struct Body {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImagePart>,
}

impl Body {
    pub fn text(text: impl Into<String>) -> Self {
        Body {
            text: Some(text.into()),
            image: None,
        }
    }

    pub fn image(data_base64: impl Into<String>, mime: impl Into<String>) -> Self {
        Body {
            text: None,
            image: Some(ImagePart {
                data_base64: data_base64.into(),
                mime: mime.into(),
                name: None,
            }),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.image.is_none()
    }

    fn validate(&self) -> Result<()> {
        if self.is_empty() {
            return Err(Error::invalid(
                "body",
                "body requires text or image".to_string(),
            ));
        }
        if let Some(text) = &self.text {
            let len = text.len();
            if len > BODY_TEXT_MAX_BYTES {
                return Err(Error::invalid(
                    "body.text",
                    format!("text exceeds {BODY_TEXT_MAX_BYTES} bytes"),
                ));
            }
        }
        if let Some(image) = &self.image {
            if !IMAGE_MIMES.contains(&image.mime.as_str()) {
                return Err(Error::invalid(
                    "body.image.mime",
                    format!("mime must be one of: {}", IMAGE_MIMES.join(", ")),
                ));
            }
            let decoded = image.decode()?;
            if decoded.len() > IMAGE_DATA_MAX_BYTES {
                return Err(Error::invalid(
                    "body.image.data_base64",
                    format!("image exceeds {} bytes", IMAGE_DATA_MAX_BYTES),
                ));
            }
        }
        Ok(())
    }
}

/// Causality chain over one task family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct Causality {
    /// Task family id, uuid v4, minted when the work is created.
    pub task: String,
    /// The task that caused this one, when downstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task: Option<String>,
    /// Envelope id being replied to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Hop count from the root task.
    pub hop: u32,
    /// Redelivery count for this envelope.
    pub attempt: u32,
}

impl Causality {
    pub fn root(task: impl Into<String>) -> Self {
        Causality {
            task: task.into(),
            parent_task: None,
            reply_to: None,
            hop: 0,
            attempt: 0,
        }
    }

    /// Derive the child task link for a downstream send.
    pub fn child_of(&self) -> Causality {
        Causality {
            task: new_task_id(),
            parent_task: Some(self.task.clone()),
            reply_to: None,
            hop: self.hop + 1,
            attempt: 0,
        }
    }
}

/// The single message shape for the whole system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Envelope {
    pub protocol: u16,
    /// Sender-minted uuid v4.
    pub id: String,
    /// Idempotency key: mandatory for Task, Completion, Control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,
    pub kind: MsgKind,
    pub from: Principal,
    pub to: Principal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ControlOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causality: Option<Causality>,
    pub body: Body,
    pub ts: DateTime<Utc>,
    /// Notes expire; a queued note past its deadline is settled `expired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    /// Set when the sender holds `admin = true` in the spec.
    pub admin: bool,
}

/// Build a validated envelope in one step.
///
/// Panics only on the impossible (a uuid that will not format); every protocol
/// rule is enforced by [`Envelope::validate`] inside.
pub fn new_envelope(
    kind: MsgKind,
    from: Principal,
    to: Principal,
    body: Body,
    causality: Option<Causality>,
) -> Result<Envelope> {
    let env = Envelope {
        protocol: PROTOCOL_VERSION,
        id: new_id(),
        op_id: match kind {
            MsgKind::Note => None,
            _ => Some(new_op_id()),
        },
        kind,
        from,
        to,
        control: None,
        causality,
        body,
        ts: Utc::now(),
        ttl_ms: None,
        admin: false,
    };
    env.validate()?;
    Ok(env)
}

impl Envelope {

    /// Copy with an incremented `causality.attempt`, keeping `op_id` intact so a
    /// retry stays idempotent.
    pub fn redelivered(&self) -> Envelope {
        let mut next = self.clone();
        if let Some(causality) = &mut next.causality {
            causality.attempt += 1;
        }
        next
    }

    /// The task this envelope belongs to, when it carries causality.
    pub fn task_id(&self) -> Option<&str> {
        self.causality.as_ref().map(|c| c.task.as_str())
    }

    /// Validate every protocol rule, naming the offending field.
    pub fn validate(&self) -> Result<()> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(Error::invalid(
                "protocol",
                format!("protocol {} unsupported, expected {PROTOCOL_VERSION}", self.protocol),
            ));
        }
        if Uuid::parse_str(&self.id).is_err() {
            return Err(Error::invalid("id", "id must be a uuid".to_string()));
        }
        match self.kind {
            MsgKind::Note => {}
            other => {
                let Some(op_id) = &self.op_id else {
                    return Err(Error::invalid(
                        "op_id",
                        format!("op_id is required for kind {other}"),
                    ));
                };
                validate_op_id(op_id)?;
            }
        }
        if self.from.role_name() == Some("") {
            return Err(Error::invalid("from.role", "role name must not be empty".to_string()));
        }
        if self.to.role_name() == Some("") {
            return Err(Error::invalid("to.role", "role name must not be empty".to_string()));
        }
        self.body.validate()?;
        match self.kind {
            MsgKind::Control => {
                if self.control.is_none() {
                    return Err(Error::invalid(
                        "control",
                        "control kind requires a control op".to_string(),
                    ));
                }
            }
            MsgKind::Note | MsgKind::Task | MsgKind::Completion => {
                if self.control.is_some() {
                    return Err(Error::invalid(
                        "control",
                        format!("control is reserved for kind {}", MsgKind::Control),
                    ));
                }
            }
        }
        if self.kind.is_control_plane() && self.causality.is_none() {
            return Err(Error::invalid(
                "causality",
                format!("causality is required for kind {}", self.kind),
            ));
        }
        if let Some(causality) = &self.causality {
            if Uuid::parse_str(&causality.task).is_err() {
                return Err(Error::invalid(
                    "causality.task",
                    "task must be a uuid".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Idempotency fingerprint: the canonical JSON of this envelope with `id`,
    /// `ts`, and `op_id` removed, hashed with SHA-256. The redelivery counter
    /// folds to zero, so a retry of one intent hashes identically.
    pub fn fingerprint(&self) -> String {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(map) = value.as_object_mut() {
            map.remove("id");
            map.remove("ts");
            map.remove("op_id");
        }
        if let Some(causality) = value.get_mut("causality").and_then(|c| c.as_object_mut()) {
            causality.insert("attempt".to_string(), serde_json::Value::from(0));
        }
        let bytes = serde_json::to_vec(&value).unwrap_or_default();
        sha256_hex(&bytes)
    }
}

/// SHA-256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from_digit((byte >> 4) as u32, 16).expect("hex digit"));
        out.push(char::from_digit((byte & 0xf) as u32, 16).expect("hex digit"));
    }
    out
}

/// The one spelling of an idempotency key, shared by every sender.
///
/// A uuid v4 is minted once per logical send, at the moment the intent is
/// created; a retry of that intent reuses the stored value.
pub fn new_op_id() -> String {
    format!("{OP_ID_PREFIX}{}", Uuid::new_v4())
}

/// A bare uuid v4, used for envelope ids.
pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// A bare uuid v4 task family id.
pub fn new_task_id() -> String {
    Uuid::new_v4().to_string()
}

fn validate_op_id(op_id: &str) -> Result<()> {
    let Some(rest) = op_id.strip_prefix(OP_ID_PREFIX) else {
        return Err(Error::invalid(
            "op_id",
            format!("op_id must start with {OP_ID_PREFIX}"),
        ));
    };
    if Uuid::parse_str(rest).is_err() {
        return Err(Error::invalid("op_id", "op_id must carry a uuid".to_string()));
    }
    Ok(())
}

/// The protocol's own error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A field failed a protocol rule.
    Invalid { field: String, message: String },
}

impl Error {
    pub fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
        Error::Invalid {
            field: field.into(),
            message: message.into(),
        }
    }

    pub fn field(&self) -> &str {
        match self {
            Error::Invalid { field, .. } => field,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Error::Invalid { message, .. } => message,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid { field, message } => write!(f, "{field}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    fn note(text: &str) -> Envelope {
        new_envelope(
            MsgKind::Note,
            Principal::role("planner"),
            Principal::role("builder"),
            Body::text(text),
            None,
        )
        .expect("valid note")
    }

    fn task(text: &str) -> Envelope {
        new_envelope(
            MsgKind::Task,
            Principal::role("planner"),
            Principal::role("builder"),
            Body::text(text),
            Some(Causality::root(new_task_id())),
        )
        .expect("valid task")
    }

    #[test]
    fn note_round_trips_through_json() {
        let env = note("hello v1");
        let json = serde_json::to_string(&env).expect("to json");
        let back: Envelope = serde_json::from_str(&json).expect("from json");
        assert_eq!(env, back);
        assert_eq!(json.matches("\"kind\":\"note\"").count(), 1);
        assert!(!json.contains("op_id"), "note carries no op_id: {json}");
    }

    #[test]
    fn task_round_trips_with_causality_and_op_id() {
        let env = task("build it");
        let back: Envelope =
            serde_json::from_str(&serde_json::to_string(&env).expect("json")).expect("back");
        assert_eq!(back, env);
        assert!(back.op_id.as_ref().expect("op_id").starts_with("o-"));
        assert_eq!(back.causality.as_ref().expect("caus").attempt, 0);
    }

    #[test]
    fn control_kind_requires_its_op() {
        let mut env = task("x");
        env.kind = MsgKind::Control;
        let err = env.validate().expect_err("control without op");
        assert_eq!(
            err,
            Error::Invalid {
                field: "control".into(),
                message: "control kind requires a control op".into()
            }
        );
        env.control = Some(ControlOp::Probe {
            task_id: env.causality.as_ref().unwrap().task.clone(),
        });
        env.validate().expect("control with op");
    }

    #[test]
    fn empty_body_is_refused() {
        let mut env = note("x");
        env.body = Body::default();
        assert_eq!(
            env.validate().unwrap_err().message(),
            "body requires text or image"
        );
    }

    #[test]
    fn oversized_text_is_refused_with_its_ceiling() {
        let mut env = note("x");
        env.body.text = Some("y".repeat(BODY_TEXT_MAX_BYTES + 1));
        let err = env.validate().unwrap_err();
        assert_eq!(err.field(), "body.text");
        assert_eq!(err.message(), "text exceeds 1048576 bytes");
    }

    #[test]
    fn oversized_image_carries_the_verbatim_ceiling() {
        let payload = vec![0u8; IMAGE_DATA_MAX_BYTES + 1];
        let mut env = note("x");
        env.body = Body::image(STANDARD.encode(&payload), "image/png");
        let err = env.validate().unwrap_err();
        assert_eq!(err.field(), "body.image.data_base64");
        assert_eq!(err.message(), "image exceeds 2097152 bytes");
    }

    #[test]
    fn unsupported_mime_is_refused() {
        let mut env = note("x");
        env.body = Body::image(STANDARD.encode(b"tiny"), "image/tiff");
        let err = env.validate().unwrap_err();
        assert_eq!(err.field(), "body.image.mime");
        assert_eq!(
            err.message(),
            "mime must be one of: image/png, image/jpeg, image/gif, image/webp"
        );
    }

    #[test]
    fn control_plane_kinds_require_causality_and_op_id() {
        let mut env = task("x");
        env.causality = None;
        assert_eq!(env.validate().unwrap_err().field(), "causality");
        let mut env = task("x");
        env.op_id = None;
        let err = env.validate().unwrap_err();
        assert_eq!(err.field(), "op_id");
        assert_eq!(err.message(), "op_id is required for kind task");
        let mut env = task("x");
        env.op_id = Some("op-1".to_string());
        assert_eq!(
            env.validate().unwrap_err().message(),
            "op_id must start with o-"
        );
    }

    #[test]
    fn redelivery_keeps_op_id_and_bumps_attempt() {
        let env = task("x");
        let next = env.redelivered();
        assert_eq!(next.op_id, env.op_id);
        assert_eq!(next.id, env.id);
        assert_eq!(next.causality.as_ref().unwrap().attempt, 1);
        assert_eq!(env.fingerprint(), next.fingerprint());
    }

    #[test]
    fn fingerprint_ignores_id_ts_and_op_id() {
        let a = task("same text");
        let mut b = a.clone();
        b.id = new_id();
        b.op_id = Some(new_op_id());
        b.ts = a.ts + chrono::Duration::seconds(30);
        assert_eq!(a.fingerprint(), b.fingerprint());
        b.body = Body::text("same text!");
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint().len(), 64);
    }

    #[test]
    fn child_causality_links_to_its_parent() {
        let env = task("x");
        let parent = env.causality.clone().expect("caus");
        let child = parent.child_of();
        assert_eq!(child.parent_task.as_deref(), Some(parent.task.as_str()));
        assert_eq!(child.hop, 1);
        assert_ne!(child.task, parent.task);
    }

    #[test]
    fn principal_display_and_accessors() {
        assert_eq!(Principal::role("a").to_string(), "a");
        assert_eq!(Principal::role_session("a", "s1").to_string(), "a#s1");
        assert_eq!(
            Principal::gateway("tg1", "telegram", Some("42".into())).to_string(),
            "gw:tg1:telegram:42"
        );
        assert_eq!(Principal::Cluster { cluster: "b".into() }.to_string(), "cluster:b");
        assert_eq!(Principal::role("a").role_name(), Some("a"));
        assert_eq!(Principal::role("a").kind(), "role");
    }

    #[test]
    fn protocol_version_mismatch_is_named() {
        let mut env = note("x");
        env.protocol = 0;
        let err = env.validate().unwrap_err();
        assert_eq!(err.field(), "protocol");
        assert_eq!(err.message(), "protocol 0 unsupported, expected 1");
    }
}
