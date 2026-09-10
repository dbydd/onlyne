use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::Verifier;
use onlyne_frame::{is_bad_frame, is_too_large, read_frame, write_frame};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use crate::NetError;
use crate::acl::AclTable;
use crate::identity::{KeyPair, challenge_message, decode_signature, parse_public};

pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct Challenge {
    bytes: [u8; 32],
}

impl Challenge {
    pub fn new() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self { bytes }
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl Default for Challenge {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeOk {
    pub role: String,
    pub aggregate: bool,
    pub agent: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloAck {
    pub ok: bool,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub aggregate: bool,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChallengeFrame {
    t: String,
    data: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct HelloFrame {
    role: String,
    key: String,
    signature: String,
    agent: String,
    version: String,
    aggregate: bool,
    protocol: u16,
}

#[derive(Debug, Serialize, Deserialize)]
struct WireError {
    ok: bool,
    code: String,
    message: String,
}

pub async fn accept<S>(
    stream: &mut S,
    table: &AclTable,
    protocol: u16,
) -> Result<HandshakeOk, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    accept_with_timeout(stream, table, protocol, DEFAULT_HANDSHAKE_TIMEOUT).await
}

pub async fn accept_with_timeout<S>(
    stream: &mut S,
    table: &AclTable,
    protocol: u16,
    limit: Duration,
) -> Result<HandshakeOk, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match timeout(limit, accept_inner(stream, table, protocol)).await {
        Ok(result) => result,
        Err(_) => Err(NetError::HandshakeTimeout),
    }
}

async fn accept_inner<S>(
    stream: &mut S,
    table: &AclTable,
    protocol: u16,
) -> Result<HandshakeOk, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let challenge = Challenge::new();
    let challenge_frame = ChallengeFrame {
        t: "challenge".to_string(),
        data: STANDARD.encode(challenge.bytes),
    };
    write_frame(stream, &challenge_frame)
        .await
        .map_err(map_frame_error)?;
    let hello: HelloFrame = match read_frame(stream).await.map_err(map_frame_error)? {
        Some(frame) => frame,
        None => return Err(NetError::Unauthorized("missing hello".to_string())),
    };
    if hello.protocol != protocol {
        return reject(
            stream,
            "protocol_version",
            format!("peer {}, expected {}", hello.protocol, protocol),
            NetError::ProtocolVersion {
                peer: hello.protocol,
                expected: protocol,
            },
        )
        .await;
    }
    let announced_key = match parse_public(&hello.key) {
        Ok(key) => key,
        Err(_) => {
            return reject(
                stream,
                "unauthorized",
                "unregistered key".to_string(),
                NetError::Unauthorized("unregistered key".to_string()),
            )
            .await;
        }
    };
    let role_acl = match table.get(&hello.role) {
        Some(role) => role,
        None => {
            return reject(
                stream,
                "unauthorized",
                "unregistered role".to_string(),
                NetError::Unauthorized("unregistered role".to_string()),
            )
            .await;
        }
    };
    if role_acl.key != announced_key {
        return reject(
            stream,
            "unauthorized",
            "key is not registered for role".to_string(),
            NetError::Unauthorized("key is not registered for role".to_string()),
        )
        .await;
    }
    let signature = match decode_signature(&hello.signature) {
        Ok(signature) => signature,
        Err(_) => {
            return reject(
                stream,
                "unauthorized",
                "invalid signature".to_string(),
                NetError::Unauthorized("invalid signature".to_string()),
            )
            .await;
        }
    };
    if announced_key
        .verify(
            &challenge_message(challenge.bytes(), &hello.role, hello.protocol),
            &signature,
        )
        .is_err()
    {
        return reject(
            stream,
            "unauthorized",
            "invalid challenge signature".to_string(),
            NetError::Unauthorized("invalid challenge signature".to_string()),
        )
        .await;
    }
    let ack = HelloAck {
        ok: true,
        role: hello.role.clone(),
        aggregate: hello.aggregate,
        agent: hello.agent.clone(),
        version: hello.version.clone(),
    };
    write_frame(stream, &ack).await.map_err(map_frame_error)?;
    Ok(HandshakeOk {
        role: hello.role,
        aggregate: hello.aggregate,
        agent: hello.agent,
        version: hello.version,
    })
}

pub async fn offer<S>(
    stream: &mut S,
    role: &str,
    keys: &KeyPair,
    protocol: u16,
    agent: &str,
    version: &str,
    aggregate: bool,
) -> Result<HelloAck, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    offer_with_timeout(
        stream,
        role,
        keys,
        protocol,
        agent,
        version,
        aggregate,
        DEFAULT_HANDSHAKE_TIMEOUT,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
pub async fn offer_with_timeout<S>(
    stream: &mut S,
    role: &str,
    keys: &KeyPair,
    protocol: u16,
    agent: &str,
    version: &str,
    aggregate: bool,
    limit: Duration,
) -> Result<HelloAck, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match timeout(
        limit,
        offer_inner(stream, role, keys, protocol, agent, version, aggregate),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(NetError::HandshakeTimeout),
    }
}

async fn offer_inner<S>(
    stream: &mut S,
    role: &str,
    keys: &KeyPair,
    protocol: u16,
    agent: &str,
    version: &str,
    aggregate: bool,
) -> Result<HelloAck, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let challenge: ChallengeFrame = match read_frame::<_, ChallengeFrame>(stream)
        .await
        .map_err(map_frame_error)?
    {
        Some(frame) if frame.t == "challenge" => frame,
        Some(_) => return Err(NetError::BadFrame),
        None => {
            return Err(NetError::Unauthorized(
                "server closed during handshake".to_string(),
            ));
        }
    };
    let decoded = STANDARD
        .decode(challenge.data)
        .map_err(|_| NetError::BadFrame)?;
    let challenge_bytes: [u8; 32] = decoded.try_into().map_err(|_| NetError::BadFrame)?;
    let hello = HelloFrame {
        role: role.to_string(),
        key: keys.public_str(),
        signature: keys.sign(&challenge_message(&challenge_bytes, role, protocol)),
        agent: agent.to_string(),
        version: version.to_string(),
        aggregate,
        protocol,
    };
    write_frame(stream, &hello).await.map_err(map_frame_error)?;
    let value: serde_json::Value = match read_frame(stream).await.map_err(map_frame_error)? {
        Some(value) => value,
        None => {
            return Err(NetError::Rejected {
                code: "closed".to_string(),
                message: "server closed during handshake".to_string(),
            });
        }
    };
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
        let error: WireError = serde_json::from_value(value).map_err(|_| NetError::BadFrame)?;
        return Err(NetError::Rejected {
            code: error.code,
            message: error.message,
        });
    }
    let ack: HelloAck = serde_json::from_value(value).map_err(|_| NetError::BadFrame)?;
    if !ack.ok {
        return Err(NetError::Rejected {
            code: "rejected".to_string(),
            message: "server rejected handshake".to_string(),
        });
    }
    Ok(ack)
}

async fn reject<S>(
    stream: &mut S,
    code: &str,
    message: String,
    error: NetError,
) -> Result<HandshakeOk, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let frame = WireError {
        ok: false,
        code: code.to_string(),
        message,
    };
    let _ = write_frame(stream, &frame).await;
    let _ = stream.shutdown().await;
    Err(error)
}

fn map_frame_error(error: std::io::Error) -> NetError {
    if is_too_large(&error) {
        NetError::FrameTooLarge
    } else if is_bad_frame(&error) {
        NetError::BadFrame
    } else {
        NetError::Io(error.to_string())
    }
}
