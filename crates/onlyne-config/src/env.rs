use crate::SpecError;
use std::{collections::HashMap, path::Path};

/// Environment resolver for `$NAME` indirection.
///
/// Values are read at load time. Resolved values are only exposed to callers that
/// request them, and redaction masks them in debug output.
#[derive(Debug, Clone, Default)]
pub struct Env {
    vars: HashMap<String, String>,
}

impl Env {
    /// Capture the process environment.
    pub fn current() -> Self {
        Self {
            vars: std::env::vars().collect(),
        }
    }

    /// Capture process environment plus key-value files.
    pub fn load(paths: &[impl AsRef<Path>]) -> Self {
        let mut vars = HashMap::new();
        for path in paths {
            read_dotenv(path.as_ref(), &mut vars);
        }
        for (k, v) in std::env::vars() {
            vars.insert(k, v);
        }
        Self { vars }
    }

    /// Build a resolver from explicit variables. Tests use this to avoid global
    /// process state.
    pub fn from_vars<K, V, I>(vars: I) -> Self
    where
        K: Into<String>,
        V: Into<String>,
        I: IntoIterator<Item = (K, V)>,
    {
        Self {
            vars: vars
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }

    /// Resolve a value. Strings starting with `$` name an environment variable.
    pub fn value(&self, raw: &str) -> Option<String> {
        let value = raw.trim();
        if value.is_empty() {
            None
        } else if let Some(name) = value.strip_prefix('$') {
            self.vars
                .get(name)
                .cloned()
                .filter(|s| !s.trim().is_empty())
        } else {
            Some(value.to_string())
        }
    }

    /// Resolve a required secret by field label.
    pub fn secret(&self, raw: &str, label: &str) -> Result<String, SpecError> {
        self.value(raw).ok_or_else(|| {
            let var = raw
                .trim()
                .strip_prefix('$')
                .map(str::to_string)
                .unwrap_or_else(|| label.to_string());
            SpecError::Secret {
                var,
                label: label.to_string(),
            }
        })
    }
}

fn read_dotenv(path: &Path, vars: &mut HashMap<String, String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let value = v.trim().trim_matches('"').trim_matches('\'').to_string();
        vars.insert(k.trim().to_string(), value);
    }
}
