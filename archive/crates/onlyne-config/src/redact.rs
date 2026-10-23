use crate::{client::ClientConfig, spec::Spec};
use serde::Serialize;

/// Keys whose values are considered secret in TOML and JSON renderings.
pub const SECRET_KEYS: &[&str] = &["key", "cert_pin"];

/// Keep the first six characters and hide the rest.
pub fn mask(raw: &str) -> String {
    let prefix: String = raw.chars().take(6).collect();
    if raw.chars().count() <= 6 {
        return prefix;
    }
    format!("{prefix}…")
}

/// Redact a parsed TOML value.
pub fn value(input: &toml::Value) -> String {
    let mut cloned = input.clone();
    redact_toml(&mut cloned);
    format!("{cloned:#?}")
}

/// Redact a spec struct.
pub fn spec(input: &Spec) -> String {
    json(input)
}

/// Redact a client config struct.
pub fn client(input: &ClientConfig) -> String {
    json(input)
}

/// Redact any serializable config-like struct by field name.
pub fn json(input: &impl Serialize) -> String {
    let mut value = serde_json::to_value(input).expect("config serializes to JSON");
    redact_json(&mut value);
    serde_json::to_string_pretty(&value).expect("redacted config serializes to JSON")
}

fn redact_toml(input: &mut toml::Value) {
    match input {
        toml::Value::Array(items) => {
            for item in items {
                redact_toml(item);
            }
        }
        toml::Value::Table(table) => {
            for (key, value) in table {
                if SECRET_KEYS.contains(&key.as_str()) {
                    if let toml::Value::String(raw) = value {
                        *raw = mask(raw);
                    }
                } else {
                    redact_toml(value);
                }
            }
        }
        toml::Value::String(raw) if raw.starts_with('$') => {
            *raw = mask(raw);
        }
        toml::Value::String(_)
        | toml::Value::Integer(_)
        | toml::Value::Float(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_) => {}
    }
}

fn redact_json(input: &mut serde_json::Value) {
    match input {
        serde_json::Value::Array(items) => {
            for item in items {
                redact_json(item);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if SECRET_KEYS.contains(&key.as_str()) {
                    if let serde_json::Value::String(raw) = value {
                        *raw = mask(raw);
                    }
                } else {
                    redact_json(value);
                }
            }
        }
        serde_json::Value::String(raw) if raw.starts_with('$') => {
            *raw = mask(raw);
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}
