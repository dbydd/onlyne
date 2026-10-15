//! The gateway's one link: dial, hello, then pull deliveries and hand them to
//! the backend of the conversation each one names.
//!
//! One gateway root is one role in the cluster, so it holds one of these —
//! an ordinary Onlyne client connection, the same `dial`, the same `hello`, the
//! same `pull`/`ack` a role client uses. What it *does* with a delivery is the
//! gateway-specific part: the body's first line names a chat key, the registry
//! resolves that key to a conversation, and the conversation's backend renders
//! the rest into that conversation's platform chat, with any media in the
//! conversation's own workspace (`docs/v2.3-CHAT-PROTOCOL.md` §5.3).
//!
//! A delivery whose key is missing, unknown, or misplaced is refused and
//! settled, never guessed at: a misfiled message lands in the wrong person's
//! chat, which is worse than a refused one (§6).

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
    /// The chat key the delivery named, for the backend and the chat log.
    pub chat_key: String,
    pub payload: ImPayload,
}

/// Dials the gateway role's link and runs it until the process ends.
pub struct RoleLink {
    /// The gateway root: its role name, its server, its registry.
    gateway: crate::config::Gateway,
}

impl RoleLink {
    pub fn new(gateway: crate::config::Gateway) -> Self {
        Self { gateway }
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
        let keypair = KeyPair::load(&self.gateway.key_path())
            .with_context(|| format!("loading {}'s key", self.gateway.role))?;
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
                &self.gateway.server,
                &keypair,
                &self.gateway.cert_pin,
                &self.gateway.role,
                settings.clone(),
            )
            .await
            {
                // The backoff is not reset here: a server that accepts the
                // dial and then refuses every pull would be redialed at the
                // first rung of a fresh ladder forever. `serve` resets it once
                // a pull has actually come back clean.
                Ok(link) => link,
                Err(error) => {
                    if onlyne_net::conn::retry_of(&error).ends_the_run() {
                        anyhow::bail!("dialing the server for role {}: {error}", self.gateway.role);
                    }
                    tracing::warn!(
                        role = %self.gateway.role,
                        error = %error,
                        "dial failed; retrying"
                    );
                    sleep_or_shutdown(backoff.next_jittered(), &mut shutdown).await;
                    continue;
                }
            };
            if let Err(error) = self.hello(&link).await {
                // A refusal the net layer reads as permanent (a key that is
                // not this role's, a protocol two builds disagree on) ends the
                // run rather than spinning; anything else is worth a redial.
                if onlyne_net::conn::retry_of(&error).ends_the_run() {
                    return Err(anyhow::Error::new(error).context(format!(
                        "role {} could not open its link",
                        self.gateway.role
                    )));
                }
                tracing::warn!(
                    role = %self.gateway.role,
                    error = %error,
                    "hello failed; retrying"
                );
                sleep_or_shutdown(backoff.next_jittered(), &mut shutdown).await;
                continue;
            }
            match self
                .serve(&link, &renders, &mut reports, &mut shutdown, &mut backoff)
                .await
            {
                // A clean shutdown ends the run.
                Ok(()) => return Ok(()),
                Err(error) if is_transient(&error) => {
                    tracing::warn!(
                        role = %self.gateway.role,
                        error = %error,
                        "link lost; redialing"
                    );
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
        backoff: &mut onlyne_net::Backoff,
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
                    // A pull that came back is the run working: the next
                    // failure earns a first-rung delay, not the one this link
                    // was already sitting on.
                    backoff.reset();
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
                tracing::warn!(role = %self.gateway.role, ?body.error, "a report was refused")
            }
            Err(error) => {
                tracing::warn!(
                    role = %self.gateway.role,
                    error = %error,
                    "a report was not sent"
                )
            }
        }
    }

    /// Send this role's application hello and classify a refusal the way the
    /// net layer classifies a dial: `unauthorized` and a protocol mismatch mean
    /// a person, and the caller ends the run; anything else may be a redial.
    async fn hello(&self, link: &ClientConn) -> std::result::Result<(), NetError> {
        let keypair = KeyPair::load(&self.gateway.key_path())?;
        let hello = HandshakeArgs {
            protocol: PROTOCOL_VERSION,
            role: self.gateway.role.clone(),
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
            anyhow::bail!("role {} pull refused: {message}", self.gateway.role);
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
        // The body's first line names the conversation; the registry says
        // whether this gateway serves it (`docs/v2.3-CHAT-PROTOCOL.md` §5.3,
        // §6). Every failure here is a refusal that settles the row rather than
        // a crash of the link: one misfiled reply is not worth losing the link
        // for every other conversation.
        let text = delivery.envelope.body.text.clone().unwrap_or_default();
        let (chat_key, rest) = match crate::chatkey::extract(&text) {
            Ok(found) => found,
            Err(_) => {
                let reason = "the chat key is not the body's first line; a reply to a \
                              conversation starts with <chat:KEY> on its own first line"
                    .to_string();
                self.refuse(link, delivery, reason).await?;
                return Ok(());
            }
        };
        if chat_key.is_empty() {
            // No key anywhere: the sender meant this as an ordinary platform
            // delivery, or forgot the key. Either way it names no conversation,
            // and guessing one would post into the wrong chat.
            let reason = "this delivery names no chat key; a reply to a conversation starts \
                          with <chat:KEY> on its own first line"
                .to_string();
            self.refuse(link, delivery, reason).await?;
            return Ok(());
        }
        let Some(entry) = self.gateway.conversation_for_key(&chat_key) else {
            let reason = format!("no conversation in this gateway carries chat key {chat_key}");
            self.refuse(link, delivery, reason).await?;
            return Ok(());
        };
        let entry = &entry;
        // The window is what a reply target resolves against, and what the
        // recorder reads later to decide whether this reply opens a discussion.
        let history = crate::history::History::new(&self.gateway.root);
        let window = crate::chain::window_of(&history, &entry.chat_key);
        // A prefix the window cannot place is a refusal, not a guess: the
        // model that wrote it gets the candidates back and writes a longer one.
        let reply_to = match reply_target(&text, &window, entry) {
            Ok(target) => target,
            Err(reason) => {
                self.refuse(link, delivery, reason).await?;
                return Ok(());
            }
        };
        // A platform with a clock refuses a late passive reply, and it is
        // better refused here with a sentence than by the platform against a
        // row that has already left the queue (QQ: `40034005 回复消息msg_id
        // 已过期`, five minutes from the message answered).
        if let (Some(target), Some(limit)) = (
            reply_to.as_deref(),
            crate::threadlink::passive_window(&entry.gateway),
        ) && let Some(sent) = window
            .messages
            .iter()
            .find(|m| m.id == target)
            .map(|m| m.at.clone())
            && let Ok(sent) = chrono::DateTime::parse_from_rfc3339(&sent)
        {
            let age = chrono::Utc::now().signed_duration_since(sent.with_timezone(&chrono::Utc));
            if let Ok(limit) = chrono::Duration::from_std(limit)
                && age > limit
            {
                let reason = format!(
                    "the message this reply answers is {} minutes old, and {} accepts a \
                     passive reply only within {} minutes; send a new message on the platform \
                     and answer that",
                    age.num_minutes(),
                    entry.gateway,
                    limit.num_minutes()
                );
                self.refuse(link, delivery, reason).await?;
                return Ok(());
            }
        }
        // The handle a platform needs to *display* the quote, when it hands
        // out a second id for that: QQ takes `msg_id` for the passive reply
        // and `message_reference` for the rendered quote, and only the second
        // shows the user what is being answered.
        let reply_reference = reply_to
            .as_deref()
            .and_then(|target| window.messages.iter().find(|m| m.id == target))
            .and_then(|message| message.handle.clone());
        let (accepted, reason) = match payload_from(
            &rest,
            reply_to,
            reply_reference,
            entry,
            &delivery,
        ) {
            Some(payload) => {
                if let Err(error) = crate::chatlog::append_outbound(entry, &payload) {
                    // The log is a convenience, not the delivery: a failed
                    // append must not cost the message.
                    tracing::warn!(role = %self.gateway.role, error = %error, "chat log write failed");
                }
                let job = RenderJob {
                    gateway: entry.gateway.clone(),
                    conversation: entry.conversation.clone(),
                    chat_key: entry.chat_key.clone(),
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
                self.gateway.role,
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
            tracing::warn!(role = %self.gateway.role, "ack refused");
        }
        Ok(())
    }

    /// Settle one delivery as refused: the row is acked with its reason so the
    /// ledger records the refusal, and the fault is reported on this role's
    /// behalf so an operator sees a reply that could not be placed
    /// (`docs/v2.3-CHAT-PROTOCOL.md` §6).
    async fn refuse(&self, link: &ClientConn, delivery: Delivery, reason: String) -> Result<()> {
        tracing::warn!(role = %self.gateway.role, %reason, "a delivery is refused");
        self.report(
            link,
            onlyne_proto::Report::Fault {
                task_id: delivery
                    .envelope
                    .causality
                    .as_ref()
                    .map(|causality| causality.task.clone()),
                session_id: None,
                generation: None,
                seq: None,
                kind: "im_unplaced_delivery".to_string(),
                reason: reason.clone(),
                desired: None,
                observed: None,
            },
        )
        .await;
        link.request(
            Frame::req(
                String::new(),
                ClientOp::Ack(AckArgs {
                    msg_id: delivery.msg_id,
                    op_id: delivery.envelope.op_id,
                    accepted: false,
                    reason: Some(reason),
                }),
            ),
            REQUEST_TIMEOUT,
        )
        .await?;
        Ok(())
    }
}

/// Build the standard payload for one delivered envelope.
///
/// The delivery carries `Body` (text plus an optional image); this maps it onto
/// the platform-independent payload, with `text` being the body after its
/// first-line chat key has been taken off — the platform sees the words, not
/// the key. The image arrives as inline base64, so it is decoded and written
/// into the **conversation's own media directory**, and the attachment names
/// that file. A body with neither text nor image is not renderable and answers
/// `None`.
fn payload_from(
    text: &str,
    reply_to: Option<String>,
    reply_reference: Option<String>,
    entry: &Conversation,
    delivery: &Delivery,
) -> Option<ImPayload> {
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
    if text.trim().is_empty() && attachments.is_empty() {
        return None;
    }
    Some(ImPayload {
        text: text.to_string(),
        format: ImFormat::Plain,
        attachments,
        actions: Vec::new(),
        reply_to,
        reply_reference,
    })
}

/// The platform event this reply answers, resolved against the window.
///
/// A reply may name the message it answers by any **prefix** of its id
/// (`docs/v2.4-REPLY-THREADS.md` §4): `ROBOT1.0_sohz` names the message whose
/// id starts that way, and the gateway resolves it to the full id before the
/// backend sends — a platform that wants the id it gave us will not take a
/// truncation of it.
///
/// Three answers, and only the first sends:
///
/// * `Ok(Some(id))` — the marker named one message, unambiguously, or the body
///   named none and the conversation's last inbound event answers.
/// * `Ok(None)` — nothing to answer: no marker and no inbound history.
/// * `Err(reason)` — the marker named something the window cannot resolve.
///   Ambiguity and absence are refused with the candidates, never guessed at,
///   because the reader of the refusal is a model that can fix its prefix.
fn reply_target(
    full_text: &str,
    window: &crate::thread::List,
    entry: &Conversation,
) -> Result<Option<String>, String> {
    let named = match crate::chatkey::extract_with_reply(full_text) {
        Ok((_key, Some(id), _rest)) => Some(id),
        _ => None,
    };
    let Some(prefix) = named else {
        // No marker: the reply answers the message the operator just read.
        return Ok(crate::chatlog::last_inbound_id(entry));
    };
    match window.resolve(&prefix) {
        Ok(found) => Ok(Some(found.id.clone())),
        // A prefix the window cannot place is still a usable id when it is the
        // whole thing: a conversation whose history this cluster never saw
        // (`§8`) has nothing to resolve against, and refusing would make the
        // first reply into an unrecorded thread impossible.
        Err(_unresolved) if prefix.len() >= FULL_ID_LEN => Ok(Some(prefix)),
        Err(error) => Err(error.to_string()),
    }
}

/// The length at which an id is taken whole rather than as a prefix.
///
/// Platform ids are tens of characters; twelve is past what anybody types by
/// hand and short of every real one, so a string this long that the window
/// cannot place is a full id for a message this cluster never recorded.
const FULL_ID_LEN: usize = 12;

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
            slug: "tg-group-1".into(),
            workspace: dir.to_path_buf(),
            config_path: dir.join(".onlyne/config.toml"),
            gateway: "tg1".into(),
            conversation: "-100".into(),
            chat_key: "abc123".into(),
        }
    }

    #[test]
    fn a_delivery_with_text_and_no_image_renders_the_text_alone() {
        let dir = tempfile::tempdir().unwrap();
        let entry = conversation(dir.path());
        let delivery = delivery(Body::text("hello"));
        let payload = payload_from("hello", None, None, &entry, &delivery).unwrap();
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
        assert!(payload_from("", None, None, &entry, &delivery).is_none());
    }

    #[test]
    fn an_inline_image_lands_in_the_conversation_media_directory() {
        let dir = tempfile::tempdir().unwrap();
        let entry = conversation(dir.path());
        // `AAECAw==` is the base64 of [0, 1, 2, 3].
        let delivery = delivery(Body::image("AAECAw==", "image/png"));
        let payload = payload_from("", None, None, &entry, &delivery).unwrap();
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
