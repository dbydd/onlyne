use crate::{SpecError, env::Env, locate::line_from_span};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Client-side `<workspace>/.onlyne/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
        let config: Self = parsed.try_into().map_err(|err: toml::de::Error| {
            SpecError::parse(
                file,
                serde_error_line(text, err.span(), err.message()),
                err.message().to_string(),
            )
        })?;
        Ok(config)
    }

    /// Collect every `$NAME` value in the config. Each tuple carries the
    /// configuration field label and the environment variable name.
    pub fn secret_refs(&self) -> Vec<(String, String)> {
        let mut refs = Vec::new();
        if let Some(name) = indirect(&self.cert_pin) {
            refs.push(("cert_pin".to_string(), name));
        }
        if let Some(name) = indirect(&self.key_path) {
            refs.push(("key_path".to_string(), name));
        }
        if let Some(name) = indirect(&self.server.host) {
            refs.push(("server.host".to_string(), name));
        }
        refs
    }

    /// Resolve every `$NAME` value in place at read time.
    pub fn resolve_secrets(&mut self, env: &Env) -> Result<(), SpecError> {
        self.cert_pin = resolve_field(&self.cert_pin, "cert_pin", env)?;
        self.key_path = resolve_field(&self.key_path, "key_path", env)?;
        self.server.host = resolve_field(&self.server.host, "server.host", env)?;
        Ok(())
    }

    /// Resolved view of the config, for callers that bind fresh maps once.
    pub fn resolved(&self, env: &Env) -> Result<ResolvedClientConfig, SpecError> {
        Ok(ResolvedClientConfig {
            role: self.role.clone(),
            server: ResolvedEndpoint {
                host: resolve_field(&self.server.host, "server.host", env)?,
                port: self.server.port,
            },
            cert_pin: resolve_field(&self.cert_pin, "cert_pin", env)?,
            key_path: resolve_field(&self.key_path, "key_path", env)?,
            plugins: self.plugins.clone(),
        })
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

/// Server host and port pair used by the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerEndpoint {
    /// Hostname or address, possibly a `$NAME` env reference.
    pub host: String,
    /// TCP port.
    pub port: u16,
}

/// Client config with every `$NAME` value resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedClientConfig {
    /// Role name.
    pub role: String,
    /// Resolved endpoint.
    pub server: ResolvedEndpoint,
    /// Resolved certificate pin.
    pub cert_pin: String,
    /// Resolved key path.
    pub key_path: String,
    /// Plugin list.
    pub plugins: Vec<String>,
}

/// Resolved server endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    /// Resolved host.
    pub host: String,
    /// TCP port.
    pub port: u16,
}

fn indirect(value: &str) -> Option<String> {
    value
        .trim()
        .strip_prefix('$')
        .map(|name| name.trim().to_string())
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
