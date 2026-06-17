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
    if cfg.adapters.weixin.enabled {
        out.push(Box::new(weixin::WeixinAdapter::new(
            &cfg.adapters.weixin,
            env,
            ws,
        )?));
    }
    Ok(out)
}

pub fn allowed(allow: &[String], id: &str) -> bool {
    allow.is_empty() || allow.iter().any(|x| x == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allow_list_allows_local_smoke() {
        assert!(allowed(&[], "peer"));
    }

    #[test]
    fn non_empty_allow_list_filters() {
        assert!(allowed(&["peer".into()], "peer"));
        assert!(!allowed(&["peer".into()], "other"));
    }
}
