pub mod feishu;
pub mod qqbot;
pub mod telegram;
pub mod weixin;

use crate::{
    config::{Config, Env},
    core::Adapter,
    workspace::Workspace,
};

pub async fn build_enabled(
    cfg: &Config,
    env: &Env,
    ws: &Workspace,
) -> anyhow::Result<Vec<Box<dyn Adapter>>> {
    let mut out: Vec<Box<dyn Adapter>> = Vec::new();
    if cfg.adapters.telegram.enabled {
        out.push(Box::new(telegram::TelegramAdapter::new(
            &cfg.adapters.telegram,
            env,
        )?));
    }
    if cfg.adapters.feishu.enabled {
        out.push(Box::new(feishu::FeishuAdapter::new(
            &cfg.adapters.feishu,
            env,
        )?));
    }
    if cfg.adapters.qqbot.enabled {
        out.push(Box::new(qqbot::QqBotAdapter::new(
            &cfg.adapters.qqbot,
            env,
        )?));
    }
    if cfg.adapters.wechat.enabled {
        out.push(Box::new(weixin::WeixinAdapter::new(
            &cfg.adapters.wechat,
            env,
            ws,
        )?));
    }
    Ok(out)
}

pub fn bound_matches(bind: &Option<String>, id: &str) -> bool {
    bind.as_deref().is_none_or(|x| x == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binding_allows_handshake_window() {
        assert!(bound_matches(&None, "peer"));
    }

    #[test]
    fn binding_filters() {
        assert!(bound_matches(&Some("peer".into()), "peer"));
        assert!(!bound_matches(&Some("peer".into()), "other"));
    }
}
