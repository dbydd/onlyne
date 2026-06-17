use crate::{
    adapters::allowed,
    config::{Env, WeixinConfig},
    core::*,
    workspace::Workspace,
};
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use anyhow::{Context, anyhow};
use async_trait::async_trait;
use base64::Engine;
use chrono::{TimeZone, Utc};
use rand::RngCore;
use reqwest::Client;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::Mutex,
    task::JoinHandle,
    time::{Duration, sleep},
};
const DEFAULT_BASE: &str = "https://ilinkai.weixin.qq.com";
const VERSION: &str = "onlyne-weixin/1.0";
pub struct WeixinAdapter {
    token: String,
    base: String,
    cdn_base: String,
    allow_chats: Vec<String>,
    client: Client,
    running: Arc<AtomicBool>,
    buf: Arc<Mutex<String>>,
    tokens: Arc<Mutex<HashMap<String, String>>>,
    state_dir: PathBuf,
    task: Option<JoinHandle<()>>,
}
impl WeixinAdapter {
    pub fn new(cfg: &WeixinConfig, env: &Env, ws: &Workspace) -> anyhow::Result<Self> {
        Ok(Self {
            token: env.secret(&cfg.token_env, &cfg.token, "weixin token")?,
            base: cfg
                .base_url
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_BASE.into())
                .trim_end_matches('/')
                .into(),
            cdn_base: cfg.cdn_base_url.trim_end_matches('/').to_string(),
            allow_chats: cfg.allow_chats.clone(),
            client: Client::builder().timeout(Duration::from_secs(45)).build()?,
            running: Arc::new(AtomicBool::new(false)),
            buf: Arc::new(Mutex::new(String::new())),
            tokens: Arc::new(Mutex::new(HashMap::new())),
            state_dir: ws.adapter_dir().join("weixin"),
            task: None,
        })
    }
    async fn post(&self, endpoint: &str, body: Value) -> anyhow::Result<Value> {
        let v: Value = self
            .client
            .post(format!(
                "{}/{}",
                self.base,
                endpoint.trim_start_matches('/')
            ))
            .bearer_auth(&self.token)
            .header("AuthorizationType", "ilink_bot_token")
            .header("X-WECHAT-UIN", "MDAwMA==")
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v)
    }
}
#[async_trait]
impl Adapter for WeixinAdapter {
    fn channel_id(&self) -> ChannelId {
        ChannelId("weixin".into())
    }
    async fn start(&mut self, ctx: AdapterContext) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(&self.state_dir).await?;
        self.check().await?;
        self.running.store(true, Ordering::SeqCst);
        let base = self.base.clone();
        let token = self.token.clone();
        let allow = self.allow_chats.clone();
        let running = self.running.clone();
        let buf = self.buf.clone();
        let tokens = self.tokens.clone();
        let inbound = ctx.inbound.clone();
        let events = ctx.events.clone();
        let media_dir = ctx.media_dir.clone();
        let cdn_base = self.cdn_base.clone();
        self.task = Some(tokio::spawn(async move {
            let client = Client::builder()
                .timeout(Duration::from_secs(45))
                .build()
                .unwrap();
            while running.load(Ordering::SeqCst) {
                let cur = buf.lock().await.clone();
                let res = client
                    .post(format!("{base}/ilink/bot/getupdates"))
                    .bearer_auth(&token)
                    .header("AuthorizationType", "ilink_bot_token")
                    .header("X-WECHAT-UIN", "MDAwMA==")
                    .json(&json!({"get_updates_buf":cur,"base_info":{"channel_version":VERSION}}))
                    .send()
                    .await;
                match res {
                    Ok(r) => match r.json::<Value>().await {
                        Ok(v) => {
                            if let Some(b) = v.get("get_updates_buf").and_then(Value::as_str) {
                                *buf.lock().await = b.to_string();
                            }
                            if let Some(msgs) = v.get("msgs").and_then(Value::as_array) {
                                for m in msgs {
                                    if let Some(env) = parse_weixin(
                                        m.clone(),
                                        &allow,
                                        &tokens,
                                        &media_dir,
                                        &cdn_base,
                                        &client,
                                    )
                                    .await
                                    {
                                        let _ = inbound.send(env).await;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ = events
                                .send(Event::AdapterFailed {
                                    channel_id: ChannelId("weixin".into()),
                                    error: e.to_string(),
                                })
                                .await;
                        }
                    },
                    Err(e) => {
                        let _ = events
                            .send(Event::AdapterReconnecting {
                                channel_id: ChannelId("weixin".into()),
                                reason: e.to_string(),
                            })
                            .await;
                        sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        }));
        Ok(())
    }
    async fn stop(&mut self) -> anyhow::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn health(&self) -> AdapterHealth {
        if self.running.load(Ordering::SeqCst) {
            AdapterHealth::Ready
        } else {
            AdapterHealth::Stopped
        }
    }
    async fn list_conversations(&self) -> anyhow::Result<Vec<Conversation>> {
        Ok(vec![])
    }
    async fn send_message(&self, msg: OutboundMessage) -> anyhow::Result<MessageEnvelope> {
        let context = self
            .tokens
            .lock()
            .await
            .get(&msg.conversation_id.0)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "weixin context_token missing for {}; receive a message from this peer first",
                    msg.conversation_id.0
                )
            })?;
        let mut items = Vec::new();
        if !msg.text.clone().unwrap_or_default().trim().is_empty() {
            items.push(json!({"type":1,"text_item":{"text":msg.text.clone().unwrap_or_default()}}));
        }
        for attachment in &msg.attachments {
            items.push(self.media_item(&msg.conversation_id.0, attachment).await?);
        }
        if items.is_empty() {
            return Err(anyhow!("weixin send_message needs text or attachments"));
        }
        let body = json!({"msg":{"from_user_id":"","to_user_id":msg.conversation_id.0,"client_id":format!("onlyne-{}",Utc::now().timestamp_nanos_opt().unwrap_or_default()),"message_type":2,"message_state":2,"item_list":items,"context_token":context},"base_info":{"channel_version":VERSION}});
        let v = self.post("ilink/bot/sendmessage", body).await?;
        if v.get("ret").and_then(Value::as_i64).unwrap_or(0) != 0 {
            return Err(anyhow!("weixin send failed: {v}"));
        }
        Ok(MessageEnvelope {
            channel_id: self.channel_id(),
            conversation_id: msg.conversation_id,
            message_id: now_id("weixin"),
            direction: Direction::Outbound,
            sender_id: None,
            sender_name: None,
            text: msg.text,
            attachments: msg.attachments,
            delivery_state: DeliveryState::Sent,
            timestamp: Utc::now(),
            platform_metadata: v,
        })
    }

    async fn check(&self) -> anyhow::Result<()> {
        let _ = self
            .post(
                "ilink/bot/getupdates",
                json!({"get_updates_buf":"","base_info":{"channel_version":VERSION}}),
            )
            .await?;
        Ok(())
    }
}

impl WeixinAdapter {
    async fn media_item(&self, to: &str, a: &AttachmentRef) -> anyhow::Result<Value> {
        let src = a
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .or_else(|| a.url.clone())
            .context("attachment needs path or url")?;
        let bytes = crate::media::read_bytes(&src).await?;
        let media_type = match a.kind {
            AttachmentKind::Image => 1,
            AttachmentKind::Video => 2,
            AttachmentKind::File | AttachmentKind::Audio | AttachmentKind::Voice => 3,
        };
        let up = self.upload_cdn(to, &bytes, media_type).await?;
        let media = json!({"encrypt_query_param": up.download_param, "aes_key": b64_hex_key(&up.aes_key), "encrypt_type": 1});
        Ok(match a.kind {
            AttachmentKind::Image => {
                json!({"type":2,"image_item":{"media":media,"mid_size":up.cipher_size}})
            }
            AttachmentKind::Voice | AttachmentKind::Audio => {
                json!({"type":3,"voice_item":{"media":media,"encode_type":0}})
            }
            AttachmentKind::Video => {
                json!({"type":5,"video_item":{"media":media,"video_size":up.cipher_size}})
            }
            AttachmentKind::File => {
                json!({"type":4,"file_item":{"media":media,"file_name":a.file_name.clone().unwrap_or_else(||"file.bin".into()),"len":up.raw_size.to_string()}})
            }
        })
    }

    async fn upload_cdn(
        &self,
        to: &str,
        bytes: &[u8],
        media_type: i32,
    ) -> anyhow::Result<CdnUpload> {
        let mut key = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut key);
        let filekey = format!(
            "{:x}",
            md5::compute(format!(
                "{}{}",
                Utc::now().timestamp_nanos_opt().unwrap_or_default(),
                to
            ))
        );
        let req = json!({"filekey":filekey,"media_type":media_type,"to_user_id":to,"rawsize":bytes.len(),"rawfilemd5":format!("{:x}", md5::compute(bytes)),"filesize":aes_padded_len(bytes.len()),"no_need_thumb":true,"aeskey":hex_lower(&key),"base_info":{"channel_version":VERSION}});
        let resp = self.post("ilink/bot/getuploadurl", req).await?;
        let upload_url = resp
            .get("upload_full_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                build_cdn_upload_url(
                    &self.cdn_base,
                    resp.get("upload_param")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    &filekey,
                )
            });
        let encrypted = aes_ecb_encrypt(bytes, &key)?;
        let r = self
            .client
            .post(upload_url)
            .header("Content-Type", "application/octet-stream")
            .body(encrypted)
            .send()
            .await?
            .error_for_status()?;
        let download_param = r
            .headers()
            .get("x-encrypted-param")
            .and_then(|h| h.to_str().ok())
            .context("weixin CDN response missing x-encrypted-param")?
            .to_string();
        Ok(CdnUpload {
            download_param,
            aes_key: key,
            cipher_size: aes_padded_len(bytes.len()),
            raw_size: bytes.len(),
        })
    }
}

async fn parse_weixin(
    m: Value,
    allow: &[String],
    tokens: &Arc<Mutex<HashMap<String, String>>>,
    media_dir: &Path,
    cdn_base: &str,
    client: &Client,
) -> Option<MessageEnvelope> {
    let peer = m.get("from_user_id").and_then(Value::as_str)?.to_string();
    if !allowed(allow, &peer) {
        return None;
    }
    if let Some(t) = m.get("context_token").and_then(Value::as_str) {
        tokens.lock().await.insert(peer.clone(), t.to_string());
    }
    let text = m
        .get("item_list")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|it| {
                it.get("text_item")
                    .and_then(|x| x.get("text"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        it.get("voice_item")
                            .and_then(|x| x.get("text"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
            })
        });
    let ts = m
        .get("create_time_ms")
        .and_then(Value::as_i64)
        .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
        .unwrap_or_else(Utc::now);
    let attachments = collect_media(&m, media_dir, cdn_base, client).await;
    Some(MessageEnvelope {
        channel_id: ChannelId("weixin".into()),
        conversation_id: ConversationId(peer),
        message_id: MessageId(
            m.get("message_id")
                .and_then(Value::as_i64)
                .map(|x| x.to_string())
                .unwrap_or_else(|| "weixin-in".into()),
        ),
        direction: Direction::Inbound,
        sender_id: m
            .get("from_user_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        sender_name: None,
        text,
        attachments,
        delivery_state: DeliveryState::Delivered,
        timestamp: ts,
        platform_metadata: m,
    })
}

struct CdnUpload {
    download_param: String,
    aes_key: [u8; 16],
    cipher_size: usize,
    raw_size: usize,
}

async fn collect_media(
    m: &Value,
    media_dir: &Path,
    cdn_base: &str,
    client: &Client,
) -> Vec<AttachmentRef> {
    let mut out = Vec::new();
    let Some(items) = m.get("item_list").and_then(Value::as_array) else {
        return out;
    };
    for (i, it) in items.iter().enumerate() {
        if let Some((kind, enc, key, name)) = media_material(it, i)
            && let Ok(bytes) = download_cdn(cdn_base, client, &enc, &key).await
            && let Ok(path) = crate::media::cache_bytes(media_dir, "weixin", &name, &bytes).await
        {
            out.push(AttachmentRef {
                kind,
                path: Some(path),
                url: None,
                file_name: Some(name),
                mime_type: None,
                size: Some(bytes.len() as u64),
            });
        }
    }
    out
}

fn media_material(it: &Value, i: usize) -> Option<(AttachmentKind, String, String, String)> {
    match it.get("type").and_then(Value::as_i64)? {
        2 => material(
            it.pointer("/image_item/media"),
            AttachmentKind::Image,
            format!("image_{i}.jpg"),
        ),
        3 => material(
            it.pointer("/voice_item/media"),
            AttachmentKind::Voice,
            format!("voice_{i}.silk"),
        ),
        4 => material(
            it.pointer("/file_item/media"),
            AttachmentKind::File,
            it.pointer("/file_item/file_name")
                .and_then(Value::as_str)
                .unwrap_or("file.bin")
                .to_string(),
        ),
        5 => material(
            it.pointer("/video_item/media"),
            AttachmentKind::Video,
            format!("video_{i}.mp4"),
        ),
        _ => None,
    }
}

fn material(
    media: Option<&Value>,
    kind: AttachmentKind,
    name: String,
) -> Option<(AttachmentKind, String, String, String)> {
    let media = media?;
    Some((
        kind,
        media.get("encrypt_query_param")?.as_str()?.to_string(),
        media.get("aes_key")?.as_str()?.to_string(),
        name,
    ))
}

async fn download_cdn(
    cdn_base: &str,
    client: &Client,
    enc: &str,
    key_b64: &str,
) -> anyhow::Result<Vec<u8>> {
    let url = format!(
        "{}/download?encrypted_query_param={}",
        cdn_base.trim_end_matches('/'),
        url::form_urlencoded::byte_serialize(enc.as_bytes()).collect::<String>()
    );
    let encrypted = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    aes_ecb_decrypt(&encrypted, &parse_aes_key(key_b64)?)
}

fn build_cdn_upload_url(base: &str, upload_param: &str, filekey: &str) -> String {
    format!(
        "{}/upload?encrypted_query_param={}&filekey={}",
        base.trim_end_matches('/'),
        url::form_urlencoded::byte_serialize(upload_param.as_bytes()).collect::<String>(),
        filekey
    )
}
fn b64_hex_key(key: &[u8; 16]) -> String {
    base64::engine::general_purpose::STANDARD.encode(hex_lower(key))
}
fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn aes_padded_len(n: usize) -> usize {
    ((n + 16) / 16) * 16
}
fn pkcs7_pad(mut b: Vec<u8>) -> Vec<u8> {
    let n = 16 - (b.len() % 16);
    b.extend(std::iter::repeat_n(n as u8, n));
    b
}
fn pkcs7_unpad(b: &[u8]) -> anyhow::Result<Vec<u8>> {
    let n = *b.last().context("empty pkcs7")? as usize;
    if n == 0 || n > 16 || n > b.len() || !b[b.len() - n..].iter().all(|x| *x as usize == n) {
        return Err(anyhow!("invalid pkcs7 padding"));
    }
    Ok(b[..b.len() - n].to_vec())
}
fn aes_ecb_encrypt(bytes: &[u8], key: &[u8; 16]) -> anyhow::Result<Vec<u8>> {
    let cipher = aes::Aes128::new_from_slice(key)?;
    let mut out = pkcs7_pad(bytes.to_vec());
    for block in out.chunks_exact_mut(16) {
        cipher.encrypt_block(block.into());
    }
    Ok(out)
}
fn aes_ecb_decrypt(bytes: &[u8], key: &[u8; 16]) -> anyhow::Result<Vec<u8>> {
    if !bytes.len().is_multiple_of(16) {
        return Err(anyhow!("ciphertext length not block aligned"));
    }
    let cipher = aes::Aes128::new_from_slice(key)?;
    let mut out = bytes.to_vec();
    for block in out.chunks_exact_mut(16) {
        cipher.decrypt_block(block.into());
    }
    pkcs7_unpad(&out)
}
fn parse_aes_key(s: &str) -> anyhow::Result<[u8; 16]> {
    let decoded = base64::engine::general_purpose::STANDARD.decode(s.trim())?;
    let raw = if decoded.len() == 16 {
        decoded
    } else if decoded.len() == 32 {
        hex_to_bytes(std::str::from_utf8(&decoded)?)?
    } else {
        return Err(anyhow!("bad aes_key length"));
    };
    raw.try_into().map_err(|_| anyhow!("bad aes_key length"))
}
fn hex_to_bytes(s: &str) -> anyhow::Result<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn aes_ecb_roundtrip_and_padding() {
        let key = *b"0123456789abcdef";
        let plain = b"hello weixin media";
        let encrypted = aes_ecb_encrypt(plain, &key).unwrap();
        assert_eq!(encrypted.len() % 16, 0);
        assert_eq!(aes_ecb_decrypt(&encrypted, &key).unwrap(), plain);
    }

    #[test]
    fn aes_key_accepts_raw_and_hex_base64() {
        let key = *b"0123456789abcdef";
        let raw = base64::engine::general_purpose::STANDARD.encode(key);
        let hex = base64::engine::general_purpose::STANDARD.encode(hex_lower(&key));
        assert_eq!(parse_aes_key(&raw).unwrap(), key);
        assert_eq!(parse_aes_key(&hex).unwrap(), key);
    }

    #[test]
    fn rejects_bad_pkcs7() {
        assert!(pkcs7_unpad(b"abc\x02").is_err());
        assert!(aes_ecb_decrypt(b"short", b"0123456789abcdef").is_err());
    }
}
