use crate::{config, workspace::Workspace};
use anyhow::{Context, anyhow};
use qrcode::{QrCode, render::unicode};
use reqwest::Client;
use serde::{Deserialize, de::DeserializeOwned};
use std::{path::Path, time::Duration};
use tokio::time::{Instant, sleep};

const FEISHU_ACCOUNTS: &str = "https://accounts.feishu.cn";
const LARK_ACCOUNTS: &str = "https://accounts.larksuite.com";
const FEISHU_OPEN: &str = "https://open.feishu.cn";
const LARK_OPEN: &str = "https://open.larksuite.com";
const WEIXIN_BASE: &str = "https://ilinkai.weixin.qq.com";
const QQBOT_TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

pub struct FeishuAuthOptions {
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub timeout: Duration,
}

pub struct WeixinAuthOptions {
    pub token: Option<String>,
    pub api_url: String,
    pub bot_type: String,
    pub timeout: Duration,
}

pub struct QqBotAuthOptions {
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    pub sandbox: bool,
}

pub async fn auth_feishu(ws: &Workspace, opts: FeishuAuthOptions) -> anyhow::Result<()> {
    ws.bootstrap()?;
    let (app_id, app_secret, domain) = match (opts.app_id, opts.app_secret) {
        (Some(id), Some(secret)) => {
            let domain = validate_feishu(&id, &secret).await?;
            (id, secret, domain)
        }
        (None, None) => {
            let r = feishu_qr(opts.timeout).await?;
            (r.app_id, r.app_secret, r.domain)
        }
        _ => return Err(anyhow!("feishu auth needs both --app-id and --app-secret")),
    };
    save_feishu_auth(ws, &app_id, &app_secret, &domain)?;
    println!("feishu auth saved to {}", ws.dir().display());
    Ok(())
}

pub async fn auth_qqbot(ws: &Workspace, opts: QqBotAuthOptions) -> anyhow::Result<()> {
    ws.bootstrap()?;
    let (Some(app_id), Some(app_secret)) = (opts.app_id, opts.app_secret) else {
        return Err(anyhow!("qqbot auth needs both --app-id and --app-secret"));
    };
    validate_qqbot(&app_id, &app_secret).await?;
    save_qqbot_auth(ws, &app_id, &app_secret, opts.sandbox)?;
    println!("qqbot auth saved to {}", ws.dir().display());
    Ok(())
}

pub async fn auth_weixin(ws: &Workspace, opts: WeixinAuthOptions) -> anyhow::Result<()> {
    ws.bootstrap()?;
    let base = trim_base(&opts.api_url);
    let (token, actual_base) = if let Some(token) = opts.token {
        verify_weixin_token(&base, &token).await?;
        (token, base)
    } else {
        let r = weixin_qr(&base, &opts.bot_type, opts.timeout).await?;
        (r.token, r.base_url.unwrap_or(base))
    };
    save_weixin_auth(ws, &token, Some(&actual_base))?;
    println!("wechat auth saved to {}", ws.dir().display());
    Ok(())
}

pub fn save_feishu_auth(
    ws: &Workspace,
    app_id: &str,
    app_secret: &str,
    domain: &str,
) -> anyhow::Result<()> {
    let mut cfg = config::load_config(&ws.config_path())?;
    cfg.adapters.feishu.enabled = true;
    cfg.adapters.feishu.domain = Some(domain.to_string());
    cfg.adapters.feishu.app_id = Some("$FEISHU_APP_ID".into());
    cfg.adapters.feishu.app_secret = Some("$FEISHU_APP_SECRET".into());
    std::fs::write(ws.config_path(), toml::to_string_pretty(&cfg)?)?;
    set_dotenv(&ws.dotenv_path(), "FEISHU_APP_ID", app_id)?;
    set_dotenv(&ws.dotenv_path(), "FEISHU_APP_SECRET", app_secret)?;
    Ok(())
}

pub fn save_qqbot_auth(
    ws: &Workspace,
    app_id: &str,
    app_secret: &str,
    sandbox: bool,
) -> anyhow::Result<()> {
    let mut cfg = config::load_config(&ws.config_path())?;
    cfg.adapters.qqbot.enabled = true;
    cfg.adapters.qqbot.sandbox = sandbox;
    cfg.adapters.qqbot.app_id = Some("$QQBOT_APP_ID".into());
    cfg.adapters.qqbot.app_secret = Some("$QQBOT_APP_SECRET".into());
    std::fs::write(ws.config_path(), toml::to_string_pretty(&cfg)?)?;
    set_dotenv(&ws.dotenv_path(), "QQBOT_APP_ID", app_id)?;
    set_dotenv(&ws.dotenv_path(), "QQBOT_APP_SECRET", app_secret)?;
    Ok(())
}

pub fn save_weixin_auth(ws: &Workspace, token: &str, base_url: Option<&str>) -> anyhow::Result<()> {
    let mut cfg = config::load_config(&ws.config_path())?;
    cfg.adapters.wechat.enabled = true;
    if let Some(base) = base_url.filter(|s| !s.trim().is_empty()) {
        cfg.adapters.wechat.base_url = Some(trim_base(base));
    }
    cfg.adapters.wechat.token = Some("$WEIXIN_ILINK_TOKEN".into());
    std::fs::write(ws.config_path(), toml::to_string_pretty(&cfg)?)?;
    set_dotenv(&ws.dotenv_path(), "WEIXIN_ILINK_TOKEN", token)?;
    Ok(())
}

async fn validate_qqbot(app_id: &str, app_secret: &str) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Resp {
        access_token: Option<String>,
    }
    let r: Resp = Client::new()
        .post(QQBOT_TOKEN_URL)
        .json(&serde_json::json!({"appId":app_id,"clientSecret":app_secret}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if r.access_token.is_some() {
        Ok(())
    } else {
        Err(anyhow!("qqbot access_token missing"))
    }
}

async fn validate_feishu(app_id: &str, app_secret: &str) -> anyhow::Result<String> {
    let mut last = None;
    for (domain, open) in [(FEISHU_OPEN, FEISHU_OPEN), (LARK_OPEN, LARK_OPEN)] {
        match feishu_token(open, app_id, app_secret).await {
            Ok(()) => return Ok(domain.to_string()),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("feishu credential validation failed")))
}

async fn feishu_token(base: &str, app_id: &str, app_secret: &str) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Resp {
        code: i64,
        msg: Option<String>,
        tenant_access_token: Option<String>,
    }
    let r: Resp = Client::new()
        .post(format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            base
        ))
        .json(&serde_json::json!({"app_id":app_id,"app_secret":app_secret}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if r.code == 0 && r.tenant_access_token.is_some() {
        Ok(())
    } else {
        Err(anyhow!(
            "feishu token failed: code={} msg={}",
            r.code,
            r.msg.unwrap_or_default()
        ))
    }
}

struct FeishuQrResult {
    app_id: String,
    app_secret: String,
    domain: String,
}

async fn feishu_qr(timeout: Duration) -> anyhow::Result<FeishuQrResult> {
    #[derive(Deserialize)]
    struct Init {
        #[serde(default)]
        supported_auth_methods: Vec<String>,
        #[serde(default)]
        error: String,
        #[serde(default)]
        error_description: String,
    }
    #[derive(Deserialize)]
    struct Begin {
        device_code: String,
        verification_uri_complete: String,
        #[serde(default)]
        interval: u64,
        #[serde(default)]
        expire_in: u64,
        #[serde(default)]
        error: String,
        #[serde(default)]
        error_description: String,
    }
    #[derive(Deserialize, Default)]
    struct UserInfo {
        #[serde(default)]
        tenant_brand: String,
    }
    #[derive(Deserialize)]
    struct Poll {
        #[serde(default)]
        client_id: String,
        #[serde(default)]
        client_secret: String,
        #[serde(default)]
        user_info: UserInfo,
        #[serde(default)]
        error: String,
        #[serde(default)]
        error_description: String,
    }

    let client = Client::builder().timeout(Duration::from_secs(15)).build()?;
    let mut base = FEISHU_ACCOUNTS.to_string();
    let init: Init = feishu_registration(&client, &base, "init", &[]).await?;
    if !init.error.is_empty() {
        return Err(anyhow!("{}: {}", init.error, init.error_description));
    }
    if !init.supported_auth_methods.is_empty()
        && !init
            .supported_auth_methods
            .iter()
            .any(|x| x == "client_secret")
    {
        return Err(anyhow!(
            "feishu onboarding does not support client_secret auth"
        ));
    }
    let begin: Begin = feishu_registration(
        &client,
        &base,
        "begin",
        &[
            ("archetype", "PersonalAgent"),
            ("auth_method", "client_secret"),
            ("request_user_info", "open_id"),
        ],
    )
    .await?;
    if !begin.error.is_empty() {
        return Err(anyhow!("{}: {}", begin.error, begin.error_description));
    }
    if begin.device_code.is_empty() || begin.verification_uri_complete.is_empty() {
        return Err(anyhow!("incomplete feishu onboarding response"));
    }
    println!(
        "Open Feishu/Lark and scan this QR URL:\n{}\n",
        begin.verification_uri_complete
    );
    print_qr(&begin.verification_uri_complete);

    let mut interval = Duration::from_secs(begin.interval.max(1));
    let expires = Duration::from_secs(begin.expire_in).min(timeout);
    let deadline = Instant::now() + if expires.is_zero() { timeout } else { expires };
    let mut domain = FEISHU_OPEN.to_string();
    while Instant::now() < deadline {
        let poll: Poll = feishu_registration(
            &client,
            &base,
            "poll",
            &[("device_code", &begin.device_code)],
        )
        .await?;
        if poll.user_info.tenant_brand.eq_ignore_ascii_case("lark") {
            base = LARK_ACCOUNTS.to_string();
            domain = LARK_OPEN.to_string();
        }
        if !poll.client_id.is_empty() && !poll.client_secret.is_empty() {
            return Ok(FeishuQrResult {
                app_id: poll.client_id,
                app_secret: poll.client_secret,
                domain,
            });
        }
        match poll.error.as_str() {
            "" | "authorization_pending" => {}
            "slow_down" => interval += Duration::from_secs(5),
            "access_denied" => return Err(anyhow!("feishu authorization denied")),
            "expired_token" => return Err(anyhow!("feishu QR expired")),
            other => return Err(anyhow!("{}: {}", other, poll.error_description)),
        }
        sleep(interval).await;
    }
    Err(anyhow!("timed out waiting for feishu QR auth"))
}

async fn feishu_registration<T: DeserializeOwned>(
    client: &Client,
    base: &str,
    action: &str,
    params: &[(&str, &str)],
) -> anyhow::Result<T> {
    let mut form = vec![("action", action)];
    form.extend_from_slice(params);
    let resp = client
        .post(format!("{base}/oauth/v1/app/registration"))
        .form(&form)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    serde_json::from_str(&text)
        .with_context(|| format!("feishu registration {action} status={status} body={text}"))
}

struct WeixinQrResult {
    token: String,
    base_url: Option<String>,
}

async fn weixin_qr(
    base: &str,
    bot_type: &str,
    timeout: Duration,
) -> anyhow::Result<WeixinQrResult> {
    #[derive(Deserialize)]
    struct Qr {
        qrcode: String,
        qrcode_img_content: String,
    }
    #[derive(Deserialize)]
    struct Status {
        #[serde(default)]
        status: String,
        #[serde(default)]
        bot_token: String,
        #[serde(default)]
        baseurl: String,
    }
    let client = Client::builder().timeout(Duration::from_secs(40)).build()?;
    let qr: Qr = client
        .get(format!("{base}/ilink/bot/get_bot_qrcode"))
        .query(&[("bot_type", bot_type)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if qr.qrcode.is_empty() || qr.qrcode_img_content.is_empty() {
        return Err(anyhow!("wechat QR response missing qrcode"));
    }
    println!(
        "Open WeChat and scan this QR URL:\n{}\n",
        qr.qrcode_img_content
    );
    print_qr(&qr.qrcode_img_content);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let status: Status = client
            .get(format!("{base}/ilink/bot/get_qrcode_status"))
            .query(&[("qrcode", qr.qrcode.as_str())])
            .header("iLink-App-ClientVersion", "1")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match status.status.as_str() {
            "confirmed" if !status.bot_token.is_empty() => {
                return Ok(WeixinQrResult {
                    token: status.bot_token,
                    base_url: (!status.baseurl.is_empty()).then_some(status.baseurl),
                });
            }
            "expired" => return Err(anyhow!("wechat QR expired; rerun auth")),
            "scaned" => println!("scanned; confirm on phone..."),
            _ => {}
        }
        sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("timed out waiting for wechat QR auth"))
}

async fn verify_weixin_token(base: &str, token: &str) -> anyhow::Result<()> {
    let _: serde_json::Value = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?
        .post(format!("{base}/ilink/bot/getupdates"))
        .bearer_auth(token)
        .header("AuthorizationType", "ilink_bot_token")
        .header("X-WECHAT-UIN", "MDAwMA==")
        .json(&serde_json::json!({"get_updates_buf":"","base_info":{"channel_version":"onlyne-auth/1.0"}}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(())
}

fn set_dotenv(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let mut found = false;
    let mut lines = Vec::new();
    for line in old.lines() {
        if line.trim_start().starts_with(&format!("{key}="))
            || line.trim_start().starts_with(&format!("# {key}="))
            || line.trim_start().starts_with(&format!("#{key}="))
        {
            lines.push(format!("{key}={value}"));
            found = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !found {
        lines.push(format!("{key}={value}"));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

fn trim_base(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() {
        WEIXIN_BASE.to_string()
    } else {
        s.to_string()
    }
}

fn print_qr(s: &str) {
    if let Ok(code) = QrCode::new(s.as_bytes()) {
        println!(
            "{}",
            code.render::<unicode::Dense1x2>().quiet_zone(false).build()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_feishu_credentials_workspace_locally() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::resolve(dir.path());
        ws.bootstrap().unwrap();

        save_feishu_auth(&ws, "cli_x", "dummy-secret", "https://open.feishu.cn").unwrap();

        let cfg = std::fs::read_to_string(ws.config_path()).unwrap();
        let env = std::fs::read_to_string(ws.dotenv_path()).unwrap();
        assert!(cfg.contains("[adapters.feishu]"));
        assert!(cfg.contains("enabled = true"));
        assert!(cfg.contains("domain = \"https://open.feishu.cn\""));
        assert!(env.contains("FEISHU_APP_ID=cli_x"));
        assert!(
            env.lines()
                .any(|line| line == format!("{}{}", "FEISHU_APP_SECRET=", "dummy-secret"))
        );
    }

    #[test]
    fn stores_qqbot_credentials_workspace_locally() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::resolve(dir.path());
        ws.bootstrap().unwrap();

        save_qqbot_auth(&ws, "appid", "dummy-secret", true).unwrap();

        let cfg = std::fs::read_to_string(ws.config_path()).unwrap();
        let env = std::fs::read_to_string(ws.dotenv_path()).unwrap();
        assert!(cfg.contains("[adapters.qqbot]"));
        assert!(cfg.contains("enabled = true"));
        assert!(cfg.contains("sandbox = true"));
        assert!(env.contains("QQBOT_APP_ID=appid"));
        assert!(
            env.lines()
                .any(|line| line == format!("{}{}", "QQBOT_APP_SECRET=", "dummy-secret"))
        );
    }

    #[test]
    fn stores_weixin_token_workspace_locally() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::resolve(dir.path());
        ws.bootstrap().unwrap();

        save_weixin_auth(&ws, "dummy-token", Some("https://ilink.example")).unwrap();

        let cfg = std::fs::read_to_string(ws.config_path()).unwrap();
        let env = std::fs::read_to_string(ws.dotenv_path()).unwrap();
        assert!(cfg.contains("[adapters.wechat]"));
        assert!(cfg.contains("enabled = true"));
        assert!(cfg.contains("base_url = \"https://ilink.example\""));
        assert!(
            env.lines()
                .any(|line| line == format!("{}{}", "WEIXIN_ILINK_TOKEN=", "dummy-token"))
        );
    }
}
