use crate::{
    SpecError,
    diff::SpecDiff,
    hash::{canonical_bytes, spec_hash},
    locate::{array_entry_lines, key_line_in_array_entry, key_line_in_table, line_from_span},
};
use base64::{Engine, engine::general_purpose};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::Path};

pub const DEFAULT_NOTE_QUEUE: bool = false;
pub const DEFAULT_FAULT_HISTORY_DAYS: u32 = 14;
pub const DEFAULT_RESYNC_LAG: u32 = 256;
pub const DEFAULT_HEARTBEAT_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_TEMPLATE_ROOT: &str = ".onlyne/templates";
pub const DEFAULT_AGENT_PACKAGE: &str = "";
pub const DEFAULT_MAX_SESSIONS: u32 = 1;
pub const DEFAULT_KEY_PREFIX: &str = "ed25519/";
pub const DEFAULT_CERT_PIN_PREFIX: &str = "sha256/";
pub const DEFAULT_INTENT_ATTEMPTS: u32 = 3;
pub const DEFAULT_BACKOFF_MS: [u64; 3] = [1_000, 2_000, 4_000];
pub const KEY_BYTE_LEN: usize = 32;
pub const ALLOWED_PLACEHOLDERS: [&str; 2] = ["session", "task"];

/// Cluster spec loaded from `<server-root>/.onlyne/spec.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub server: ServerSection,
    #[serde(default)]
    pub client: Vec<ClientEntry>,
    #[serde(default)]
    pub gateway: Vec<GatewayEntry>,
    #[serde(default)]
    pub route: Vec<RouteEntry>,
}

/// `[server]` section for the cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    pub name: String,
    pub listen: String,
    pub cert_pin: String,
    #[serde(default = "default_note_queue")]
    pub note_queue: bool,
    #[serde(default = "default_fault_history_days")]
    pub fault_history_days: u32,
    #[serde(default = "default_resync_lag")]
    pub resync_lag: u32,
    #[serde(default = "default_heartbeat_timeout_ms")]
    pub heartbeat_timeout_ms: u64,
    #[serde(default)]
    pub agent_package: String,
    #[serde(default = "default_template_root")]
    pub template_root: String,
}

/// `[[client]]` entry. Aggregate roles use the same struct and carry an
/// annotation in `aggregate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClientEntry {
    pub role: String,
    pub key: String,
    #[serde(default)]
    pub prose: String,
    #[serde(default)]
    pub admin: bool,
    #[serde(default = "default_max_sessions")]
    pub max_sessions: u32,
    #[serde(default)]
    pub reuse: bool,
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    #[serde(default)]
    pub allowed_targets: Vec<String>,
    #[serde(default)]
    pub session_command: Vec<String>,
    #[serde(default)]
    pub timeout: Timeouts,
    #[serde(default)]
    pub intent: IntentPolicy,
    #[serde(default)]
    pub aggregate: String,
}

/// `[[gateway]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GatewayEntry {
    pub id: String,
    pub platform: String,
    pub key: String,
    #[serde(default)]
    pub enabled: bool,
}

/// External inbound message route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteEntry {
    pub gateway: String,
    pub channel: String,
    #[serde(default)]
    pub conversation: Option<String>,
    pub to: RouteTarget,
}

/// Destination role and optional fixed session for a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteTarget {
    pub role: String,
    #[serde(default)]
    pub session: Option<String>,
}

/// Client timeout policy in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Timeouts {
    #[serde(default = "default_ready_ms")]
    pub ready_ms: u64,
    #[serde(default = "default_running_ms")]
    pub running_ms: u64,
    #[serde(default = "default_idle_ms")]
    pub idle_ms: u64,
}

/// Intent retry policy for coding-agent responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntentPolicy {
    #[serde(default = "default_intent_attempts")]
    pub attempts: u32,
    #[serde(default = "default_backoff_ms")]
    pub backoff_ms: Vec<u64>,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            name: String::new(),
            listen: String::new(),
            cert_pin: String::new(),
            note_queue: default_note_queue(),
            fault_history_days: default_fault_history_days(),
            resync_lag: default_resync_lag(),
            heartbeat_timeout_ms: default_heartbeat_timeout_ms(),
            agent_package: String::new(),
            template_root: default_template_root(),
        }
    }
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            ready_ms: default_ready_ms(),
            running_ms: default_running_ms(),
            idle_ms: default_idle_ms(),
        }
    }
}

impl Default for IntentPolicy {
    fn default() -> Self {
        Self {
            attempts: default_intent_attempts(),
            backoff_ms: default_backoff_ms(),
        }
    }
}

impl Spec {
    /// Load and validate a spec from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SpecError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| SpecError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse_named(&text, display_name(path))
    }

    /// Load and validate a replacement spec, then diff it against this spec.
    pub fn load_validate(&self, path: impl AsRef<Path>) -> Result<SpecDiff, SpecError> {
        let next = Self::load(path)?;
        Ok(SpecDiff::between(self, &next))
    }

    /// Parse a `spec.toml` string using `spec.toml` in diagnostics.
    pub fn parse_str(text: &str) -> Result<Self, SpecError> {
        Self::parse_named(text, "spec.toml")
    }

    /// Parse a `spec.toml` string and set the display name used in errors.
    pub fn parse_named(text: &str, file: &str) -> Result<Self, SpecError> {
        let parsed: toml::Value = text.parse::<toml::Value>().map_err(|err| {
            SpecError::parse(
                file,
                line_from_span(text, err.span()),
                err.message().to_string(),
            )
        })?;
        let spec: Spec = parsed.clone().try_into().map_err(|err: toml::de::Error| {
            SpecError::parse(
                file,
                serde_error_line(text, err.span(), err.message()),
                err.message().to_string(),
            )
        })?;
        validate(&spec, &parsed, text, file)?;
        Ok(spec)
    }

    /// Stable semantic hash for a parsed spec.
    pub fn semantic_hash(&self) -> String {
        let value = toml::Value::try_from(self).expect("spec serializes to TOML value");
        spec_hash(&canonical_bytes(&value))
    }
}

fn validate(spec: &Spec, value: &toml::Value, text: &str, file: &str) -> Result<(), SpecError> {
    validate_cert_pin(&spec.server.cert_pin).map_err(|message| {
        SpecError::validate(file, key_line_in_table(text, "server", "cert_pin"), message)
    })?;
    validate_duplicate_roles(spec, text, file)?;
    validate_duplicate_gateways(spec, text, file)?;

    for (idx, client) in spec.client.iter().enumerate() {
        validate_ed25519_key(&client.key).map_err(|message| {
            SpecError::validate(
                file,
                key_line_in_array_entry(text, "client", idx, "key"),
                format!("role {} has invalid key: {message}", client.role),
            )
        })?;
        validate_session_command(&client.role, &client.session_command, text, file, idx)?;
    }

    for (idx, gateway) in spec.gateway.iter().enumerate() {
        validate_ed25519_key(&gateway.key).map_err(|message| {
            SpecError::validate(
                file,
                key_line_in_array_entry(text, "gateway", idx, "key"),
                format!("gateway {} has invalid key: {message}", gateway.id),
            )
        })?;
    }

    if !value.is_table() {
        return Err(SpecError::validate(
            file,
            1,
            "spec root must be a TOML table",
        ));
    }
    Ok(())
}

fn serde_error_line(text: &str, span: Option<std::ops::Range<usize>>, message: &str) -> usize {
    let line = line_from_span(text, span.clone());
    if span.is_some() && line > 1 {
        return line;
    }
    if let Some(field) = unknown_field(message) {
        return locate_field_line(text, field);
    }
    if let Some(literal) = invalid_type_literal(message) {
        return locate_value_line(text, literal);
    }
    if let Some(field) = missing_field(message) {
        return locate_table_line(text, field);
    }
    if let Some(field) = expected_field(message) {
        return locate_field_line(text, field);
    }
    line
}

fn missing_field(message: &str) -> Option<&str> {
    message
        .strip_prefix("missing field `")
        .and_then(|rest| rest.split_once('`').map(|(field, _)| field))
}

fn locate_table_line(text: &str, field: &str) -> usize {
    const SERVER_FIELDS: &[&str] = &[
        "server",
        "name",
        "listen",
        "cert_pin",
        "note_queue",
        "fault_history_days",
        "resync_lag",
        "heartbeat_timeout_ms",
        "agent_package",
        "template_root",
    ];
    const CLIENT_FIELDS: &[&str] = &[
        "client",
        "role",
        "prose",
        "admin",
        "max_sessions",
        "reuse",
        "allowed_senders",
        "allowed_targets",
        "session_command",
        "timeout",
        "intent",
        "aggregate",
        "ready_ms",
        "running_ms",
        "idle_ms",
        "attempts",
        "backoff_ms",
    ];
    const GATEWAY_FIELDS: &[&str] = &["gateway", "id", "platform", "enabled"];
    const ROUTE_FIELDS: &[&str] = &["route", "channel", "conversation", "to", "session"];
    if field == "key" {
        let mut starts = array_entry_lines(text, "client");
        starts.extend(array_entry_lines(text, "gateway"));
        return starts.into_iter().min().unwrap_or(1);
    }
    if SERVER_FIELDS.contains(&field) {
        return text
            .lines()
            .enumerate()
            .find_map(|(idx, line)| (line.trim() == "[server]").then_some(idx + 1))
            .unwrap_or(1);
    }
    if CLIENT_FIELDS.contains(&field) {
        return array_entry_lines(text, "client")
            .into_iter()
            .min()
            .unwrap_or(1);
    }
    if GATEWAY_FIELDS.contains(&field) {
        return array_entry_lines(text, "gateway")
            .into_iter()
            .min()
            .unwrap_or(1);
    }
    if ROUTE_FIELDS.contains(&field) {
        return array_entry_lines(text, "route")
            .into_iter()
            .min()
            .unwrap_or(1);
    }
    locate_field_line(text, field)
}

fn unknown_field(message: &str) -> Option<&str> {
    message
        .strip_prefix("unknown field `")
        .and_then(|rest| rest.split_once('`').map(|(field, _)| field))
}

fn expected_field(message: &str) -> Option<&str> {
    message
        .rsplit_once("expected a ")
        .and_then(|(prefix, _)| prefix.rsplit_once('`'))
        .and_then(|(prefix, _)| prefix.rsplit_once('`'))
        .map(|(_, field)| field)
}

fn invalid_type_literal(message: &str) -> Option<&str> {
    message
        .strip_prefix("invalid type: ")
        .and_then(|rest| rest.split_once('`'))
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(literal, _)| literal)
}

fn locate_value_line(text: &str, literal: &str) -> usize {
    for (idx, line) in text.lines().enumerate() {
        if line.contains(literal) {
            return idx + 1;
        }
    }
    1
}

fn locate_field_line(text: &str, field: &str) -> usize {
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with(&format!("{field} =")) || trimmed.starts_with(&format!("{field}=")) {
            return idx + 1;
        }
    }
    1
}

fn validate_duplicate_roles(spec: &Spec, text: &str, file: &str) -> Result<(), SpecError> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (idx, client) in spec.client.iter().enumerate() {
        if let Some(first_idx) = seen.insert(client.role.as_str(), idx) {
            let first_line = key_line_in_array_entry(text, "client", first_idx, "role");
            let second_line = key_line_in_array_entry(text, "client", idx, "role");
            return Err(SpecError::validate(
                file,
                second_line,
                format!(
                    "duplicate client role `{}` at lines {} and {}",
                    client.role, first_line, second_line
                ),
            ));
        }
    }
    Ok(())
}

fn validate_duplicate_gateways(spec: &Spec, text: &str, file: &str) -> Result<(), SpecError> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (idx, gateway) in spec.gateway.iter().enumerate() {
        if let Some(first_idx) = seen.insert(gateway.id.as_str(), idx) {
            let first_line = key_line_in_array_entry(text, "gateway", first_idx, "id");
            let second_line = key_line_in_array_entry(text, "gateway", idx, "id");
            return Err(SpecError::validate(
                file,
                second_line,
                format!(
                    "duplicate gateway id `{}` at lines {} and {}",
                    gateway.id, first_line, second_line
                ),
            ));
        }
    }
    Ok(())
}

fn validate_session_command(
    role: &str,
    tokens: &[String],
    text: &str,
    file: &str,
    entry_index: usize,
) -> Result<(), SpecError> {
    for token in tokens {
        for placeholder in placeholders(token) {
            if !ALLOWED_PLACEHOLDERS.contains(&placeholder.as_str()) {
                return Err(SpecError::validate(
                    file,
                    key_line_in_array_entry(text, "client", entry_index, "session_command"),
                    format!(
                        "role {role} has unknown session_command placeholder {{{placeholder}}}"
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn placeholders(token: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = token;
    while let Some(start) = rest.find('{') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            break;
        };
        found.push(after_start[..end].to_string());
        rest = &after_start[end + 1..];
    }
    found
}

fn validate_ed25519_key(key: &str) -> Result<(), String> {
    let Some(encoded) = key.strip_prefix(DEFAULT_KEY_PREFIX) else {
        return Err("expected ed25519/<base64>".to_string());
    };
    let decoded = general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "base64 decode failed".to_string())?;
    if decoded.len() == KEY_BYTE_LEN {
        Ok(())
    } else {
        Err(format!(
            "decoded key must be 32 bytes, got {}",
            decoded.len()
        ))
    }
}

fn validate_cert_pin(pin: &str) -> Result<(), String> {
    let Some(encoded) = pin.strip_prefix(DEFAULT_CERT_PIN_PREFIX) else {
        return Err("cert_pin must start with sha256/".to_string());
    };
    if encoded.is_empty() {
        return Err("cert_pin digest is empty".to_string());
    }
    if is_hex_digest(encoded) {
        return Ok(());
    }
    let decoded = general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "cert_pin digest must be base64 or lowercase hex".to_string())?;
    if decoded.len() == Sha256::output_size() {
        Ok(())
    } else {
        Err(format!(
            "cert_pin digest must decode to 32 bytes, got {}",
            decoded.len()
        ))
    }
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == Sha256::output_size() * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn default_note_queue() -> bool {
    DEFAULT_NOTE_QUEUE
}

pub(crate) fn default_fault_history_days() -> u32 {
    DEFAULT_FAULT_HISTORY_DAYS
}

pub(crate) fn default_resync_lag() -> u32 {
    DEFAULT_RESYNC_LAG
}

pub(crate) fn default_heartbeat_timeout_ms() -> u64 {
    DEFAULT_HEARTBEAT_TIMEOUT_MS
}

pub(crate) fn default_template_root() -> String {
    DEFAULT_TEMPLATE_ROOT.to_string()
}

pub(crate) fn default_max_sessions() -> u32 {
    DEFAULT_MAX_SESSIONS
}

fn default_ready_ms() -> u64 {
    30_000
}

fn default_running_ms() -> u64 {
    120_000
}

fn default_idle_ms() -> u64 {
    60_000
}

fn default_intent_attempts() -> u32 {
    DEFAULT_INTENT_ATTEMPTS
}

fn default_backoff_ms() -> Vec<u64> {
    DEFAULT_BACKOFF_MS.to_vec()
}

fn display_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("spec.toml")
}
