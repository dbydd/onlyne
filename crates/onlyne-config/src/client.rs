use crate::{
    SpecError,
    env::Env,
    locate::{find_key_line_in_table, line_from_span},
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::path::Path;
use std::time::Duration;

/// Client-side `<workspace>/.onlyne/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientConfig {
    /// Role name used for this workspace.
    pub role: String,
    /// Server endpoint.
    pub server: ServerEndpoint,
    /// Server certificate SPKI fingerprint.
    pub cert_pin: String,
    /// Path to this role's private key file.
    pub key_path: String,
    /// Local plugin list.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Orca session backend settings.
    #[serde(default)]
    pub orca: OrcaSection,
    /// ACP session backend settings.
    #[serde(default)]
    pub acp: AcpSection,
    /// Seconds a running session may sit without an Applied tuple change before
    /// this client reports it stalled. Zero disables the report.
    #[serde(default = "default_stall_report_secs")]
    pub stall_report_secs: u64,
    /// Seconds a dropped plugin connection may stay away before this client
    /// retires the session it left behind. A session with a task bound goes with
    /// it: an unsettled task ends `failed`, the delivery that session still held
    /// is refused with reason `session_dead`, and the session's own exit is
    /// published. An agent that reconnects inside the window keeps its session; a
    /// connection that returns after a newer session took the task is held
    /// read-only, and what it sends rides that session's closing handoff. 0
    /// disables the sweep.
    #[serde(default = "default_reconnect_grace_secs")]
    pub reconnect_grace_secs: u64,
    /// Requested session backend (`herdr` | `orca` | `zellij` | `exec` /
    /// `headless` | `acp` | `fake` | `auto`). Empty means auto-detect. The
    /// process environment `ONLYNE_BACKEND` takes precedence when it is
    /// nonempty.
    #[serde(default)]
    pub backend: String,
    /// `[client]` — the workspace-local client policy.
    #[serde(default)]
    pub client: ClientSection,
}

/// `[client]` — the workspace-local client policy, beside `plugins` and
/// `[server]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientSection {
    /// `[client.session]` — what a session serves and when it closes.
    #[serde(default)]
    pub session: SessionPolicy,
}

/// `[client.session]` — the per-role session policy. `scope` decides which
/// deliveries one session serves; `idle_close` decides when an idle session
/// ends. An absent key keeps the scope's own default, so a workspace written
/// before v2 stays valid and lands on `oneshot`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionPolicy {
    /// Which deliveries a session serves. The default, `oneshot`, is v1's
    /// behavior: one delivery per session, closed when that delivery settles.
    #[serde(default)]
    pub scope: SessionScope,
    /// How long a session may sit idle before this client closes it. `None`
    /// means the scope's own default. `Some(ZERO)` means no idle close at all,
    /// the same "0 disables" reading `stall_report_secs` and
    /// `reconnect_grace_secs` take. Spelled as a duration (`30s`, `5m`, `2h`,
    /// `1d`) or a bare integer of seconds.
    #[serde(default, deserialize_with = "de_idle_close")]
    // The published schema answers what a `config.toml` may write, and a
    // `Duration`'s own shape is a `{ secs, nanos }` struct TOML never sees.
    #[schemars(with = "Option<String>")]
    pub idle_close: Option<Duration>,
}

/// The `scope` of a `[client.session]` table: which deliveries one session
/// serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum SessionScope {
    /// One delivery, closed when that delivery settles.
    #[default]
    Oneshot,
    /// Every delivery one task family sends this role, closed on the idle
    /// timeout or an operator close.
    Task,
    /// A standing session pool for the role, at most the role's
    /// `max_sessions` active, closed on an operator close or a recycle.
    Role,
}

impl SessionScope {
    /// The one spelling of each scope, as `config.toml` writes it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Oneshot => "oneshot",
            Self::Task => "task",
            Self::Role => "role",
        }
    }

    /// Parse a written scope, `None` for anything else.
    ///
    /// An unknown scope is a refusal and never a fall back to `oneshot`: a
    /// typo read as the default would silently change which conversation a
    /// delivery lands in.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "oneshot" => Some(Self::Oneshot),
            "task" => Some(Self::Task),
            "role" => Some(Self::Role),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for SessionScope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw)
            .ok_or_else(|| de::Error::custom(format!("unknown client.session scope `{raw}`")))
    }
}

impl std::fmt::Display for SessionScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `idle_close` written as a duration string or a bare integer of seconds.
fn de_idle_close<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Duration>, D::Error> {
    let raw = toml::Value::deserialize(deserializer)?;
    idle_close_value(&raw).ok_or_else(|| {
        de::Error::custom(format!(
            "client.session.idle_close must be a duration (`30s`, `5m`, `2h`, `1d`, or \
             bare seconds), got {}",
            toml_literal(&raw)
        ))
    })
}

/// The duration one written `idle_close` carries, `None` when the value is not
/// one.
fn idle_close_value(raw: &toml::Value) -> Option<Option<Duration>> {
    match raw {
        toml::Value::String(text) => parse_idle_close(text).map(Some),
        toml::Value::Integer(secs) => u64::try_from(*secs).ok().map(Duration::from_secs).map(Some),
        _ => None,
    }
}

/// A written `idle_close`: `30s`, `5m`, `2h`, `1d`, or a bare integer of
/// seconds. `0` is a real value, not an absent one, and means no idle close.
fn parse_idle_close(raw: &str) -> Option<Duration> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let (digits, scale) = match text.as_bytes().last() {
        Some(b's') => (&text[..text.len() - 1], 1),
        Some(b'm') => (&text[..text.len() - 1], 60),
        Some(b'h') => (&text[..text.len() - 1], 3_600),
        Some(b'd') => (&text[..text.len() - 1], 86_400),
        _ => (text, 1),
    };
    let digits = digits.trim();
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits
        .parse::<u64>()
        .ok()?
        .checked_mul(scale)
        .map(Duration::from_secs)
}

/// A rejected value as the refusal names it.
fn toml_literal(raw: &toml::Value) -> String {
    match raw {
        toml::Value::String(text) => format!("`{text}`"),
        other => other.to_string(),
    }
}

/// `[orca]` — settings for the Orca session backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OrcaSection {
    /// Where a spawned terminal's tab lands: `host` (the default) uses the
    /// worktree the spawning supervisor's own Orca tab runs in
    /// (`ORCA_WORKTREE_ID`, inherited by the daemon), `inherit` passes no
    /// selector and leaves the choice to Orca's active worktree, and any other
    /// value is used verbatim as an Orca worktree selector (`id:<…>`,
    /// `path:<abs>`, `name:<…>`, `branch:<…>`).
    ///
    /// Only the tab's home is decided here; the agent still runs in the role
    /// workspace (`cd` in the spawned command), which Orca never has to know.
    #[serde(default = "default_orca_worktree")]
    pub worktree: String,
}

impl Default for OrcaSection {
    fn default() -> Self {
        Self {
            worktree: default_orca_worktree(),
        }
    }
}

fn default_orca_worktree() -> String {
    "host".to_string()
}

/// `[acp]` — settings for the ACP session backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AcpSection {
    /// Session mode handed to the agent. Empty leaves the agent's own default.
    #[serde(default)]
    pub mode: String,
    /// Model handed to the agent. Empty leaves the agent's own default.
    #[serde(default)]
    pub model: String,
    /// Reasoning effort handed to the agent. Empty leaves the agent's own
    /// default.
    #[serde(default)]
    pub reasoning_effort: String,
    /// What the local client answers when the agent asks for permission:
    /// `deny` (the default) refuses every request and records a fault, `allow`
    /// grants it.
    #[serde(default = "default_acp_permission")]
    pub permission: String,
}

impl Default for AcpSection {
    fn default() -> Self {
        Self {
            mode: String::new(),
            model: String::new(),
            reasoning_effort: String::new(),
            permission: default_acp_permission(),
        }
    }
}

fn default_acp_permission() -> String {
    "deny".to_string()
}

pub const DEFAULT_STALL_REPORT_SECS: u64 = 1800;
pub const DEFAULT_RECONNECT_GRACE_SECS: u64 = 60;

fn default_stall_report_secs() -> u64 {
    DEFAULT_STALL_REPORT_SECS
}

fn default_reconnect_grace_secs() -> u64 {
    DEFAULT_RECONNECT_GRACE_SECS
}

impl ClientConfig {
    /// Load a client config from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SpecError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| SpecError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse_named(&text, display_name(path))
    }

    /// Parse a `config.toml` string using `config.toml` in diagnostics.
    pub fn parse_str(text: &str) -> Result<Self, SpecError> {
        Self::parse_named(text, "config.toml")
    }

    /// Parse with a display name for diagnostics.
    pub fn parse_named(text: &str, file: &str) -> Result<Self, SpecError> {
        let parsed: toml::Value = text.parse::<toml::Value>().map_err(|err| {
            SpecError::parse(
                file,
                line_from_span(text, err.span()),
                err.message().to_string(),
            )
        })?;
        validate_session(&parsed, text, file)?;
        let config: Self = parsed.clone().try_into().map_err(|err: toml::de::Error| {
            SpecError::parse(
                file,
                serde_error_line(text, err.span(), err.message()),
                err.message().to_string(),
            )
        })?;
        validate_acp(&config, text, file)?;
        crate::keys::warn_ignored(&crate::keys::client_unknown(&parsed), file);
        Ok(config)
    }

    /// Resolve every `$NAME` value in place at read time.
    pub fn resolve_secrets(&mut self, env: &Env) -> Result<(), SpecError> {
        self.cert_pin = resolve_field(&self.cert_pin, "cert_pin", env)?;
        self.key_path = resolve_field(&self.key_path, "key_path", env)?;
        self.server.host = resolve_field(&self.server.host, "server.host", env)?;
        Ok(())
    }
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
    if let Some(field) = expected_field(message) {
        return locate_field_line(text, field);
    }
    line
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

/// `[acp] permission` is `deny` | `allow`. The refusal points at the line that
/// carries the rejected value, falling back to the `[acp]` key.
fn validate_acp(config: &ClientConfig, text: &str, file: &str) -> Result<(), SpecError> {
    let permission = config.acp.permission.as_str();
    if matches!(permission, "deny" | "allow") {
        return Ok(());
    }
    Err(SpecError::parse(
        file,
        acp_permission_line(text, permission),
        format!("acp.permission must be `deny` or `allow`, got `{permission}`"),
    ))
}

/// Line of `[acp] permission`: the key inside the table when it is written
/// there, otherwise the line that carries the rejected value (dotted or inline
/// table forms).
fn acp_permission_line(text: &str, permission: &str) -> usize {
    find_key_line_in_table(text, "acp", "permission")
        .or_else(|| {
            text.lines()
                .position(|line| line.contains("permission") && line.contains(permission))
                .map(|idx| idx + 1)
        })
        .unwrap_or(1)
}

/// `[client.session] scope` is `oneshot` | `task` | `role`, and `idle_close` is
/// a duration. Both refusals point at the line that carries the rejected value.
///
/// The check runs on the parsed document, before the struct conversion, so an
/// unknown scope is refused with its own line rather than a serde message that
/// points somewhere else — and so a typo in `scope` never becomes `oneshot`.
fn validate_session(parsed: &toml::Value, text: &str, file: &str) -> Result<(), SpecError> {
    let Some(session) = parsed
        .get("client")
        .and_then(|client| client.get("session"))
    else {
        return Ok(());
    };
    if let Some(scope) = session.get("scope") {
        let written = scope.as_str().unwrap_or_default();
        if SessionScope::parse(written).is_none() {
            return Err(SpecError::parse(
                file,
                session_key_line(text, "scope", scope),
                format!(
                    "client.session.scope must be `oneshot`, `task` or `role`, got {}",
                    toml_literal(scope)
                ),
            ));
        }
    }
    if let Some(idle_close) = session.get("idle_close") {
        if idle_close_value(idle_close).is_none() {
            return Err(SpecError::parse(
                file,
                session_key_line(text, "idle_close", idle_close),
                format!(
                    "client.session.idle_close must be a duration (`30s`, `5m`, `2h`, `1d`, or \
                     bare seconds), got {}",
                    toml_literal(idle_close)
                ),
            ));
        }
    }
    Ok(())
}

/// Line of a `[client.session]` key: the key inside the table when it is
/// written there, otherwise the line that carries the rejected value (dotted
/// or inline table forms).
fn session_key_line(text: &str, key: &str, value: &toml::Value) -> usize {
    find_key_line_in_table(text, "client.session", key)
        .or_else(|| {
            let literal = value.to_string();
            text.lines()
                .position(|line| line.contains(key) && line.contains(&literal))
                .map(|idx| idx + 1)
        })
        .unwrap_or(1)
}

/// Server host and port used by the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServerEndpoint {
    /// Hostname or address, possibly a `$NAME` env reference.
    pub host: String,
    /// TCP port.
    pub port: u16,
}

fn resolve_field(raw: &str, label: &str, env: &Env) -> Result<String, SpecError> {
    if raw.trim().starts_with('$') {
        env.secret(raw, label)
    } else {
        Ok(raw.to_string())
    }
}

fn display_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml")
}
