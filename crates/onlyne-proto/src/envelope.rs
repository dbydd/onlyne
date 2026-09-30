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

/// Base64 ceiling for [`ImagePart::data_base64`]: the longest encoded text that can
/// still decode inside [`IMAGE_DATA_MAX_BYTES`].
///
/// Standard base64 expands each run of three bytes into four, so a text longer than
/// the exact expansion of the budget cannot describe an in-budget image at all. The
/// expansion rounds up (`ceil(2 MiB / 3) * 4 = 2796204` for the budget itself) and the
/// padding characters sit on top of it, so the ceiling carries slack above the exact
/// expansion. [`ImagePart::decode`] refuses an overlong text before decoding it, which
/// is what keeps a rejected attachment from allocating a decoded buffer the size of
/// the sender's text; the verdict it reports is the same one the decoded-size check
/// would reach, so the wire sees the spec's wording either way.
pub const IMAGE_DATA_MAX_ENCODED_BYTES: usize = IMAGE_DATA_MAX_BYTES * 4 / 3 + 1024;

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
    /// The delivery ended waiting on something outside it.
    ///
    /// A first-class result rather than a synonym for `Failed`: the ending rule
    /// settles a delivery whose session ended without a completion as blocked,
    /// and a board reads it as waiting rather than as failed.
    Blocked,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Failed => "failed",
            Outcome::Cancelled => "cancelled",
            Outcome::Blocked => "blocked",
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
    ///
    /// The encoded text is measured first: past [`IMAGE_DATA_MAX_ENCODED_BYTES`] it
    /// cannot decode to an in-budget image, so the call fails with
    /// the same error the decoded-size check reports, instead of materialising a decoded
    /// buffer the size of the sender's text. A text inside that bound decodes in full,
    /// and the exact decoded ceiling stays [`Body::validate`]'s verdict, so an image one
    /// byte over the budget reads exactly as it did before the gate existed.
    pub fn decode(&self) -> Result<Vec<u8>> {
        if self.data_base64.len() > IMAGE_DATA_MAX_ENCODED_BYTES {
            return Err(image_over_budget());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.data_base64)
            .map_err(|e| Error::invalid("body.image.data_base64", format!("bad base64: {e}")))?;
        Ok(bytes)
    }
}

/// The plan's §3 wording for an image over [`IMAGE_DATA_MAX_BYTES`], reported on the
/// field `body.image.data_base64`. Both the pre-decode length gate in
/// [`ImagePart::decode`] and the decoded-size check in [`Body::validate`] answer with
/// it, so the message a client matches on cannot drift between the two.
fn image_over_budget() -> Error {
    Error::invalid(
        "body.image.data_base64",
        format!("image exceeds {IMAGE_DATA_MAX_BYTES} bytes"),
    )
}

/// Envelope payload: text plus at most one inline image.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default)]
pub struct Body {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// One display line the sender names for this body, which a reader shows in
    /// place of a preview of the body's own text. `None` is the ordinary case:
    /// the body's text is the only content, and a store that keeps a one-line
    /// preview derives it from the text. A completion that carries its full
    /// result in `text` and a one-line summary separately sets this, so the
    /// summary is what a ledger shows rather than the first clusters of the
    /// result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImagePart>,
}

impl Body {
    pub fn text(text: impl Into<String>) -> Self {
        Body {
            text: Some(text.into()),
            head: None,
            image: None,
        }
    }

    pub fn image(data_base64: impl Into<String>, mime: impl Into<String>) -> Self {
        Body {
            text: None,
            head: None,
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
                return Err(image_over_budget());
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
    ///
    /// The hop adds saturating: it counts up to `u32::MAX` and never wraps. Depth is
    /// how a role learns whether it is the hop that keeps the work, so a child that
    /// came back around to `hop = 0` would read as the root of a fresh family — under
    /// its parent's budget, with nothing spent — and a ring of that shape never stops.
    /// Sitting past the deepest hop any budget names is the reading that ends the
    /// chain, which is why the saturated counter stays an ordinary `Causality` rather
    /// than becoming an error this infallible constructor could only report by taking
    /// every caller's signature with it.
    pub fn child_of(&self) -> Causality {
        Causality {
            task: new_task_id(),
            parent_task: Some(self.task.clone()),
            reply_to: None,
            hop: self.hop.saturating_add(1),
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
    ///
    /// `None` is legal only for [`MsgKind::Note`], and [`Envelope::validate`] enforces
    /// that: a Task, Completion, or Control without a key is refused on the field
    /// `op_id`, so the retry the sender eventually makes dedups on one id.
    ///
    /// A note that needs a durable row — the client queues every outbound envelope
    /// keyed by its `op_id` — is stamped once by the code that writes the row, and the
    /// stamped envelope is what gets stored and replayed (see `stamp_op_id` in
    /// `onlyne-client`). So the absent key here never means "mint one per attempt": a
    /// caller that generated a fresh id on every read would key each retry as a new
    /// send, and idempotence would silently stop existing. Keep `op_id` as received,
    /// and stamp before the first write.
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
    /// retry stays idempotent. The counter adds saturating, so a redelivery count read
    /// off a long-lived row keeps meaning "delivered at least this many times" instead
    /// of wrapping back to "never delivered".
    pub fn redelivered(&self) -> Envelope {
        let mut next = self.clone();
        if let Some(causality) = &mut next.causality {
            causality.attempt = causality.attempt.saturating_add(1);
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
