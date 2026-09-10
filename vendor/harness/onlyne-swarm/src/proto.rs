use serde::{Deserialize, Serialize};
use std::fmt;

pub const SWARM_PROTOCOL: u32 = 2;
pub const MAX_ID_LEN: usize = 128;
pub const MAX_REASON_LEN: usize = 1024;
pub const MAX_COUNTER: u64 = 9_007_199_254_740_991;
/// Frame header for a worker→scheduler lifecycle report. A distinct prefix from
/// `---swarm`/`---swarm-ctl` so a report never reaches the task delivery paths.
pub const REPORT_PREFIX: &str = "---swarm-report\n";

/// Swarm envelope carried inside the message body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmHeader {
    pub task_id: String,
    pub from: String,
    #[serde(default)]
    pub transfer_send_to: String,
    #[serde(default = "default_attempt")]
    pub attempt: u32,
}

fn default_attempt() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmMessage {
    pub header: SwarmHeader,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseResult<T> {
    NotSwarm,
    ProtocolFault(ParseError),
    Parsed(T),
}

impl<T> ParseResult<T> {
    pub fn parsed(self) -> Option<T> {
        match self {
            Self::Parsed(v) => Some(v),
            _ => None,
        }
    }
    pub fn is_not_swarm(&self) -> bool {
        matches!(self, Self::NotSwarm)
    }
    pub fn is_fault(&self) -> bool {
        matches!(self, Self::ProtocolFault(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MissingDelimiter,
    MissingField(&'static str),
    DuplicateField(String),
    UnknownField(String),
    MalformedField(String),
    UnsupportedProtocol(u64),
    InvalidId(&'static str),
    InvalidAttempt,
    InvalidCounter(&'static str),
    InvalidReason,
    InvalidOperation,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ParseError {}

pub fn parse_result(text: &str) -> ParseResult<SwarmMessage> {
    let Some(rest) = text.strip_prefix("---swarm\n") else {
        return ParseResult::NotSwarm;
    };
    let Some((head_raw, payload)) = split_wire(rest) else {
        return ParseResult::ProtocolFault(ParseError::MissingDelimiter);
    };
    match parse_header(head_raw) {
        Ok(header) => ParseResult::Parsed(SwarmMessage {
            header,
            payload: payload.to_string(),
        }),
        Err(e) => ParseResult::ProtocolFault(e),
    }
}

/// Compatibility API. Callers that need diagnostics should use `parse_result`.
pub fn parse(text: &str) -> Option<SwarmMessage> {
    parse_result(text).parsed()
}

fn split_wire(rest: &str) -> Option<(&str, &str)> {
    let end = rest
        .find("\n---\n")
        .or_else(|| rest.strip_suffix("\n---").map(str::len))?;
    let payload = rest[end..].strip_prefix("\n---\n").unwrap_or("");
    Some((&rest[..end], payload))
}

fn parse_header(raw: &str) -> Result<SwarmHeader, ParseError> {
    let fields = fields(
        raw,
        &[
            "protocol",
            "task_id",
            "from",
            "transfer_send_to",
            "attempt",
            "delivery",
        ],
    )?;
    let protocol = number(
        fields
            .get("protocol")
            .ok_or(ParseError::MissingField("protocol"))?,
    )?;
    if protocol != SWARM_PROTOCOL as u64 {
        return Err(ParseError::UnsupportedProtocol(protocol));
    }
    let task_id = required_id(fields.get("task_id"), "task_id")?;
    let from = fields
        .get("from")
        .ok_or(ParseError::MissingField("from"))?
        .to_string();
    if from.trim().is_empty() {
        return Err(ParseError::MalformedField("from".into()));
    }
    let transfer = fields
        .get("transfer_send_to")
        .map(|v| v.to_string())
        .unwrap_or_default();
    if !transfer.is_empty() {
        validate_id(&transfer, "transfer_send_to")?;
    }
    let attempt = fields
        .get("attempt")
        .map(|v| v.parse().map_err(|_| ParseError::InvalidAttempt))
        .transpose()?
        .unwrap_or(1);
    if attempt == 0 {
        return Err(ParseError::InvalidAttempt);
    }
    if let Some(delivery) = fields.get("delivery") {
        if *delivery != "scheduler" {
            return Err(ParseError::MalformedField("delivery".into()));
        }
    }
    Ok(SwarmHeader {
        task_id,
        from,
        transfer_send_to: transfer,
        attempt,
    })
}

fn fields<'a>(
    raw: &'a str,
    allowed: &[&str],
) -> Result<std::collections::BTreeMap<String, &'a str>, ParseError> {
    let mut out = std::collections::BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(ParseError::MalformedField(line.into()));
        };
        let key = key.trim();
        if key.is_empty() || !allowed.contains(&key) {
            return Err(ParseError::UnknownField(key.to_string()));
        }
        if out.contains_key(key) {
            return Err(ParseError::DuplicateField(key.to_string()));
        }
        let value = value.trim();
        if value.is_empty() {
            return Err(ParseError::MalformedField(key.into()));
        }
        if value.starts_with('"') || value.starts_with('\'') {
            let q = value.as_bytes()[0] as char;
            if value.len() < 2 || !value.ends_with(q) {
                return Err(ParseError::MalformedField(key.into()));
            }
        }
        out.insert(key.to_string(), unquote(value));
    }
    Ok(out)
}
fn unquote(v: &str) -> &str {
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        &v[1..v.len() - 1]
    } else {
        v
    }
}
fn number(v: &str) -> Result<u64, ParseError> {
    v.parse()
        .map_err(|_| ParseError::MalformedField("protocol".into()))
}
fn required_id(v: Option<&&str>, name: &'static str) -> Result<String, ParseError> {
    let s = v.ok_or(ParseError::MissingField(name))?.trim().to_string();
    validate_id(&s, name)?;
    Ok(s)
}
fn validate_id(s: &str, name: &'static str) -> Result<(), ParseError> {
    if s.is_empty() || s.len() > MAX_ID_LEN || uuid::Uuid::parse_str(s).is_err() {
        return Err(ParseError::InvalidId(name));
    }
    Ok(())
}
fn validate_counter(v: u64, name: &'static str) -> Result<(), ParseError> {
    if v > MAX_COUNTER {
        Err(ParseError::InvalidCounter(name))
    } else {
        Ok(())
    }
}

pub fn render(header: &SwarmHeader, role: &str, payload_markdown: &str) -> String {
    let mut s = format!(
        "---swarm\nprotocol: {SWARM_PROTOCOL}\ntask_id: {}\nfrom: {}\n",
        header.task_id, header.from
    );
    if !header.transfer_send_to.is_empty() {
        s.push_str(&format!("transfer_send_to: {}\n", header.transfer_send_to));
    }
    s.push_str(&format!(
        "attempt: {}\ndelivery: scheduler\n---\n",
        header.attempt
    ));
    if !role.is_empty() {
        s.push_str(role);
        if !role.ends_with('\n') {
            s.push('\n');
        }
        s.push('\n');
    }
    s.push_str(payload_markdown);
    if !payload_markdown.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlOp {
    Probe,
    Recycle,
    Snapshot,
}
impl ControlOp {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Recycle => "recycle",
            Self::Snapshot => "snapshot",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlMessage {
    pub op: ControlOp,
    pub task_id: String,
    pub reason: String,
    pub op_id: String,
}

pub fn render_ctl(task_id: &str, reason: &str) -> String {
    let op_id = if task_id.is_empty() {
        "legacy-recycle"
    } else {
        task_id
    };
    render_ctl_with_op(ControlOp::Recycle, task_id, reason, op_id)
}
pub fn render_ctl_with_op(op: ControlOp, task_id: &str, reason: &str, op_id: &str) -> String {
    format!("---swarm-ctl\nprotocol: {SWARM_PROTOCOL}\nop: {}\ntask_id: {}\nreason: {}\nop_id: {}\n---\n", op.as_str(), task_id, reason, op_id)
}
pub fn parse_ctl(text: &str) -> ParseResult<ControlMessage> {
    let Some(rest) = text.strip_prefix("---swarm-ctl\n") else {
        return ParseResult::NotSwarm;
    };
    let Some((raw, _)) = split_wire(rest) else {
        return ParseResult::ProtocolFault(ParseError::MissingDelimiter);
    };
    let result = (|| {
        let f = fields(raw, &["protocol", "op", "task_id", "reason", "op_id"])?;
        let p = number(
            f.get("protocol")
                .ok_or(ParseError::MissingField("protocol"))?,
        )?;
        if p != 2 {
            return Err(ParseError::UnsupportedProtocol(p));
        }
        let op = match *f.get("op").ok_or(ParseError::MissingField("op"))? {
            "probe" => ControlOp::Probe,
            "recycle" => ControlOp::Recycle,
            "snapshot" => ControlOp::Snapshot,
            _ => return Err(ParseError::InvalidOperation),
        };
        let task_id = *f
            .get("task_id")
            .ok_or(ParseError::MissingField("task_id"))?;
        if task_id != "*" {
            validate_id(task_id, "task_id")?;
        }
        let reason = *f.get("reason").ok_or(ParseError::MissingField("reason"))?;
        if reason.is_empty() || reason.len() > MAX_REASON_LEN {
            return Err(ParseError::InvalidReason);
        }
        let op_id = *f.get("op_id").ok_or(ParseError::MissingField("op_id"))?;
        if op_id.is_empty() || op_id.len() > MAX_ID_LEN {
            return Err(ParseError::InvalidId("op_id"));
        }
        Ok(ControlMessage {
            op,
            task_id: task_id.into(),
            reason: reason.into(),
            op_id: op_id.into(),
        })
    })();
    match result {
        Ok(v) => ParseResult::Parsed(v),
        Err(e) => ParseResult::ProtocolFault(e),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleKind {
    Heartbeat,
    Ready,
    TurnStarted,
    Snapshot,
    Complete,
    Fault,
}
impl LifecycleKind {
    /// Wire token for this report kind.
    pub fn as_str(self) -> &'static str {
        match self {
            LifecycleKind::Heartbeat => "heartbeat",
            LifecycleKind::Ready => "ready",
            LifecycleKind::TurnStarted => "turn_started",
            LifecycleKind::Snapshot => "snapshot",
            LifecycleKind::Complete => "complete",
            LifecycleKind::Fault => "fault",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleReport {
    pub kind: LifecycleKind,
    pub task_id: String,
    pub generation: u64,
    pub seq: u64,
}
pub fn render_report(kind: LifecycleKind, task_id: &str, generation: u64, seq: u64) -> String {
    format!("{REPORT_PREFIX}protocol: {SWARM_PROTOCOL}\nop: {}\ntask_id: {task_id}\ngeneration: {generation}\nseq: {seq}\n---\n", kind.as_str())
}
pub fn parse_report(text: &str) -> ParseResult<LifecycleReport> {
    let Some(rest) = text.strip_prefix(REPORT_PREFIX) else {
        return ParseResult::NotSwarm;
    };
    let Some((raw, _)) = split_wire(rest) else {
        return ParseResult::ProtocolFault(ParseError::MissingDelimiter);
    };
    let r = (|| {
        let f = fields(raw, &["protocol", "op", "task_id", "generation", "seq"])?;
        let p = number(
            f.get("protocol")
                .ok_or(ParseError::MissingField("protocol"))?,
        )?;
        if p != 2 {
            return Err(ParseError::UnsupportedProtocol(p));
        };
        let kind = match *f.get("op").ok_or(ParseError::MissingField("op"))? {
            "heartbeat" => LifecycleKind::Heartbeat,
            "ready" => LifecycleKind::Ready,
            "turn_started" => LifecycleKind::TurnStarted,
            "snapshot" => LifecycleKind::Snapshot,
            "complete" => LifecycleKind::Complete,
            "fault" => LifecycleKind::Fault,
            _ => return Err(ParseError::InvalidOperation),
        };
        let task_id = required_id(f.get("task_id"), "task_id")?;
        let generation = number(
            f.get("generation")
                .ok_or(ParseError::MissingField("generation"))?,
        )?;
        let seq = number(f.get("seq").ok_or(ParseError::MissingField("seq"))?)?;
        validate_counter(generation, "generation")?;
        validate_counter(seq, "seq")?;
        Ok(LifecycleReport {
            kind,
            task_id,
            generation,
            seq,
        })
    })();
    match r {
        Ok(v) => ParseResult::Parsed(v),
        Err(e) => ParseResult::ProtocolFault(e),
    }
}

pub fn failed_reason(reason: &str) -> String {
    format!("swarm-failed: {reason}")
}
pub fn cancelled_reason(reason: &str) -> String {
    format!("swarm-cancelled: {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
    #[test]
    fn roundtrip() {
        let h = SwarmHeader {
            task_id: id(),
            from: "planner".into(),
            transfer_send_to: String::new(),
            attempt: 1,
        };
        let m = parse_result(&render(&h, "planner role", "do X"));
        assert!(matches!(m, ParseResult::Parsed(_)));
        assert!(render(&h, "", "x").contains("protocol: 2\n"));
    }
    #[test]
    fn ordinary_and_prefixed_faults_are_distinct() {
        assert!(matches!(parse_result("hello"), ParseResult::NotSwarm));
        assert!(matches!(
            parse_result("---swarm\nprotocol: 1\n---\n"),
            ParseResult::ProtocolFault(_)
        ));
        assert!(matches!(parse_ctl("hello"), ParseResult::NotSwarm));
    }
    #[test]
    fn malformed_headers_table() {
        let i = id();
        let cases = [
            format!("---swarm\ntask_id: {i}\nfrom: a\n---\n"),
            format!("---swarm\nprotocol: 2\ntask_id: {i}\nfrom: a\ntask_id: {i}\n---\n"),
            format!("---swarm\nprotocol: 2\ntask_id: {i}\nfrom: a\nwat: x\n---\n"),
            format!("---swarm\nprotocol: two\ntask_id: {i}\nfrom: a\n---\n"),
        ];
        for wire in cases {
            assert!(matches!(parse_result(&wire), ParseResult::ProtocolFault(_)));
        }
    }
    #[test]
    fn controls_table() {
        for op in [ControlOp::Probe, ControlOp::Recycle, ControlOp::Snapshot] {
            let w = render_ctl_with_op(op, "*", "reason", "op-1");
            assert!(matches!(parse_ctl(&w), ParseResult::Parsed(_)));
        }
        assert!(parse_ctl(&format!(
            "---swarm-ctl\nprotocol: 2\nop: nope\ntask_id: *\nreason: x\nop_id: x\n---\n"
        ))
        .is_fault());
    }
    #[test]
    fn reports_and_bounds() {
        let t = id();
        let w = render_report(LifecycleKind::Ready, &t, 3, 4);
        assert!(matches!(parse_report(&w), ParseResult::Parsed(_)));
        let bad = w.replace("seq: 4", "seq: 9007199254740992");
        assert!(parse_report(&bad).is_fault());
    }
    #[test]
    fn legacy_helpers() {
        let w = render_ctl("some-task", "cancel");
        assert!(parse(&w).is_none());
        assert_eq!(failed_reason("x"), "swarm-failed: x");
    }
}
