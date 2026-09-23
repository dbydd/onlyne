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
use std::collections::BTreeMap;
use std::fmt;
use uuid::Uuid;

/// UTF-8 ceiling for [`Body::text`].
pub const BODY_TEXT_MAX_BYTES: usize = 1024 * 1024;

/// Decoded ceiling for [`ImagePart::data_base64`].
pub const IMAGE_DATA_MAX_BYTES: usize = 2 * 1024 * 1024;

/// The image types the core accepts, in stable order.
pub const IMAGE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Ceiling on the entries a family's [`Causality::labels`] may carry.
pub const CAUSALITY_LABEL_MAX_ENTRIES: usize = 8;

/// Ceiling on one label key's bytes.
pub const CAUSALITY_LABEL_KEY_MAX_BYTES: usize = 32;

/// Ceiling on one label value's bytes.
pub const CAUSALITY_LABEL_VALUE_MAX_BYTES: usize = 256;

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
    /// Bring the task's live session to the front of its host.
    Focus { task_id: String },
}

impl ControlOp {
    pub fn task_id(&self) -> &str {
        match self {
            ControlOp::Recycle { task_id, .. }
            | ControlOp::Probe { task_id }
            | ControlOp::Snapshot { task_id }
            | ControlOp::Cancel { task_id, .. }
            | ControlOp::Focus { task_id } => task_id,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ControlOp::Recycle { .. } => "recycle",
            ControlOp::Probe { .. } => "probe",
            ControlOp::Snapshot { .. } => "snapshot",
            ControlOp::Cancel { .. } => "cancel",
            ControlOp::Focus { .. } => "focus",
        }
    }

    pub fn name(&self) -> &'static str {
        self.as_str()
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
    /// The family's root task id. Every task handed on from one root carries the same
    /// family, which is what lets a supervisor read a whole run as one arc instead of
    /// walking `parent_task` links, and what `onlyne ledger` prints beside the hop. A
    /// root minted before this field existed names none, and the first child mints the
    /// family from its own parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// The hops this family may spend, set by whoever started it. A role reads it to
    /// learn whether it is the hop that keeps the task, which keeps that budget out of
    /// the task text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hop_budget: Option<u32>,
    /// The role this family reports home to, when the sender is not that role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Wall-clock bound for the whole family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DateTime<Utc>>,
    /// Free-form metadata the core carries and never interprets. Bounded by
    /// [`CAUSALITY_LABEL_MAX_ENTRIES`], [`CAUSALITY_LABEL_KEY_MAX_BYTES`], and
    /// [`CAUSALITY_LABEL_VALUE_MAX_BYTES`] at [`Envelope::validate`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<BTreeMap<String, String>>,
}

impl Causality {
    pub fn root(task: impl Into<String>) -> Self {
        let task = task.into();
        Causality {
            family: Some(task.clone()),
            task,
            parent_task: None,
            reply_to: None,
            hop: 0,
            attempt: 0,
            hop_budget: None,
            origin: None,
            deadline: None,
            labels: None,
        }
    }

    /// Derive the child task link for a downstream send.
    ///
    /// The family's metadata rides along: the child inherits the root id, the hop
    /// budget, the origin, the deadline, and the labels, so every hop of one run reads
    /// the same figures. A parent that names no family — a root minted before the field
    /// existed — hands its own task id down as the family its children carry.
    pub fn child_of(&self) -> Causality {
        Causality {
            task: new_task_id(),
            parent_task: Some(self.task.clone()),
            reply_to: None,
            hop: self.hop + 1,
            attempt: 0,
            family: Some(self.family.clone().unwrap_or_else(|| self.task.clone())),
            hop_budget: self.hop_budget,
            origin: self.origin.clone(),
            deadline: self.deadline,
            labels: self.labels.clone(),
        }
    }

    /// Validate the family metadata's bounds, naming the offending field.
    pub fn validate(&self) -> Result<()> {
        let Some(labels) = &self.labels else {
            return Ok(());
        };
        if labels.len() > CAUSALITY_LABEL_MAX_ENTRIES {
            return Err(Error::invalid(
                "causality.labels",
                format!(
                    "{} labels carried, at most {CAUSALITY_LABEL_MAX_ENTRIES}",
                    labels.len()
                ),
            ));
        }
        for (key, value) in labels {
            if key.is_empty() || key.len() > CAUSALITY_LABEL_KEY_MAX_BYTES {
                return Err(Error::invalid(
                    "causality.labels",
                    format!("label key {key:?} must be 1..={CAUSALITY_LABEL_KEY_MAX_BYTES} bytes"),
                ));
            }
            if value.len() > CAUSALITY_LABEL_VALUE_MAX_BYTES {
                return Err(Error::invalid(
                    "causality.labels",
                    format!(
                        "label {key:?} carries {} bytes, at most {CAUSALITY_LABEL_VALUE_MAX_BYTES}",
                        value.len()
                    ),
                ));
            }
        }
        Ok(())
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
                format!(
                    "protocol {} unsupported, expected {PROTOCOL_VERSION}",
                    self.protocol
                ),
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
            return Err(Error::invalid(
                "from.role",
                "role name must not be empty".to_string(),
            ));
        }
        if self.to.role_name() == Some("") {
            return Err(Error::invalid(
                "to.role",
                "role name must not be empty".to_string(),
            ));
        }
        self.body.validate()?;
        if let Some(causality) = &self.causality {
            causality.validate()?;
        }
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
                crate::text::causality_required(&self.kind.to_string()),
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
        return Err(Error::invalid(
            "op_id",
            "op_id must carry a uuid".to_string(),
        ));
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
    fn focus_control_op_round_trips_and_names_itself() {
        let task_id = new_task_id();
        let op = ControlOp::Focus {
            task_id: task_id.clone(),
        };
        assert_eq!(op.as_str(), "focus");
        assert_eq!(op.name(), "focus");
        assert_eq!(op.task_id(), task_id.as_str());
        let json = serde_json::to_value(&op).expect("encode");
        assert_eq!(json["op"], "focus");
        assert_eq!(json["task_id"], task_id);
        assert!(json.get("reason").is_none());
        let back: ControlOp = serde_json::from_value(json).expect("decode");
        assert_eq!(back, op);
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
        let err = env.validate().unwrap_err();
        assert_eq!(err.field(), "causality");
        assert_eq!(err.message(), crate::text::causality_required("task"));
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
        assert_eq!(
            Principal::Cluster {
                cluster: "b".into()
            }
            .to_string(),
            "cluster:b"
        );
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

    #[test]
    fn a_child_inherits_the_family_and_its_budget() {
        let mut root = Causality::root("root-task");
        root.hop_budget = Some(12);
        root.origin = Some("_supervisor".into());
        root.deadline = Some(Utc::now());
        root.labels = Some(std::collections::BTreeMap::from([(
            "run".to_string(),
            "ring".to_string(),
        )]));

        let child = root.child_of();
        assert_eq!(child.family.as_deref(), Some("root-task"));
        assert_eq!(child.parent_task.as_deref(), Some("root-task"));
        assert_eq!(child.hop, 1);
        assert_eq!(child.hop_budget, Some(12));
        assert_eq!(child.origin.as_deref(), Some("_supervisor"));
        assert_eq!(child.deadline, root.deadline);
        assert_eq!(child.labels, root.labels);

        let grandchild = child.child_of();
        assert_eq!(
            grandchild.family.as_deref(),
            Some("root-task"),
            "every hop names the same family"
        );
        assert_eq!(grandchild.hop, 2);
        assert_eq!(grandchild.hop_budget, Some(12));
    }

    #[test]
    fn a_parent_that_names_no_family_hands_its_own_task_down_as_one() {
        let legacy = Causality {
            task: "old-root".into(),
            parent_task: None,
            reply_to: None,
            hop: 0,
            attempt: 0,
            ..Default::default()
        };
        assert_eq!(
            legacy.child_of().family.as_deref(),
            Some("old-root"),
            "a chain minted before the field still lands on one family"
        );
    }

    #[test]
    fn a_causality_that_names_no_family_serializes_as_it_did() {
        let causality = Causality {
            task: "t".into(),
            parent_task: Some("p".into()),
            hop: 2,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&causality).expect("json"),
            r#"{"task":"t","parent_task":"p","hop":2,"attempt":0}"#,
            "an envelope written before the family metadata reads back byte for byte"
        );
    }

    #[test]
    fn label_bounds_are_enforced() {
        let mut causality = Causality::root("t");
        causality.labels = Some(
            (0..=CAUSALITY_LABEL_MAX_ENTRIES)
                .map(|i| (format!("k{i}"), "v".to_string()))
                .collect(),
        );
        let error = causality
            .validate()
            .expect_err("one label past the ceiling");
        assert!(
            error.to_string().contains("causality.labels"),
            "the refusal names the field: {error}"
        );

        causality.labels = Some(std::collections::BTreeMap::from([(
            "k".repeat(CAUSALITY_LABEL_KEY_MAX_BYTES + 1),
            "v".to_string(),
        )]));
        assert!(causality.validate().is_err(), "a long key is refused");

        causality.labels = Some(std::collections::BTreeMap::from([(
            "k".to_string(),
            "v".repeat(CAUSALITY_LABEL_VALUE_MAX_BYTES + 1),
        )]));
        assert!(causality.validate().is_err(), "a long value is refused");

        causality.labels = Some(std::collections::BTreeMap::from([(
            String::new(),
            "v".to_string(),
        )]));
        assert!(causality.validate().is_err(), "an empty key is refused");

        causality.labels = Some(std::collections::BTreeMap::from([(
            "k".to_string(),
            "v".to_string(),
        )]));
        assert!(causality.validate().is_ok(), "one short label passes");
    }
}
