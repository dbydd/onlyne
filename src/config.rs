use anyhow::{Context, anyhow};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::{collections::HashMap, path::Path};

pub const DEFAULT_DOTENV: &str = r#"# Onlyne workspace-local secrets.
# TELEGRAM_BOT_TOKEN=
# FEISHU_APP_ID=
# FEISHU_APP_SECRET=
# QQBOT_APP_ID=
# QQBOT_APP_SECRET=
# WEIXIN_ILINK_TOKEN=
"#;

pub const DEFAULT_CONFIG: &str = r#"#:schema ./onlyne-config.schema.json
[workspace]
name = "onlyne"

[io]
in_format = "markdown"
out_content = "latest_only"
out_cursor = "consume"
history_context_messages = 20

[loopback.io]
in_format = "markdown"
out_content = "latest_only"
out_cursor = "consume"
history_context_messages = 20

[adapters.telegram]
enabled = false
token = "$TELEGRAM_BOT_TOKEN"
bind_conversation_id = ""

[adapters.feishu]
enabled = false
app_id = "$FEISHU_APP_ID"
app_secret = "$FEISHU_APP_SECRET"
rich_text = true
bind_conversation_id = ""

[adapters.qqbot]
enabled = false
app_id = "$QQBOT_APP_ID"
app_secret = "$QQBOT_APP_SECRET"
sandbox = false
rich_text = true
bind_conversation_id = ""

[adapters.wechat]
enabled = false
token = "$WEIXIN_ILINK_TOKEN"
base_url = ""
cdn_base_url = "https://novac2c.cdn.weixin.qq.com/c2c"
bind_conversation_id = ""

[rich_text]
max_attachment_bytes = 26214400
"#;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct Config {
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub adapters: AdapterConfigs,
    #[serde(default)]
    pub rich_text: RichTextConfig,
    #[serde(default)]
    pub io: IoConfig,
    #[serde(default)]
    pub loopback: LoopbackConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RichTextConfig {
    #[serde(default = "default_max_attachment_bytes")]
    pub max_attachment_bytes: u64,
}
impl Default for RichTextConfig {
    fn default() -> Self {
        Self {
            max_attachment_bytes: default_max_attachment_bytes(),
        }
    }
}
fn default_max_attachment_bytes() -> u64 {
    25 * 1024 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IoInFormat {
    #[default]
    Markdown,
    RawText,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IoOutContent {
    #[default]
    LatestOnly,
    WithHistory,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IoOutCursor {
    Retain,
    #[default]
    Consume,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IoConfig {
    #[serde(default)]
    pub in_format: IoInFormat,
    #[serde(default)]
    pub out_content: IoOutContent,
    #[serde(default)]
    pub out_cursor: IoOutCursor,
    #[serde(default = "default_history_context_messages")]
    pub history_context_messages: u32,
}
impl Default for IoConfig {
    fn default() -> Self {
        Self {
            in_format: IoInFormat::default(),
            out_content: IoOutContent::default(),
            out_cursor: IoOutCursor::default(),
            history_context_messages: default_history_context_messages(),
        }
    }
}
fn default_history_context_messages() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct LoopbackConfig {
    #[serde(default)]
    pub io: IoConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceConfig {
    pub name: String,
}
impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            name: "onlyne".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AdapterConfigs {
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub feishu: FeishuConfig,
    #[serde(default)]
    pub qqbot: QqBotConfig,
    #[serde(default, alias = "weixin")]
    pub wechat: WechatConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
    #[serde(default, deserialize_with = "empty_string_none")]
    pub bind_conversation_id: Option<String>,
    #[serde(default)]
    pub io: Option<IoConfig>,
    #[serde(default)]
    pub proxy: Option<String>,
}
impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: telegram_token(),
            token_env: None,
            bind_conversation_id: None,
            io: None,
            proxy: None,
        }
    }
}
fn telegram_token() -> Option<String> {
    Some("$TELEGRAM_BOT_TOKEN".into())
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FeishuConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id_env: Option<String>,
    #[serde(default)]
    pub app_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_secret_env: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default = "default_true")]
    pub rich_text: bool,
    #[serde(default, deserialize_with = "empty_string_none")]
    pub bind_conversation_id: Option<String>,
    #[serde(default)]
    pub io: Option<IoConfig>,
}
impl Default for FeishuConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: feishu_app_id(),
            app_id_env: None,
            app_secret: feishu_app_secret(),
            app_secret_env: None,
            domain: None,
            rich_text: true,
            bind_conversation_id: None,
            io: None,
        }
    }
}
fn feishu_app_id() -> Option<String> {
    Some("$FEISHU_APP_ID".into())
}
fn feishu_app_secret() -> Option<String> {
    Some("$FEISHU_APP_SECRET".into())
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QqBotConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id_env: Option<String>,
    #[serde(default)]
    pub app_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_secret_env: Option<String>,
    #[serde(default)]
    pub sandbox: bool,
    #[serde(default = "default_true")]
    pub rich_text: bool,
    #[serde(default, deserialize_with = "empty_string_none")]
    pub bind_conversation_id: Option<String>,
    #[serde(default)]
    pub io: Option<IoConfig>,
}
impl Default for QqBotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: qqbot_app_id(),
            app_id_env: None,
            app_secret: qqbot_app_secret(),
            app_secret_env: None,
            sandbox: false,
            rich_text: true,
            bind_conversation_id: None,
            io: None,
        }
    }
}
fn qqbot_app_id() -> Option<String> {
    Some("$QQBOT_APP_ID".into())
}
fn qqbot_app_secret() -> Option<String> {
    Some("$QQBOT_APP_SECRET".into())
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WechatConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_weixin_cdn")]
    pub cdn_base_url: String,
    #[serde(default, deserialize_with = "empty_string_none")]
    pub bind_conversation_id: Option<String>,
    #[serde(default)]
    pub io: Option<IoConfig>,
}
impl Default for WechatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: weixin_token(),
            token_env: None,
            base_url: None,
            cdn_base_url: default_weixin_cdn(),
            bind_conversation_id: None,
            io: None,
        }
    }
}
fn weixin_token() -> Option<String> {
    Some("$WEIXIN_ILINK_TOKEN".into())
}
fn default_weixin_cdn() -> String {
    "https://novac2c.cdn.weixin.qq.com/c2c".into()
}

fn empty_string_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.filter(|s| !s.trim().is_empty()))
}

#[derive(Debug, Clone, Default)]
pub struct Env {
    vars: HashMap<String, String>,
}
impl Env {
    pub fn load(workspace_dotenv: &Path, root_dotenv: &Path) -> Self {
        let mut vars = HashMap::new();
        for path in [root_dotenv, workspace_dotenv] {
            read_dotenv(path, &mut vars);
        }
        for (k, v) in std::env::vars() {
            vars.insert(k, v);
        }
        Self { vars }
    }
    pub fn value(&self, raw: &Option<String>) -> Option<String> {
        let value = raw.as_ref()?.trim();
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

    pub fn secret(
        &self,
        value: &Option<String>,
        legacy_env: &Option<String>,
        label: &str,
    ) -> anyhow::Result<String> {
        self.value(value)
            .or_else(|| {
                legacy_env
                    .as_ref()
                    .and_then(|name| self.vars.get(name))
                    .cloned()
            })
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("missing secret {label}; set config value or ${label}"))
    }
}

pub fn load_config(path: &Path) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_parses_default() {
        let cfg = toml::from_str::<Config>(DEFAULT_CONFIG).unwrap();
        assert_eq!(cfg.adapters.wechat.bind_conversation_id, None);
    }

    #[test]
    fn config_parses_bind_conversation_id() {
        let cfg: Config = toml::from_str(
            r#"[adapters.telegram]
token_env = "OLD_TG_TOKEN"
bind_conversation_id = "123"

[adapters.weixin]
bind_conversation_id = "wx"
"#,
        )
        .unwrap();
        assert_eq!(cfg.adapters.telegram.token, None);
        assert_eq!(
            cfg.adapters.telegram.token_env.as_deref(),
            Some("OLD_TG_TOKEN")
        );
        assert_eq!(
            cfg.adapters.telegram.bind_conversation_id.as_deref(),
            Some("123")
        );
        assert_eq!(
            cfg.adapters.wechat.bind_conversation_id.as_deref(),
            Some("wx")
        );
    }

    #[test]
    fn env_resolves_dollar_values() {
        let mut env = Env::default();
        env.vars.insert("CHAT".into(), "chat-1".into());
        assert_eq!(env.value(&Some("$CHAT".into())).as_deref(), Some("chat-1"));
        assert_eq!(
            env.value(&Some("literal".into())).as_deref(),
            Some("literal")
        );
    }
    #[test]
    fn dotenv_reads_values() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(&p, "A=one\nB=\"two\"\n").unwrap();
        let env = Env::load(&p, &dir.path().join("nope"));
        assert_eq!(env.vars.get("A").unwrap(), "one");
        assert_eq!(env.vars.get("B").unwrap(), "two");
    }
}
