//! Native client for the TypeSafe System One (`POST /v1/systemone`) decision API.
//!
//! System One decision models (such as TypeSafe Jev or Clef) evaluate a JSON
//! `state` against a map of named typed questions
//! ([`Question::Noul`], [`Question::Choice`], [`Question::Score`]) and return
//! calibrated probabilities and confidence scores rather than generated text.
//!
//! This module speaks HTTP/1.1 directly over [`tokio::net::TcpStream`] so that
//! `onlyne-client`, `onlyne-im`, and `onlyne-cli` can call a local or remote
//! System One endpoint without pulling `reqwest` across the binary firewall.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::NetError;

/// Default local System One gateway base URL when `ONLYNE_DECIDER_URL` / `TYPESAFE_BASE_URL` is unset.
pub const DEFAULT_DECIDER_BASE_URL: &str = "http://127.0.0.1:8080";

/// Default System One model identifier (`jev-latest`).
pub const DEFAULT_DECIDER_MODEL: &str = "jev-latest";

/// Default per-call timeout (milliseconds) so an unreachable gateway degrades immediately.
pub const DEFAULT_DECIDER_TIMEOUT_MS: u64 = 1_500;

/// Default environment variable checked for the System One gateway URL.
pub const DEFAULT_BASE_URL_ENV: &str = "ONLYNE_DECIDER_URL";

/// Default environment variable checked for the System One model identifier.
pub const DEFAULT_MODEL_ENV: &str = "ONLYNE_DECIDER_MODEL";

/// Default environment variable checked for the System One API key / Bearer token.
pub const DEFAULT_API_KEY_ENV: &str = "ONLYNE_DECIDER_API_KEY";

/// Default HTTP header used for authentication.
pub const DEFAULT_AUTH_HEADER: &str = "Authorization";

/// Default HTTP authentication scheme prefix (`Bearer`).
pub const DEFAULT_AUTH_SCHEME: &str = "Bearer";

/// Optional descriptions for the `true` and `false` outcomes of a [`Question::Noul`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoulCriteria {
    #[serde(rename = "true", default, skip_serializing_if = "Option::is_none")]
    pub yes: Option<Value>,
    #[serde(rename = "false", default, skip_serializing_if = "Option::is_none")]
    pub no: Option<Value>,
}

/// One typed question evaluated against the request `state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// Yes/no judgment returning the probability of "yes" in `[0.0, 1.0]`.
    Noul {
        instructions: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// Multiple-choice judgment over a closed map of `label -> optional rubric`.
    Choice {
        instructions: Value,
        criteria: BTreeMap<String, Option<Value>>,
    },
    /// Rubric-based rating over ordered descriptive levels (lowest to highest).
    Score {
        instructions: Value,
        criteria: Vec<Value>,
    },
}

impl Question {
    /// Build a yes/no [`Question::Noul`] with plain-text instructions.
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul {
            instructions: Value::String(instructions.into()),
            criteria: None,
        }
    }

    /// Build a [`Question::Noul`] with explicit rubrics for `true` and `false`.
    pub fn noul_with_criteria(
        instructions: impl Into<String>,
        yes: impl Into<String>,
        no: impl Into<String>,
    ) -> Self {
        Self::Noul {
            instructions: Value::String(instructions.into()),
            criteria: Some(NoulCriteria {
                yes: Some(Value::String(yes.into())),
                no: Some(Value::String(no.into())),
            }),
        }
    }

    /// Build a multiple-choice [`Question::Choice`] from `(label, optional_rubric)` pairs.
    pub fn choice<I, K>(instructions: impl Into<String>, options: I) -> Self
    where
        I: IntoIterator<Item = (K, Option<String>)>,
        K: Into<String>,
    {
        let criteria = options
            .into_iter()
            .map(|(k, v)| (k.into(), v.map(Value::String)))
            .collect();
        Self::Choice {
            instructions: Value::String(instructions.into()),
            criteria,
        }
    }

    /// Build an ordered-rubric [`Question::Score`] from level descriptions (lowest to highest).
    pub fn score<I, S>(instructions: impl Into<String>, levels: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let criteria = levels
            .into_iter()
            .map(|s| Value::String(s.into()))
            .collect();
        Self::Score {
            instructions: Value::String(instructions.into()),
            criteria,
        }
    }
}

/// Answer to a [`Question::Noul`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoulAnswer {
    /// Probability of "yes" in `[0.0, 1.0]`.
    pub noul: f64,
}

/// Answer to a [`Question::Choice`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceAnswer {
    /// Highest-probability option label.
    pub choice: String,
    /// Every option mapped to its probability.
    #[serde(default)]
    pub probabilities: BTreeMap<String, f64>,
    /// Concentration of the distribution in `[0.0, 1.0]`.
    #[serde(default)]
    pub confidence: f64,
}

/// Answer to a [`Question::Score`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreAnswer {
    /// Probability-weighted level index.
    pub score: f64,
    /// Level index (as string key) mapped to its probability.
    #[serde(default)]
    pub probabilities: BTreeMap<String, f64>,
    /// Concentration of the distribution in `[0.0, 1.0]`.
    #[serde(default)]
    pub confidence: f64,
}

/// One typed answer returned under its question key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Noul(NoulAnswer),
    Choice(ChoiceAnswer),
    Score(ScoreAnswer),
}

/// Token and cost accounting reported by a System One endpoint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemOneUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cost: Option<f64>,
}

/// Wire body sent to `POST /v1/systemone`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemOneRequest {
    pub model: String,
    pub state: Value,
    pub questions: BTreeMap<String, Question>,
}

/// Wire body returned by `POST /v1/systemone`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemOneResponse {
    #[serde(default)]
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    #[serde(default)]
    pub usage: Option<SystemOneUsage>,
}

/// Embedded default meta-programmable specification for all five System-1 edge reflexes.
pub const DECIDER_DEFAULTS_JSON: &str = include_str!("decider_defaults.json");

/// Declarative, meta-programmable specification for one System-1 edge reflex.
///
/// Holds the question templates (`noul`, `choice`, `score`), numeric thresholds,
/// and string templates for one checkpoint so that no prompt text or rubric is
/// hardcoded in procedural Rust logic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReflexSpec {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub questions: BTreeMap<String, Question>,
    #[serde(default)]
    pub thresholds: BTreeMap<String, f64>,
    #[serde(default)]
    pub templates: BTreeMap<String, String>,
}

fn default_true() -> bool {
    true
}

fn default_base_url() -> String {
    DEFAULT_DECIDER_BASE_URL.to_string()
}

fn default_base_url_env() -> String {
    DEFAULT_BASE_URL_ENV.to_string()
}

fn default_model() -> String {
    DEFAULT_DECIDER_MODEL.to_string()
}

fn default_model_env() -> String {
    DEFAULT_MODEL_ENV.to_string()
}

fn default_api_key_env() -> String {
    DEFAULT_API_KEY_ENV.to_string()
}

fn default_auth_header() -> String {
    DEFAULT_AUTH_HEADER.to_string()
}

fn default_auth_scheme() -> String {
    DEFAULT_AUTH_SCHEME.to_string()
}

fn default_timeout_ms() -> u64 {
    DEFAULT_DECIDER_TIMEOUT_MS
}

/// Connection, model, `.env` file, environment-variable indirection, and HTTP
/// authentication settings for the System One decision endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeciderClientSpec {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_base_url_env")]
    pub base_url_env: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_model_env")]
    pub model_env: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_auth_header")]
    pub auth_header: String,
    #[serde(default = "default_auth_scheme")]
    pub auth_scheme: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub env_file: Option<String>,
}

impl Default for DeciderClientSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url: default_base_url(),
            base_url_env: default_base_url_env(),
            model: default_model(),
            model_env: default_model_env(),
            api_key: None,
            api_key_env: default_api_key_env(),
            auth_header: default_auth_header(),
            auth_scheme: default_auth_scheme(),
            timeout_ms: default_timeout_ms(),
            env_file: None,
        }
    }
}

impl Default for ReflexSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            questions: BTreeMap::new(),
            thresholds: BTreeMap::new(),
            templates: BTreeMap::new(),
        }
    }
}

impl ReflexSpec {
    /// Look up a numeric threshold by key, returning `default` when absent.
    pub fn threshold(&self, key: &str, default: f64) -> f64 {
        self.thresholds.get(key).copied().unwrap_or(default)
    }

    /// Look up a string template by key, returning `default` when absent.
    pub fn template<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.templates
            .get(key)
            .map(String::as_str)
            .unwrap_or(default)
    }

    /// Render this reflex's questions with `{var}` interpolation against top-level
    /// keys of `state`.
    pub fn render_questions(&self, state: &Value) -> BTreeMap<String, Question> {
        self.questions
            .iter()
            .map(|(id, q)| (id.clone(), interpolate_question(q, state)))
            .collect()
    }
}

/// Complete meta-programmable specification for Onlyne's five System-1 edge reflexes.
///
/// Loaded from [`DECIDER_DEFAULTS_JSON`] and deep-merged with optional overrides
/// from `<root>/.onlyne/decider.json` and `ONLYNE_DECIDER_SPEC`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeciderSpec {
    #[serde(default)]
    pub client: DeciderClientSpec,
    pub im_inbound: ReflexSpec,
    pub complete_guard: ReflexSpec,
    pub turn_end: ReflexSpec,
    pub heartbeat: ReflexSpec,
    pub triage: ReflexSpec,
}

impl Default for DeciderSpec {
    fn default() -> Self {
        Self::defaults()
    }
}

impl DeciderSpec {
    /// Parse the embedded [`DECIDER_DEFAULTS_JSON`] baseline.
    pub fn defaults() -> Self {
        serde_json::from_str(DECIDER_DEFAULTS_JSON)
            .expect("embedded decider_defaults.json is valid DeciderSpec")
    }

    /// Load defaults and apply any `ONLYNE_DECIDER_SPEC` override (file path or inline JSON).
    pub fn from_env() -> Self {
        Self::load_for_root(None)
    }

    /// Load defaults, deep-merge any `<ancestor>/.onlyne/decider.json` from
    /// outermost cluster root to innermost workspace `root`, and then deep-merge
    /// `ONLYNE_DECIDER_SPEC` (inline JSON or file path) if set in the process
    /// environment or `.env`.
    pub fn load_for_root(root: Option<&std::path::Path>) -> Self {
        let mut base: Value = serde_json::from_str(DECIDER_DEFAULTS_JSON)
            .expect("embedded decider_defaults.json is valid JSON");
        for dir in ancestor_dirs(root) {
            let candidate = dir.join(".onlyne/decider.json");
            if let Ok(text) = std::fs::read_to_string(&candidate)
                && let Ok(overlay) = serde_json::from_str::<Value>(&text)
            {
                merge_json(&mut base, overlay);
            }
        }
        let dotenv = load_dotenv_map(root, None);
        if let Some(spec_env) = resolve_env_var("ONLYNE_DECIDER_SPEC", &dotenv) {
            let overlay_opt = if spec_env.starts_with('{') {
                serde_json::from_str::<Value>(&spec_env).ok()
            } else {
                std::fs::read_to_string(&spec_env)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            };
            if let Some(overlay) = overlay_opt {
                merge_json(&mut base, overlay);
            }
        }
        serde_json::from_value(base).unwrap_or_else(|_| Self::defaults())
    }

    /// Load defaults and deep-merge an explicit JSON file at `path`.
    pub fn load_from_path(path: &std::path::Path) -> Result<Self, NetError> {
        let mut base: Value = serde_json::from_str(DECIDER_DEFAULTS_JSON)
            .expect("embedded decider_defaults.json is valid JSON");
        let text = std::fs::read_to_string(path)
            .map_err(|e| NetError::Io(format!("reading {}: {e}", path.display())))?;
        let overlay: Value = serde_json::from_str(&text)
            .map_err(|e| NetError::Io(format!("parsing {}: {e}", path.display())))?;
        merge_json(&mut base, overlay);
        serde_json::from_value(base)
            .map_err(|e| NetError::Io(format!("invalid DeciderSpec in {}: {e}", path.display())))
    }

    /// Build a [`SystemOneClient`] from this spec's `client` block, resolving
    /// `.env` files under `root` and environment variables named by `*_env` or
    /// `$VAR` / `${VAR}` placeholders. Returns `None` when disabled.
    pub fn build_client(&self, root: Option<&std::path::Path>) -> Option<SystemOneClient> {
        let dotenv = load_dotenv_map(root, self.client.env_file.as_deref());
        let toggle =
            resolve_env_var("ONLYNE_DECIDER", &dotenv).map(|v| v.trim().to_ascii_lowercase());
        if matches!(toggle.as_deref(), Some("0" | "false" | "off" | "no")) {
            return None;
        }
        let explicitly_enabled = matches!(toggle.as_deref(), Some("1" | "true" | "on" | "yes"));
        if !self.client.enabled && !explicitly_enabled {
            return None;
        }
        if !explicitly_enabled
            && std::env::var("ONLYNE_BACKEND")
                .ok()
                .is_some_and(|b| b.trim().eq_ignore_ascii_case("fake"))
        {
            return None;
        }

        Some(self.build_client_unchecked(root, &dotenv))
    }

    /// Build a [`SystemOneClient`] from this spec's `client` block even when
    /// `ONLYNE_BACKEND=fake` is set (used by explicit CLI commands like `onlyne triage`).
    pub fn build_client_or_default(&self, root: Option<&std::path::Path>) -> SystemOneClient {
        let dotenv = load_dotenv_map(root, self.client.env_file.as_deref());
        self.build_client_unchecked(root, &dotenv)
    }

    fn build_client_unchecked(
        &self,
        _root: Option<&std::path::Path>,
        dotenv: &BTreeMap<String, String>,
    ) -> SystemOneClient {
        let cfg = &self.client;

        let env_url = non_empty_trimmed(&cfg.base_url_env)
            .and_then(|k| resolve_env_var(k, dotenv))
            .or_else(|| {
                if cfg.base_url == DEFAULT_DECIDER_BASE_URL {
                    resolve_env_var("TYPESAFE_BASE_URL", dotenv)
                } else {
                    None
                }
            });
        let base_url = env_url
            .unwrap_or_else(|| expand_env_refs(&cfg.base_url, dotenv))
            .trim_end_matches('/')
            .to_string();

        let env_model = non_empty_trimmed(&cfg.model_env)
            .and_then(|k| resolve_env_var(k, dotenv))
            .or_else(|| {
                if cfg.model == DEFAULT_DECIDER_MODEL {
                    resolve_env_var("TYPESAFE_DEFAULT_MODEL", dotenv)
                } else {
                    None
                }
            });
        let model = env_model.unwrap_or_else(|| expand_env_refs(&cfg.model, dotenv));

        let env_key = non_empty_trimmed(&cfg.api_key_env)
            .and_then(|k| resolve_env_var(k, dotenv))
            .or_else(|| resolve_env_var("TYPESAFE_API_KEY", dotenv));
        let api_key = env_key.or_else(|| {
            cfg.api_key
                .as_deref()
                .map(|raw| expand_env_refs(raw, dotenv))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

        let timeout_ms = resolve_env_var("ONLYNE_DECIDER_TIMEOUT_MS", dotenv)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(cfg.timeout_ms);

        SystemOneClient {
            base_url: if base_url.is_empty() {
                DEFAULT_DECIDER_BASE_URL.to_string()
            } else {
                base_url
            },
            model: if model.trim().is_empty() {
                DEFAULT_DECIDER_MODEL.to_string()
            } else {
                model
            },
            api_key,
            auth_header: cfg.auth_header.clone(),
            auth_scheme: cfg.auth_scheme.clone(),
            timeout: Duration::from_millis(timeout_ms.max(100)),
        }
    }
}

/// Deep-merge `overlay` into `base` (objects merge recursively; scalars and arrays replace).
pub fn merge_json(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            for (k, v) in overlay_map {
                match base_map.get_mut(&k) {
                    Some(existing) => merge_json(existing, v),
                    None => {
                        base_map.insert(k, v);
                    }
                }
            }
        }
        (base_slot, replacement) => *base_slot = replacement,
    }
}

/// Substitute `{key}` placeholders in `value` using top-level fields of `state`.
pub fn interpolate_value(value: &Value, state: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(interpolate_str(s, state)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| interpolate_value(item, state))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), interpolate_value(v, state)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn interpolate_str(template: &str, state: &Value) -> String {
    let Some(obj) = state.as_object() else {
        return template.to_string();
    };
    if !template.contains('{') {
        return template.to_string();
    }
    let mut out = template.to_string();
    for (k, v) in obj {
        let needle = format!("{{{k}}}");
        if out.contains(&needle) {
            let replacement = match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            out = out.replace(&needle, &replacement);
        }
    }
    out
}

fn interpolate_question(q: &Question, state: &Value) -> Question {
    match q {
        Question::Noul {
            instructions,
            criteria,
        } => Question::Noul {
            instructions: interpolate_value(instructions, state),
            criteria: criteria.as_ref().map(|c| NoulCriteria {
                yes: c.yes.as_ref().map(|v| interpolate_value(v, state)),
                no: c.no.as_ref().map(|v| interpolate_value(v, state)),
            }),
        },
        Question::Choice {
            instructions,
            criteria,
        } => Question::Choice {
            instructions: interpolate_value(instructions, state),
            criteria: criteria
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_ref().map(|val| interpolate_value(val, state)),
                    )
                })
                .collect(),
        },
        Question::Score {
            instructions,
            criteria,
        } => Question::Score {
            instructions: interpolate_value(instructions, state),
            criteria: criteria
                .iter()
                .map(|v| interpolate_value(v, state))
                .collect(),
        },
    }
}

impl SystemOneResponse {
    /// Extract the `noul` probability (`0.0..=1.0`) for question `id`.
    pub fn noul(&self, id: &str) -> Option<f64> {
        match self.answers.get(id)? {
            Answer::Noul(ans) => Some(ans.noul),
            _ => None,
        }
    }

    /// Extract the [`ChoiceAnswer`] for question `id`.
    pub fn choice(&self, id: &str) -> Option<&ChoiceAnswer> {
        match self.answers.get(id)? {
            Answer::Choice(ans) => Some(ans),
            _ => None,
        }
    }

    /// Extract the [`ScoreAnswer`] for question `id`.
    pub fn score(&self, id: &str) -> Option<&ScoreAnswer> {
        match self.answers.get(id)? {
            Answer::Score(ans) => Some(ans),
            _ => None,
        }
    }
}

/// Async client for a TypeSafe System One endpoint (`POST /v1/systemone`).
#[derive(Debug, Clone)]
pub struct SystemOneClient {
    base_url: String,
    model: String,
    api_key: Option<String>,
    auth_header: String,
    auth_scheme: String,
    timeout: Duration,
}

impl SystemOneClient {
    /// Construct a client targeting `base_url` and `model`.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Self {
            base_url,
            model: model.into(),
            api_key: None,
            auth_header: DEFAULT_AUTH_HEADER.to_string(),
            auth_scheme: DEFAULT_AUTH_SCHEME.to_string(),
            timeout: Duration::from_millis(DEFAULT_DECIDER_TIMEOUT_MS),
        }
    }

    /// Build a client from environment variables, or `None` when disabled.
    pub fn from_env() -> Option<Self> {
        Self::from_root(None)
    }

    /// Build a client by loading `<root>/.onlyne/decider.json`, `.env` files,
    /// and environment variables, or `None` when disabled.
    pub fn from_root(root: Option<&std::path::Path>) -> Option<Self> {
        DeciderSpec::load_for_root(root).build_client(root)
    }

    pub fn with_api_key(mut self, api_key: Option<String>) -> Self {
        self.api_key = api_key.filter(|k| !k.trim().is_empty());
        self
    }

    pub fn with_auth_header(
        mut self,
        header: impl Into<String>,
        scheme: impl Into<String>,
    ) -> Self {
        self.auth_header = header.into();
        self.auth_scheme = scheme.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// Evaluate `questions` over `state` via a single batched `POST /v1/systemone` request.
    pub async fn evaluate(
        &self,
        state: Value,
        questions: BTreeMap<String, Question>,
    ) -> Result<SystemOneResponse, NetError> {
        if questions.is_empty() {
            return Err(NetError::Io(
                "systemone request requires at least one question".to_string(),
            ));
        }
        let req_body = SystemOneRequest {
            model: self.model.clone(),
            state,
            questions,
        };
        let payload =
            serde_json::to_vec(&req_body).map_err(|e| NetError::Io(format!("json encode: {e}")))?;
        let timeout = self.timeout;
        tokio::time::timeout(timeout, self.post_systemone(&payload))
            .await
            .map_err(|_| NetError::Io(format!("systemone request timed out after {timeout:?}")))?
    }

    async fn post_systemone(&self, payload: &[u8]) -> Result<SystemOneResponse, NetError> {
        let (host_port, host_header, path_prefix) = parse_http_base(&self.base_url)?;
        let path = if path_prefix.ends_with("/v1/systemone") || path_prefix.ends_with("/decisions")
        {
            path_prefix
        } else if path_prefix.ends_with("/v1") {
            format!("{path_prefix}/systemone")
        } else {
            format!("{path_prefix}/v1/systemone")
        };

        let addresses = tokio::net::lookup_host(&host_port).await?;
        let mut last_err = None;
        let mut stream_opt = None;
        for addr in addresses {
            match TcpStream::connect(addr).await {
                Ok(s) => {
                    stream_opt = Some(s);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let mut stream = match stream_opt {
            Some(s) => s,
            None => {
                return Err(last_err.map(NetError::from).unwrap_or_else(|| {
                    NetError::Io(format!("endpoint {host_port} resolved to no addresses"))
                }));
            }
        };

        let mut request = format!(
            "POST {path} HTTP/1.1\r\n\
             Host: {host_header}\r\n\
             User-Agent: onlyne\r\n\
             Content-Type: application/json\r\n\
             Accept: application/json\r\n\
             Connection: close\r\n\
             Content-Length: {}\r\n",
            payload.len()
        );
        if let Some(token) = &self.api_key {
            let header_name = if self.auth_header.trim().is_empty() {
                DEFAULT_AUTH_HEADER
            } else {
                self.auth_header.trim()
            };
            let scheme = self.auth_scheme.trim();
            let trimmed_token = token.trim();
            let already_prefixed = !scheme.is_empty()
                && trimmed_token.len() > scheme.len()
                && trimmed_token[..scheme.len()].eq_ignore_ascii_case(scheme)
                && trimmed_token.as_bytes().get(scheme.len()) == Some(&b' ');
            let header_val = if scheme.is_empty() || already_prefixed {
                trimmed_token.to_string()
            } else {
                format!("{scheme} {trimmed_token}")
            };
            request.push_str(&format!("{header_name}: {header_val}\r\n"));
        }
        request.push_str("\r\n");

        let mut wire = Vec::with_capacity(request.len() + payload.len());
        wire.extend_from_slice(request.as_bytes());
        wire.extend_from_slice(payload);
        stream.write_all(&wire).await?;
        stream.flush().await?;

        let mut raw = Vec::with_capacity(4096);
        stream.read_to_end(&mut raw).await?;
        let (status, body) = decode_http_response(&raw)?;
        if !(200..300).contains(&status) {
            let text = String::from_utf8_lossy(&body);
            return Err(NetError::Io(format!(
                "systemone HTTP {status}: {}",
                text.trim()
            )));
        }
        serde_json::from_slice::<SystemOneResponse>(&body)
            .map_err(|e| NetError::Io(format!("systemone decode: {e}")))
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn non_empty_trimmed(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t) }
}

fn ancestor_dirs(root: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
    let start = root
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    let Some(start) = start else {
        return Vec::new();
    };
    let mut chain: Vec<std::path::PathBuf> = start
        .ancestors()
        .take(5)
        .map(std::path::Path::to_path_buf)
        .collect();
    chain.reverse();
    chain
}

/// Parse `.env` files discovered from `root` (and its ancestors) plus an optional
/// explicit `env_file` path. Closer directories override parent directories.
pub fn load_dotenv_map(
    root: Option<&std::path::Path>,
    env_file: Option<&str>,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for dir in ancestor_dirs(root) {
        for rel in [".env", ".onlyne/.env"] {
            let path = dir.join(rel);
            if let Ok(content) = std::fs::read_to_string(&path) {
                parse_dotenv_into(&content, &mut map);
            }
        }
    }
    if let Some(custom) = env_file.and_then(non_empty_trimmed) {
        let custom_path = std::path::Path::new(custom);
        let mut candidates = Vec::new();
        if custom_path.is_absolute() {
            candidates.push(custom_path.to_path_buf());
        } else {
            for dir in ancestor_dirs(root) {
                candidates.push(dir.join(custom_path));
            }
        }
        for path in candidates {
            if let Ok(content) = std::fs::read_to_string(&path) {
                parse_dotenv_into(&content, &mut map);
            }
        }
    }
    map
}

fn parse_dotenv_into(content: &str, out: &mut BTreeMap<String, String>) {
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export ")
            .map(str::trim_start)
            .unwrap_or(line);
        let Some((raw_key, raw_val)) = line.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        if key.is_empty() {
            continue;
        }
        let val = raw_val.trim();
        let parsed = if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
            val[1..val.len() - 1]
                .replace("\\\"", "\"")
                .replace("\\\\", "\\")
                .replace("\\n", "\n")
        } else if val.len() >= 2 && val.starts_with('\'') && val.ends_with('\'') {
            val[1..val.len() - 1].to_string()
        } else {
            let without_comment = match val.find(" #") {
                Some(idx) => val[..idx].trim_end(),
                None => val,
            };
            without_comment.to_string()
        };
        if !parsed.is_empty() {
            out.insert(key.to_string(), parsed);
        }
    }
}

fn resolve_env_var(key: &str, dotenv: &BTreeMap<String, String>) -> Option<String> {
    non_empty_env(key).or_else(|| {
        dotenv
            .get(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

fn expand_env_refs(input: &str, dotenv: &BTreeMap<String, String>) -> String {
    let trimmed = input.trim();
    if let Some(var_name) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return resolve_env_var(var_name.trim(), dotenv).unwrap_or_default();
    }
    if let Some(var_name) = trimmed.strip_prefix('$')
        && !var_name.is_empty()
        && var_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return resolve_env_var(var_name, dotenv).unwrap_or_default();
    }
    let mut out = input.to_string();
    while let Some(start) = out.find("${") {
        let Some(rel_end) = out[start + 2..].find('}') else {
            break;
        };
        let end = start + 2 + rel_end;
        let var_name = out[start + 2..end].trim();
        let replacement = resolve_env_var(var_name, dotenv).unwrap_or_default();
        out.replace_range(start..=end, &replacement);
    }
    out
}

fn parse_http_base(url: &str) -> Result<(String, String, String), NetError> {
    let rest = if let Some(stripped) = url.strip_prefix("http://") {
        stripped
    } else if url.starts_with("https://") {
        return Err(NetError::Io(
            "systemone client requires an http:// gateway URL (set ONLYNE_DECIDER_URL or TYPESAFE_BASE_URL)"
                .to_string(),
        ));
    } else {
        url
    };
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err(NetError::Io(format!("invalid systemone URL: {url}")));
    }
    let host_port = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };
    let path_prefix = path.trim_end_matches('/').to_string();
    Ok((host_port, authority.to_string(), path_prefix))
}

fn decode_http_response(raw: &[u8]) -> Result<(u16, Vec<u8>), NetError> {
    let Some(header_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Err(NetError::Io("malformed HTTP response header".to_string()));
    };
    let header_str = String::from_utf8_lossy(&raw[..header_end]);
    let mut lines = header_str.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| NetError::Io("empty HTTP status line".to_string()))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| NetError::Io(format!("invalid HTTP status line: {status_line}")))?;

    let is_chunked = lines.any(|line| {
        if let Some((k, v)) = line.split_once(':') {
            k.trim().eq_ignore_ascii_case("transfer-encoding")
                && v.to_ascii_lowercase().contains("chunked")
        } else {
            false
        }
    });

    let body_slice = &raw[header_end + 4..];
    let body = if is_chunked {
        decode_chunked_body(body_slice)?
    } else {
        body_slice.to_vec()
    };
    Ok((status, body))
}

fn decode_chunked_body(mut input: &[u8]) -> Result<Vec<u8>, NetError> {
    let mut out = Vec::new();
    while let Some(line_end) = input.windows(2).position(|w| w == b"\r\n") {
        let size_str = String::from_utf8_lossy(&input[..line_end]);
        let size_hex = size_str.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|e| NetError::Io(format!("invalid chunk size {size_hex:?}: {e}")))?;
        input = &input[line_end + 2..];
        if size == 0 {
            break;
        }
        if input.len() < size {
            return Err(NetError::Io("truncated chunked HTTP body".to_string()));
        }
        out.extend_from_slice(&input[..size]);
        input = &input[size..];
        if input.starts_with(b"\r\n") {
            input = &input[2..];
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn evaluates_noul_choice_and_score_against_mock_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let req_text = String::from_utf8_lossy(&buf[..n]);
            assert!(req_text.starts_with("POST /v1/systemone HTTP/1.1\r\n"));
            let body_json = json!({
                "model": "clef-flash",
                "answers": {
                    "should_handle": { "type": "noul", "noul": 0.94 },
                    "route": {
                        "type": "choice",
                        "choice": "coder",
                        "confidence": 0.89,
                        "probabilities": { "coder": 0.89, "planner": 0.11 }
                    },
                    "severity": {
                        "type": "score",
                        "score": 1.75,
                        "confidence": 0.82,
                        "probabilities": { "0": 0.05, "1": 0.15, "2": 0.80 }
                    }
                },
                "usage": { "input_tokens": 120, "output_tokens": 0 }
            });
            let body_bytes = serde_json::to_vec(&body_json).unwrap();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body_bytes.len()
            );
            stream.write_all(resp.as_bytes()).await.unwrap();
            stream.write_all(&body_bytes).await.unwrap();
        });

        let client = SystemOneClient::new(format!("http://{addr}"), "jev-latest");
        let mut questions = BTreeMap::new();
        questions.insert(
            "should_handle".to_string(),
            Question::noul("Is this a task for the cluster?"),
        );
        questions.insert(
            "route".to_string(),
            Question::choice(
                "Which role should handle this?",
                [
                    ("coder", Some("Code changes and bug fixes".to_string())),
                    ("planner", Some("Architecture and planning".to_string())),
                ],
            ),
        );
        questions.insert(
            "severity".to_string(),
            Question::score("How urgent is this?", ["low", "medium", "high"]),
        );

        let response = client
            .evaluate(json!({"text": "Fix the panic in link.rs"}), questions)
            .await
            .unwrap();
        assert_eq!(response.noul("should_handle"), Some(0.94));
        assert_eq!(response.choice("route").unwrap().choice, "coder");
        assert_eq!(response.choice("route").unwrap().confidence, 0.89);
        assert_eq!(response.score("severity").unwrap().score, 1.75);
    }

    #[test]
    fn decodes_chunked_http_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                    1c\r\n{\"model\":\"jev\",\"answers\":{}}\r\n0\r\n\r\n";
        let (status, body) = decode_http_response(raw).unwrap();
        assert_eq!(status, 200);
        let parsed: SystemOneResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.model, "jev");
        assert!(parsed.answers.is_empty());
    }

    #[test]
    fn decider_spec_deep_merges_overrides_and_interpolates_state() {
        let mut base = serde_json::to_value(DeciderSpec::defaults()).unwrap();
        merge_json(
            &mut base,
            json!({
                "triage": {
                    "thresholds": { "min_confidence": 0.82 },
                    "questions": {
                        "action": {
                            "instructions": "Triage task {task_id} on role {role}"
                        },
                        "custom_noul": {
                            "type": "noul",
                            "instructions": "Does {task_id} need immediate paging?"
                        }
                    }
                }
            }),
        );
        let spec: DeciderSpec = serde_json::from_value(base).unwrap();
        assert_eq!(spec.triage.threshold("min_confidence", 0.60), 0.82);
        let rendered = spec
            .triage
            .render_questions(&json!({"task_id": "t-99", "role": "coder"}));
        assert!(rendered.contains_key("severity"));
        assert!(rendered.contains_key("custom_noul"));
        match &rendered["action"] {
            Question::Choice {
                instructions,
                criteria,
            } => {
                assert_eq!(instructions, &json!("Triage task t-99 on role coder"));
                assert!(criteria.contains_key("retry"));
            }
            other => panic!("expected choice, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn loads_dotenv_custom_env_names_and_sends_auth_header() {
        let tmp = tempfile::tempdir().unwrap();
        let cluster_root = tmp.path();
        let ws_root = cluster_root.join("planner");
        std::fs::create_dir_all(cluster_root.join(".onlyne")).unwrap();
        std::fs::create_dir_all(ws_root.join(".onlyne")).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        std::fs::write(
            cluster_root.join(".env"),
            format!(
                "# Cluster-level dotenv\n\
                 export MY_GATEWAY_URL=\"http://{addr}\"\n\
                 MY_DECIDER_MODEL=clef-pro # inline comment\n\
                 MY_SECRET_TOKEN='tok_live_987654'\n"
            ),
        )
        .unwrap();

        std::fs::write(
            cluster_root.join(".onlyne/decider.json"),
            serde_json::to_string_pretty(&json!({
                "client": {
                    "base_url_env": "MY_GATEWAY_URL",
                    "model_env": "MY_DECIDER_MODEL",
                    "api_key_env": "MY_SECRET_TOKEN"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        // Role workspace is a child of cluster_root and inherits both .env and decider.json!
        let spec = DeciderSpec::load_for_root(Some(&ws_root));
        let client = spec.build_client_or_default(Some(&ws_root));
        assert_eq!(client.base_url(), format!("http://{addr}"));
        assert_eq!(client.model(), "clef-pro");
        assert_eq!(client.api_key(), Some("tok_live_987654"));

        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let req_text = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = seen_tx.send(req_text);
            let body_bytes = br#"{"model":"clef-pro","answers":{"ok":{"type":"noul","noul":1.0}}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body_bytes.len()
            );
            stream.write_all(resp.as_bytes()).await.unwrap();
            stream.write_all(body_bytes).await.unwrap();
        });

        let mut q = BTreeMap::new();
        q.insert("ok".to_string(), Question::noul("ok?"));
        let ans = client.evaluate(json!({}), q).await.unwrap();
        assert_eq!(ans.noul("ok"), Some(1.0));

        let raw_http = seen_rx.await.unwrap();
        assert!(
            raw_http.contains("\r\nAuthorization: Bearer tok_live_987654\r\n"),
            "expected Bearer token header in HTTP request, got:\n{raw_http}"
        );
    }
}
