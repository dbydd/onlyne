//! One conversation's link: dial, hello, then pull deliveries and hand them to
//! a backend for rendering.
//!
//! A gateway serving N conversations holds N of these, one per conversation
//! directory, and each is an ordinary Onlyne client connection — the same
//! `dial`, the same `hello`, the same `pull`/`ack` a role client uses. Nothing
//! here is gateway-specific except what it *does* with a delivery: render it
//! through the conversation's backend into its platform chat, leaving any media
//! in the conversation's own workspace.
//!
//! `docs/v2.2-IM-GATEWAY-PLAN.md` §2, §5.

use crate::config::Conversation;
use anyhow::{Context, Result};
use onlyne_im_backend::payload::{ImAttachment, ImFormat, ImPayload};
use onlyne_net::conn::{ClientConn, dial};
use onlyne_net::{ConnSettings, KeyPair, NetError};
use onlyne_proto::{
    AckArgs, ClientOp, Delivery, Frame, HandshakeArgs, PROTOCOL_VERSION, PullArgs, PullReply,
};
use std::time::Duration;
use tokio::sync::mpsc;

/// How long one `pull` holds before the server answers with nothing.
const PULL_HOLD_MS: u64 = 20_000;
/// Bound on one request frame.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The agent word this connection reports; the server logs it and reads nothing.
const AGENT: &str = "onlyne-im";

/// One outbound render the backend must perform, tagged with the conversation
/// it belongs to **and** the backend that owns that conversation.
#[derive(Debug, Clone)]
pub struct RenderJob {
    /// The `[[im.backend]]` gateway id whose process renders this.
    pub gateway: String,
    /// The platform conversation the payload goes to.
    pub conversation: String,
    pub payload: ImPayload,
}

/// Dials one conversation's link and runs it until the process ends.
pub struct RoleLink {
    entry: Conversation,
    server: String,
    cert_pin: String,
}

impl RoleLink {
    pub fn new(entry: Conversation, server: String, cert_pin: String) -> Self {
        Self {
            entry,
            server,
            cert_pin,
        }
    }

    /// Connect this role and serve it: pull deliveries, render each through the
    /// backend, ack. A dial or hello that fails retries under backoff, because
    /// the server may simply not be up yet; a failure the net layer calls
    /// permanent ends the run.
    pub async fn run(
        self,
        renders: mpsc::Sender<RenderJob>,
        mut reports: mpsc::Receiver<onlyne_proto::Report>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        let keypair = KeyPair::load(&self.entry.key_path)
            .with_context(|| format!("loading {}'s key", self.entry.role))?;
        let settings = ConnSettings {
            agent: AGENT.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            ..ConnSettings::new(PROTOCOL_VERSION)
        };
        let mut backoff = onlyne_net::Backoff::new();
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let link: ClientConn = match dial(
                &self.server,
                &keypair,
                &self.cert_pin,
                &self.entry.role,
                settings.clone(),
            )
            .await
            {
                Ok(link) => {
                    backoff.reset();
                    link
                }
                Err(error) => {
                    if onlyne_net::conn::retry_of(&error).ends_the_run() {
                        anyhow::bail!("dialing the server for role {}: {error}", self.entry.role);
                    }
                    tracing::warn!(role = %self.entry.role, error = %error, "dial failed; retrying");
                    sleep_or_shutdown(backoff.next_jittered(), &mut shutdown).await;
                    continue;
                }
            };
            if let Err(error) = self.hello(&link).await {
                // A refusal the net layer reads as permanent (a key that is
                // not this role's, a protocol two builds disagree on) ends the
                // run rather than spinning; anything else is worth a redial.
                if onlyne_net::conn::retry_of(&error).ends_the_run() {
                    return Err(anyhow::Error::new(error)
                        .context(format!("role {} could not open its link", self.entry.role)));
                }
                tracing::warn!(role = %self.entry.role, error = %error, "hello failed; retrying");
                sleep_or_shutdown(backoff.next_jittered(), &mut shutdown).await;
                continue;
            }
            match self
                .serve(&link, &renders, &mut reports, &mut shutdown)
                .await
            {
                // A clean shutdown ends the run.
                Ok(()) => return Ok(()),
                Err(error) if is_transient(&error) => {
                    tracing::warn!(role = %self.entry.role, error = %error, "link lost; redialing");
                    sleep_or_shutdown(backoff.next_jittered(), &mut shutdown).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Pull and render until the link fails or a shutdown is asked for.
    async fn serve(
        &self,
        link: &ClientConn,
        renders: &mpsc::Sender<RenderJob>,
        reports: &mut mpsc::Receiver<onlyne_proto::Report>,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        // Once every sender is gone, nothing can report again and the branch
        // parks instead of spinning on a closed channel.
        let mut reports_open = true;
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        let _ = link.close().await;
                        return Ok(());
                    }
                }
                // A fault the gateway wants recorded on this role's behalf,
                // carried on the link that speaks for the role.
                report = reports.recv(), if reports_open => {
                    match report {
                        Some(report) => self.report(link, report).await,
                        None => reports_open = false,
                    }
                }
                outcome = self.pull_and_render(link, renders) => {
                    outcome?;
                }
            }
        }
    }

    /// Send one report on this role's link. A refusal is the server's to
    /// explain and a lost link is the redial's to recover, so neither fails
    /// the run.
    async fn report(&self, link: &ClientConn, report: onlyne_proto::Report) {
        match link
            .request(
                Frame::req(String::new(), ClientOp::Report(report)),
                REQUEST_TIMEOUT,
            )
            .await
        {
            Ok(body) if body.ok => {}
            Ok(body) => {
                tracing::warn!(role = %self.entry.role, ?body.error, "a report was refused")
            }
            Err(error) => {
                tracing::warn!(role = %self.entry.role, error = %error, "a report was not sent")
            }
        }
    }

    /// Send this role's application hello and classify a refusal the way the
    /// net layer classifies a dial: `unauthorized` and a protocol mismatch mean
    /// a person, and the caller ends the run; anything else may be a redial.
    async fn hello(&self, link: &ClientConn) -> std::result::Result<(), NetError> {
        let keypair = KeyPair::load(&self.entry.key_path)?;
        let hello = HandshakeArgs {
            protocol: PROTOCOL_VERSION,
            role: self.entry.role.clone(),
            key: keypair.public_str(),
            signature: String::new(),
            agent: AGENT.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            aggregate: false,
            live_sessions: Vec::new(),
        };
        let body = link
            .request(
                Frame::req(String::new(), ClientOp::Hello(hello)),
                REQUEST_TIMEOUT,
            )
            .await?;
        if !body.ok {
            return Err(match body.error {
                Some(error) => NetError::Rejected {
                    code: error.code.to_string(),
                    message: error.message,
                },
                None => NetError::Rejected {
                    code: "rejected".to_string(),
                    message: "the server refused the hello".to_string(),
                },
            });
        }
        Ok(())
    }

    /// One pull; render and ack whatever it named.
    async fn pull_and_render(
        &self,
        link: &ClientConn,
        renders: &mpsc::Sender<RenderJob>,
    ) -> Result<()> {
        let body = link
            .request(
                Frame::req(
                    String::new(),
                    ClientOp::Pull(PullArgs {
                        role: None,
                        limit: 8,
                        hold_ms: Some(PULL_HOLD_MS),
                        control_only: None,
                    }),
                ),
                // The pull may hold for the whole window; the request bound sits
                // above it so the hold is not what times out.
                Duration::from_millis(PULL_HOLD_MS) + REQUEST_TIMEOUT + Duration::from_secs(5),
            )
            .await?;
        if !body.ok {
            let message = body
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "pull refused".to_string());
            anyhow::bail!("role {} pull refused: {message}", self.entry.role);
        }
        let reply: PullReply = serde_json::from_value(body.data().cloned().unwrap_or_default())
            .context("reading the pull reply")?;
        for delivery in reply.deliveries {
            self.render(link, renders, delivery).await?;
        }
        Ok(())
    }

    /// Turn one delivery into a render job, then settle the row.
    async fn render(
        &self,
        link: &ClientConn,
        renders: &mpsc::Sender<RenderJob>,
        delivery: Delivery,
    ) -> Result<()> {
        let (accepted, reason) = match payload_from(&delivery, &self.entry) {
            Some(payload) => {
                if let Err(error) = crate::chatlog::append_outbound(&self.entry, &payload) {
                    // The log is a convenience, not the delivery: a failed
                    // append must not cost the message.
                    tracing::warn!(role = %self.entry.role, error = %error, "chat log write failed");
                }
                let job = RenderJob {
                    gateway: self.entry.gateway.clone(),
                    conversation: self.entry.conversation.clone(),
                    payload,
                };
                match renders.send(job).await {
                    Ok(()) => (true, None),
                    // The backend is gone: hold the row, do not ack, so it stays
                    // and the next session renders it.
                    Err(_) => (false, Some("the backend is not running".to_string())),
                }
            }
            None => (
                false,
                Some("a delivery this gateway cannot render".to_string()),
            ),
        };
        if !accepted {
            anyhow::bail!(
                "role {}: {}",
                self.entry.role,
                reason.as_deref().unwrap_or("render failed")
            );
        }
        let body = link
            .request(
                Frame::req(
                    String::new(),
                    ClientOp::Ack(AckArgs {
                        msg_id: delivery.msg_id,
                        op_id: delivery.envelope.op_id.clone(),
                        accepted,
                        reason,
                    }),
                ),
                REQUEST_TIMEOUT,
            )
            .await?;
        if !body.ok && body.error.is_some() {
            // A refused ack is not fatal to the link; the row is retried.
            tracing::warn!(role = %self.entry.role, "ack refused");
        }
        Ok(())
    }
}

/// Build the standard payload for one delivered envelope.
///
/// The delivery carries `Body` (text plus an optional image); this maps it onto
/// the platform-independent payload. The image arrives as inline base64, so it
/// is decoded and written into the **conversation's own media directory**, and
/// the attachment names that file. A body with neither text nor image is not
/// renderable and answers `None`.
fn payload_from(delivery: &Delivery, entry: &Conversation) -> Option<ImPayload> {
    let body = &delivery.envelope.body;
    let mut attachments = Vec::new();
    if let Some(image) = &body.image {
        // A note carries no `op_id`; one delivery without a key still names its
        // file by the envelope id, which is unique per envelope.
        let stem = delivery
            .envelope
            .op_id
            .clone()
            .unwrap_or_else(|| delivery.envelope.id.clone());
        match write_image(image, entry, &stem) {
            Ok(attachment) => attachments.push(attachment),
            Err(error) => {
                tracing::warn!(error = %error, "an inbound image was not written out");
            }
        }
    }
    let text = body.text.clone().unwrap_or_default();
    if text.trim().is_empty() && attachments.is_empty() {
        return None;
    }
    Some(ImPayload {
        text,
        format: ImFormat::Plain,
        attachments,
        actions: Vec::new(),
        reply_to: None,
    })
}

/// Decode one inline image and write it into the conversation's media directory
/// for the backend to read.
///
/// The name comes from the delivery's idempotency key, so a re-delivered row
/// (which keeps its `op_id`) overwrites the same file rather than accumulating
/// copies.
fn write_image(
    image: &onlyne_proto::ImagePart,
    entry: &Conversation,
    stem: &str,
) -> Result<ImAttachment> {
    let bytes = image
        .decode()
        .map_err(|error| anyhow::anyhow!("{}", error.message()))?;
    let dir = entry.media_dir();
    std::fs::create_dir_all(&dir)?;
    let ext = extension_for(&image.mime);
    let name = format!("out-{}.{ext}", sanitize_stem(stem));
    let path = dir.join(name);
    std::fs::write(&path, &bytes)?;
    Ok(ImAttachment {
        kind: "image".to_string(),
        path: path.to_string_lossy().into_owned(),
        alt: image.name.clone(),
    })
}

fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    }
}

/// Keep an id to its base component with no separators, so it can name a file
/// inside the media directory.
fn sanitize_stem(id: &str) -> String {
    let stem: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if stem.is_empty() {
        "image".to_string()
    } else {
        stem
    }
}

/// Whether a link failure is worth redialing rather than ending the run.
fn is_transient(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<NetError>()
        .map(|net| !onlyne_net::conn::retry_of(net).ends_the_run())
        .unwrap_or(true)
}

/// Sleep a backoff interval, returning early when a shutdown is asked for.
async fn sleep_or_shutdown(delay: Duration, shutdown: &mut tokio::sync::watch::Receiver<bool>) {
    tokio::select! {
        _ = tokio::time::sleep(delay) => {}
        _ = shutdown.changed() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_proto::{Body, Envelope, MsgKind, Principal, new_envelope};
    use std::path::{Path, PathBuf};

    fn conversation(dir: &Path) -> Conversation {
        Conversation {
            role: "tg-group-1".into(),
            workspace: dir.to_path_buf(),
            config_path: dir.join(".onlyne/config.toml"),
            key_path: dir.join(".onlyne/keys/role.key"),
            gateway: "tg1".into(),
            conversation: "-100".into(),
        }
    }

    #[test]
    fn a_delivery_with_text_and_no_image_renders_the_text_alone() {
        let dir = tempfile::tempdir().unwrap();
        let entry = conversation(dir.path());
        let delivery = delivery(Body::text("hello"));
        let payload = payload_from(&delivery, &entry).unwrap();
        assert_eq!(payload.text, "hello");
        assert!(payload.attachments.is_empty());
    }

    #[test]
    fn an_empty_body_is_not_renderable() {
        let dir = tempfile::tempdir().unwrap();
        let entry = conversation(dir.path());
        // Built through the constructor and then blanked: a body with neither
        // text nor image is the shape a wire validator refuses, and this path
        // still has to answer `None` rather than panic.
        let mut delivery = delivery(Body::text("placeholder"));
        delivery.envelope.body = Body::default();
        assert!(payload_from(&delivery, &entry).is_none());
    }

    #[test]
    fn an_inline_image_lands_in_the_conversation_media_directory() {
        let dir = tempfile::tempdir().unwrap();
        let entry = conversation(dir.path());
        // `AAECAw==` is the base64 of [0, 1, 2, 3].
        let delivery = delivery(Body::image("AAECAw==", "image/png"));
        let payload = payload_from(&delivery, &entry).unwrap();
        assert_eq!(payload.attachments.len(), 1);
        let path = PathBuf::from(&payload.attachments[0].path);
        assert!(
            path.starts_with(entry.media_dir()),
            "{} is outside {}",
            path.display(),
            entry.media_dir().display()
        );
        assert!(path.is_file());
        assert_eq!(std::fs::read(&path).unwrap(), vec![0, 1, 2, 3]);
    }

    /// A delivery over a note, which needs no causality and no `op_id`, so the
    /// test body is the only thing under test.
    fn delivery(body: Body) -> Delivery {
        let envelope: Envelope = new_envelope(
            MsgKind::Note,
            Principal::role("sender"),
            Principal::role("tg-group-1"),
            body,
            None,
        )
        .expect("a test envelope");
        Delivery {
            msg_id: "m1".into(),
            envelope: Box::new(envelope),
        }
    }
}
