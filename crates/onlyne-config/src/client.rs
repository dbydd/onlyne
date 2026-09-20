use crate::{
    SpecError,
    env::Env,
    locate::{find_key_line_in_table, line_from_span},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

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
    /// Seconds a startup reconcile waits before declaring an orphaned in-flight task stale.
    #[serde(default = "default_stale_grace_secs")]
    pub stale_grace_secs: u64,
    /// Seconds a running session may sit without an Applied tuple change before
    /// this client reports it stalled. Zero disables the report.
    #[serde(default = "default_stall_report_secs")]
    pub stall_report_secs: u64,
    /// Seconds a dropped plugin connection may stay away before this client
    /// retires the task-free session it left behind. Zero disables the sweep.
    #[serde(default = "default_reconnect_grace_secs")]
    pub reconnect_grace_secs: u64,
    /// Requested session backend (`herdr` | `orca` | `zellij` | `exec` /
    /// `headless` | `acp` | `fake` | `auto`). Empty means auto-detect. The
    /// process environment `ONLYNE_BACKEND` takes precedence when it is
    /// nonempty.
    #[serde(default)]
    pub backend: String,
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

pub const DEFAULT_STALE_GRACE_SECS: u64 = 300;
pub const DEFAULT_STALL_REPORT_SECS: u64 = 1800;
pub const DEFAULT_RECONNECT_GRACE_SECS: u64 = 60;

fn default_stale_grace_secs() -> u64 {
    DEFAULT_STALE_GRACE_SECS
}

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

/// Server host and port pair used by the client.
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
