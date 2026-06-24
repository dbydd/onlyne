use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

pub const DEFAULT_DOTENV: &str = r#"# Onlyne workspace-local secrets.
# TELEGRAM_BOT_TOKEN=
# FEISHU_APP_ID=
# FEISHU_APP_SECRET=
# QQBOT_APP_ID=
# QQBOT_APP_SECRET=
# WEIXIN_ILINK_TOKEN=
"#;

pub const DEFAULT_CONFIG: &str = r#"[workspace]
name = "onlyne"

[adapters.telegram]
enabled = false
token_env = "TELEGRAM_BOT_TOKEN"
allow_chats = []

[adapters.feishu]
enabled = false
app_id_env = "FEISHU_APP_ID"
app_secret_env = "FEISHU_APP_SECRET"
rich_text = true
allow_chats = []

[adapters.qqbot]
enabled = false
app_id_env = "QQBOT_APP_ID"
app_secret_env = "QQBOT_APP_SECRET"
sandbox = false
rich_text = true
allow_chats = []

[adapters.weixin]
enabled = false
token_env = "WEIXIN_ILINK_TOKEN"
base_url = ""
cdn_base_url = "https://novac2c.cdn.weixin.qq.com/c2c"
allow_chats = []

[rich_text]
max_attachment_bytes = 26214400
"#;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub adapters: AdapterConfigs,
    #[serde(default)]
    pub rich_text: RichTextConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdapterConfigs {
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub feishu: FeishuConfig,
    #[serde(default)]
    pub qqbot: QqBotConfig,
    #[serde(default)]
    pub weixin: WeixinConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default = "telegram_token_env")]
    pub token_env: String,
    #[serde(default)]
    pub allow_chats: Vec<String>,
    #[serde(default)]
    pub proxy: Option<String>,
}
impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: None,
            token_env: telegram_token_env(),
            allow_chats: vec![],
            proxy: None,
        }
    }
}
fn telegram_token_env() -> String {
    "TELEGRAM_BOT_TOKEN".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default = "feishu_app_id_env")]
    pub app_id_env: String,
    #[serde(default)]
    pub app_secret: Option<String>,
    #[serde(default = "feishu_app_secret_env")]
    pub app_secret_env: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default = "default_true")]
    pub rich_text: bool,
    #[serde(default)]
    pub allow_chats: Vec<String>,
}
impl Default for FeishuConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: None,
            app_id_env: feishu_app_id_env(),
            app_secret: None,
            app_secret_env: feishu_app_secret_env(),
            domain: None,
            rich_text: true,
            allow_chats: vec![],
        }
    }
}
fn feishu_app_id_env() -> String {
    "FEISHU_APP_ID".into()
}
fn feishu_app_secret_env() -> String {
    "FEISHU_APP_SECRET".into()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QqBotConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default = "qqbot_app_id_env")]
    pub app_id_env: String,
    #[serde(default)]
    pub app_secret: Option<String>,
    #[serde(default = "qqbot_app_secret_env")]
    pub app_secret_env: String,
    #[serde(default)]
    pub sandbox: bool,
    #[serde(default = "default_true")]
    pub rich_text: bool,
    #[serde(default)]
    pub allow_chats: Vec<String>,
}
impl Default for QqBotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: None,
            app_id_env: qqbot_app_id_env(),
            app_secret: None,
            app_secret_env: qqbot_app_secret_env(),
            sandbox: false,
            rich_text: true,
            allow_chats: vec![],
        }
    }
}
fn qqbot_app_id_env() -> String {
    "QQBOT_APP_ID".into()
}
fn qqbot_app_secret_env() -> String {
    "QQBOT_APP_SECRET".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeixinConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default = "weixin_token_env")]
    pub token_env: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_weixin_cdn")]
    pub cdn_base_url: String,
    #[serde(default)]
    pub allow_chats: Vec<String>,
}
impl Default for WeixinConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: None,
            token_env: weixin_token_env(),
            base_url: None,
            cdn_base_url: default_weixin_cdn(),
            allow_chats: vec![],
        }
    }
}
fn weixin_token_env() -> String {
    "WEIXIN_ILINK_TOKEN".into()
}
fn default_weixin_cdn() -> String {
    "https://novac2c.cdn.weixin.qq.com/c2c".into()
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
    pub fn secret(
        &self,
        env_name: &str,
        inline: &Option<String>,
        label: &str,
    ) -> anyhow::Result<String> {
        self.vars
            .get(env_name)
            .cloned()
            .or_else(|| inline.clone())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("missing secret {label}; set {env_name} or config value"))
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
        toml::from_str::<Config>(DEFAULT_CONFIG).unwrap();
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
