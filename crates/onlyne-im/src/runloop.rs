//! The gateway runloop: spawn one backend per `[[im.backend]]`, dial one link
//! per conversation, dispatch events into trigger scripts, and render
//! deliveries through the backend that owns each conversation.
//!
//! Three shapes meet here (`docs/v2.2-IM-GATEWAY-PLAN.md` §2, §6):
//!
//! - **A backend process per gateway id.** A bot connection serves every
//!   conversation it is in, so the process is the unit of platform state, not of
//!   conversation. Each runs in its own task, reading its stdio and taking its
//!   send jobs off a channel.
//! - **A role link per conversation.** Ordinary Onlyne connections; their only
//!   gateway-specific work is turning a delivery into a render.
//! - **A push script per event, off the loop.** A script is the operator's and
//!   may spend its whole timeout, so an outbound render never waits on one.

use crate::backend_host::BackendHost;
use crate::chatlog;
use crate::config::{Conversation, Gateway, PushMode};
use crate::link::{RenderJob, RoleLink};
use crate::trigger::{
    PUSH_FAILED_KIND, PushEnv, PushFailure, PushInput, PushRetry, run_script_retrying,
};
use anyhow::{Context, Result};
use chrono::Utc;
use onlyne_im_backend::frame::{BackendOp, SendArgs, SentArgs};
use onlyne_proto::Report;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

/// What one backend task takes from the loop: a payload to render, or the
/// instruction to stop and take its process with it.
enum BackendCommand {
    Send(RenderJob),
    Stop,
}

/// Record one outbound reply now that the platform has named it.
///
/// The inbound half's rule, applied to the outbound side
/// (`crate::chain::record_inbound`): the row goes into the role's aggregate
/// always, and into a discussion file of its own when this reply is the one
/// that opened the branch. What the send adds is the two ids a platform hands
/// back — the message's own id, and on a platform with a second one the handle
/// a later reply quotes it by. Without them the chain of a gateway's own
/// messages has no links, and no branch can ever form.
fn record_outbound(gateway: &Gateway, job: &RenderJob, sent: &SentArgs) {
    let history = crate::history::History::new(&gateway.root);
    let window = crate::chain::window_of(&history, &job.chat_key);
    let mut row = crate::history::HistoryEntry::outbound(
        &job.chat_key,
        &job.gateway,
        job.payload.reply_to.as_deref(),
    );
    row.conversation = Some(job.conversation.clone());
    row.text = Some(job.payload.text.clone());
    row.platform_message = sent.message_id.clone();
    row.ref_handle = sent.ref_handle.clone();
    if crate::chain::record_reply(&history, &mut row, &window, job.payload.reply_to.as_deref()) {
        tracing::info!(
            role = %gateway.role,
            chat_key = %job.chat_key,
            list = row.list.as_deref().unwrap_or_default(),
            answered = job.payload.reply_to.as_deref().unwrap_or_default(),
            "a reply opened a discussion"
        );
    }
}

/// One event, tagged with the backend that reported it.
struct BackendEvent {
    gateway: String,
    op: BackendOp,
}

/// Everything one gateway run needs that is not in a config file.
pub struct RunOptions {
    /// The served socket path, exported to scripts as `ONLYNE_SOCKET`.
    pub socket: Option<String>,
}

/// Run the gateway until a permanent failure or a shutdown signal.
pub async fn run(gateway: Gateway, options: RunOptions) -> Result<()> {
    let gateway = Arc::new(gateway);
    if gateway.conversations().is_empty() {
        tracing::warn!(
            root = %gateway.root.display(),
            "this gateway root has no conversation workspace; run `onlyne-im generate` to add one"
        );
    }
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Each backend announces its platform in its `hello`. A heartbeat names
    // that platform for the conversations it beats over, and the only place it
    // is written down is the backend's own announcement, so the hello records it
    // here and the heartbeat tasks read it back.
    let platforms: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new(BTreeMap::new()));

    // One task per backend: its own command channel in, one shared event stream
    // out. A backend that dies ends its own task and is reported; the others and
    // the links keep running, because a lost bot is an operator's to fix.
    let (events_tx, mut events_rx) = mpsc::channel::<BackendEvent>(64);
    let mut backends: JoinSet<(String, Result<()>)> = JoinSet::new();
    let mut senders: BTreeMap<String, mpsc::Sender<BackendCommand>> = BTreeMap::new();
    for entry in &gateway.backends {
        let credential = onlyne_im_backend::credential_path(&gateway.root, &entry.gateway);
        let host = BackendHost::spawn(&entry.run, &entry.gateway, &credential)
            .await
            .with_context(|| format!("spawning the backend for gateway {}", entry.gateway))?;
        let (command_tx, command_rx) = mpsc::channel::<BackendCommand>(64);
        senders.insert(entry.gateway.clone(), command_tx);
        let events = events_tx.clone();
        let platforms = Arc::clone(&platforms);
        let gateway_id = entry.gateway.clone();
        let backend_gateway = Arc::clone(&gateway);
        let mut shutdown = shutdown_rx.clone();
        backends.spawn(async move {
            (
                gateway_id.clone(),
                serve_backend(
                    host,
                    gateway_id.clone(),
                    backend_gateway,
                    command_rx,
                    events,
                    platforms,
                    &mut shutdown,
                )
                .await,
            )
        });
    }
    // Keep one sender so the event channel never closes while links live.
    drop(events_tx);

    // One link for the whole gateway: it speaks for the one role every
    // conversation's traffic rides (`docs/v2.3-CHAT-PROTOCOL.md` §3). The set
    // is held so a link that ends for good is seen: a role the gateway can no
    // longer speak for is a fault, not a silence.
    let (renders_tx, mut renders_rx) = mpsc::channel::<RenderJob>(64);
    let mut links: JoinSet<(String, Result<()>)> = JoinSet::new();
    let mut reporters: BTreeMap<String, mpsc::Sender<Report>> = BTreeMap::new();
    let link = RoleLink::new(gateway.as_ref().clone());
    let rx = shutdown_rx.clone();
    let tx = renders_tx.clone();
    let (report_tx, report_rx) = mpsc::channel::<Report>(8);
    reporters.insert(gateway.role.clone(), report_tx);
    let role = gateway.role.clone();
    links.spawn(async move { (role, link.run(tx, report_rx, rx).await) });
    drop(renders_tx);

    // Heartbeat scripts, one task each.
    for script in gateway
        .pushes
        .iter()
        .filter(|script| script.mode == PushMode::Heartbeat)
        .cloned()
        .collect::<Vec<_>>()
    {
        let gateway = Arc::clone(&gateway);
        let socket = options.socket.clone();
        let reporters = reporters.clone();
        let platforms = Arc::clone(&platforms);
        let every = script.every.expect("a heartbeat carries an every");
        let mut shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            // First beat waits out one interval, so a restart is not a storm.
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(every) => {}
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            return;
                        }
                    }
                }
                for conversation in gateway.conversations() {
                    let platform = platforms
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get(&conversation.gateway)
                        .cloned()
                        .unwrap_or_else(|| conversation.gateway.clone());
                    let input = PushInput::Heartbeat {
                        role: gateway.role.clone(),
                        gateway: conversation.gateway.clone(),
                        platform,
                        chat: conversation.chat_key.clone(),
                        at: Utc::now().to_rfc3339(),
                    };
                    run_one_push(
                        &script,
                        &input,
                        &conversation,
                        socket.clone(),
                        reporters.get(&gateway.role),
                    )
                    .await;
                }
            }
        });
    }

    loop {
        tokio::select! {
            // A backend frame: run the trigger scripts for an event, route a
            // health report to the log.
            event = events_rx.recv() => {
                let Some(event) = event else {
                    // Every backend task is gone; the render channel has nothing
                    // to write to and the links have nothing to speak for.
                    tracing::error!("every backend is gone; the gateway stops");
                    break;
                };
                handle_backend_op(&gateway, event, &options, &reporters).await;
            }
            // A render job: hand it to the backend that owns the conversation.
            job = renders_rx.recv() => {
                let Some(job) = job else {
                    // Unreachable while a link holds a sender.
                    break;
                };
                // The row this delivery becomes is written on the backend's
                // own task, once the platform has named the message: only there
                // is the id known, and that id is what a later reply points at
                // (`docs/v2.4-REPLY-THREADS.md` §6). Waiting for it here would
                // stall every other conversation on one platform's round trip.
                let backend = job.gateway.clone();
                match senders.get(&backend) {
                    Some(sender) if sender.send(BackendCommand::Send(job)).await.is_ok() => {}
                    _ => tracing::warn!(
                        gateway = %backend,
                        "no backend is running to render this delivery; it stays unacked"
                    ),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutdown requested");
                break;
            }
            ended = backends.join_next(), if !backends.is_empty() => {
                match ended {
                    Some(Ok((gateway_id, Ok(())))) => tracing::info!(gateway = %gateway_id, "a backend ended"),
                    Some(Ok((gateway_id, Err(error)))) => tracing::error!(
                        gateway = %gateway_id,
                        error = %format!("{error:#}"),
                        "a backend failed; its conversations' deliveries stay queued"
                    ),
                    Some(Err(error)) => tracing::error!(error = %error, "a backend task panicked"),
                    None => {}
                }
                if backends.is_empty() {
                    tracing::error!("every backend is gone; the gateway stops");
                    break;
                }
            }
            ended = links.join_next(), if !links.is_empty() => {
                match ended {
                    Some(Ok((role, Ok(())))) => tracing::info!(role, "a conversation link ended"),
                    Some(Ok((role, Err(error)))) => tracing::error!(
                        role,
                        error = %format!("{error:#}"),
                        "a conversation link failed; deliveries to it stay queued"
                    ),
                    Some(Err(error)) => tracing::error!(error = %error, "a conversation link task panicked"),
                    None => {}
                }
            }
        }
    }

    let _ = shutdown_tx.send(true);
    // Tell each backend to stop before reaping it, so a backend that owns a
    // platform cursor gets the chance to write it.
    for (gateway_id, sender) in &senders {
        if sender.send(BackendCommand::Stop).await.is_err() {
            tracing::debug!(gateway = %gateway_id, "this backend had already stopped");
        }
    }
    links.shutdown().await;
    backends.shutdown().await;
    Ok(())
}

/// Drive one backend: read its frames, take its send jobs, stop on command or
/// on shutdown.
async fn serve_backend(
    mut host: BackendHost,
    // The `[[im.backend]]` row's id: what an event names to reach the
    // conversation it belongs to, and what the log lines below call the
    // backend. It is *not* the role — the role is one for the whole gateway
    // and carries every backend at once.
    gateway_id: String,
    // The gateway root itself: the record of a sent message is written here.
    gateway: Arc<Gateway>,
    mut commands: mpsc::Receiver<BackendCommand>,
    events: mpsc::Sender<BackendEvent>,
    platforms: Arc<Mutex<BTreeMap<String, String>>>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<()> {
    // The hello is the first frame a backend must send; reading it here means a
    // backend that speaks another protocol is refused at once rather than on its
    // first event.
    let hello = host.hello().await?;
    platforms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(gateway_id.clone(), hello.platform.clone());
    tracing::info!(
        gateway = %gateway_id,
        platform = %hello.platform,
        version = %hello.version,
        "a backend is up"
    );
    // A backend with no credential is a gateway that will never see a message.
    // Saying so at startup is the difference between an operator reading one
    // sentence and reading a silently idle gateway.
    if !hello.authenticated {
        match &hello.onboarding {
            Some(prompt) => tracing::error!(
                gateway = %gateway_id,
                "this backend is not authenticated: {} (`onlyne-im login --gateway {gateway_id}`)",
                prompt.payload
            ),
            None => tracing::error!(
                gateway = %gateway_id,
                "this backend reported no credential and asked for none; \
                 check its environment"
            ),
        }
    }
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(BackendCommand::Send(job)) => {
                    // The send and the id it earns are one round trip, and the
                    // row is written here: a send that failed leaves the id
                    // absent, which the record already handles.
                    let sent = match host.send(job.send_args()).await {
                        Ok(sent) => sent,
                        Err(error) => {
                            tracing::warn!(gateway = %gateway_id, error = %error, "a send failed");
                            SentArgs::default()
                        }
                    };
                    record_outbound(&gateway, &job, &sent);
                }
                Some(BackendCommand::Stop) | None => {
                    let _ = host.stop("gateway shutdown").await;
                    return Ok(());
                }
            },
            op = host.next_op() => {
                let Some(op) = op? else {
                    tracing::warn!(gateway = %gateway_id, "a backend ended its stream");
                    return Ok(());
                };
                if matches!(op, BackendOp::Hello(_)) {
                    tracing::warn!(gateway = %gateway_id, "a backend sent a second hello");
                    continue;
                }
                if events.send(BackendEvent { gateway: gateway_id.clone(), op }).await.is_err() {
                    return Ok(());
                }
            }
            _ = shutdown.changed(), if *shutdown.borrow() => {
                let _ = host.stop("gateway shutdown").await;
                return Ok(());
            }
        }
    }
}

/// Act on one backend frame: run the trigger scripts for an event, log health.
async fn handle_backend_op(
    gateway: &Arc<Gateway>,
    event: BackendEvent,
    options: &RunOptions,
    reporters: &BTreeMap<String, mpsc::Sender<Report>>,
) {
    match event.op {
        BackendOp::Event(inbound) => {
            let conversation = match gateway.conversation_for(&event.gateway, &inbound.conversation)
            {
                Some(known) => known,
                // A conversation nobody registered. The listen policy decides
                // whether this gateway serves it; a refusal is the operator's
                // own choice, so it is a sentence and not a fault.
                None => {
                    if !gateway.listen.admits(&inbound.conversation) {
                        tracing::info!(
                            gateway = %event.gateway,
                            conversation = %inbound.conversation,
                            "the listen policy refuses this conversation"
                        );
                        return;
                    }
                    match crate::adopt::adopt(gateway, &event.gateway, &inbound.conversation) {
                        Ok(adopted) => {
                            // Loud on purpose: a workspace and a chat key just
                            // appeared, and an operator reading the log is how
                            // they learn which conversation this root now serves.
                            tracing::info!(
                                gateway = %event.gateway,
                                conversation = %adopted.conversation,
                                slug = %adopted.slug,
                                chat_key = %adopted.chat_key,
                                "adopted a conversation nobody registered"
                            );
                            adopted
                        }
                        Err(error) => {
                            tracing::error!(
                                gateway = %event.gateway,
                                conversation = %inbound.conversation,
                                error = %format!("{error:#}"),
                                "could not adopt this conversation; the event is dropped"
                            );
                            return;
                        }
                    }
                }
            };
            // The log is a convenience, not the delivery: a failed append is
            // logged and the event still reaches its scripts.
            if let Err(error) = chatlog::append_inbound(&conversation, &inbound) {
                tracing::warn!(role = %gateway.role, error = %error, "chat log write failed");
            }
            // The role's own record, which is what a later reply counts its
            // chain in. The coordinate the platform reported is the anchor: a
            // reply into a discussion this cluster never recorded still names
            // what it answered, so the record starts from the middle rather
            // than refusing the message (`docs/v2.4-REPLY-THREADS.md` §8).
            let coordinate = inbound
                .reply
                .as_ref()
                .map(|reply| (reply.anchor().unwrap_or_default(), reply.parent.as_deref()));
            let list = crate::chain::record_inbound(
                &crate::history::History::new(&gateway.root),
                &inbound,
                &conversation.chat_key,
                coordinate,
            );
            tracing::debug!(
                role = %gateway.role,
                conversation = %inbound.conversation,
                list = %list,
                answered = ?inbound.reply.as_ref().and_then(|r| r.parent.as_deref()),
                "recorded an inbound event"
            );
            let reporter = reporters.get(&gateway.role).cloned();
            let input = PushInput::Trigger {
                role: gateway.role.clone(),
                gateway: event.gateway.clone(),
                platform: inbound.platform.clone(),
                conversation: inbound.conversation.clone(),
                chat: conversation.chat_key.clone(),
                event: Box::new(inbound),
                at: Utc::now().to_rfc3339(),
            };
            spawn_pushes(
                gateway,
                PushMode::Trigger,
                input,
                &conversation,
                options.socket.clone(),
                reporter,
            );
        }
        BackendOp::Health(health) => {
            tracing::info!(
                gateway = %event.gateway,
                state = %health.state,
                uptime_s = health.uptime_s,
                "backend health"
            );
        }
        // A backend that says its credential stopped working is telling the
        // operator what to do, not failing: it keeps its process and its other
        // conversations. The sentence it sends is the one an operator acts on.
        BackendOp::Onboarding(prompt) => {
            tracing::warn!(
                gateway = %event.gateway,
                kind = ?prompt.kind,
                "this backend needs a credential: {}",
                prompt.payload
            );
        }
        // The answers to calls this loop never makes: `login` and
        // `list_conversations` are driven by the setup verbs, which own their
        // own backend process.
        BackendOp::Login(result) => {
            tracing::debug!(gateway = %event.gateway, ok = result.ok, "an unsolicited login answer")
        }
        // A send's answer is read on the send path itself, which is what needs
        // the id; one that reaches here arrived with no caller waiting.
        BackendOp::Sent(sent) => {
            tracing::debug!(
                gateway = %event.gateway,
                conversation = %sent.conversation,
                message_id = sent.message_id.as_deref().unwrap_or("(none)"),
                "a send answered with no caller waiting"
            );
        }
        BackendOp::Conversations(_) => {
            tracing::debug!(gateway = %event.gateway, "an unsolicited conversation listing")
        }
        BackendOp::Hello(_) => {}
    }
}

impl RenderJob {
    /// The frame the owning backend renders this job with.
    fn send_args(&self) -> SendArgs {
        SendArgs {
            conversation: self.conversation.clone(),
            payload: self.payload.clone(),
        }
    }
}

/// Hand one input to every script of its mode, off the runloop.
///
/// The run is spawned rather than awaited: a script is the operator's and may
/// spend its whole timeout, and an outbound render must not wait on an inbound
/// script (`docs/v2.2-IM-GATEWAY-PLAN.md` §4, §5).
fn spawn_pushes(
    gateway: &Arc<Gateway>,
    mode: PushMode,
    input: PushInput,
    conversation: &Conversation,
    socket: Option<String>,
    reporter: Option<mpsc::Sender<Report>>,
) {
    let gateway = Arc::clone(gateway);
    let conversation = conversation.clone();
    tokio::spawn(async move {
        for script in gateway.pushes.iter().filter(|script| script.mode == mode) {
            run_one_push(
                script,
                &input,
                &conversation,
                socket.clone(),
                reporter.as_ref(),
            )
            .await;
        }
    });
}

/// Run one script for one input, reporting the failure that survives its
/// attempts.
async fn run_one_push(
    script: &crate::config::PushScript,
    input: &PushInput,
    conversation: &Conversation,
    socket: Option<String>,
    reporter: Option<&mpsc::Sender<Report>>,
) {
    let env = PushEnv {
        socket,
        workspace: Some(conversation.workspace.to_string_lossy().into_owned()),
        media: Some(conversation.media_dir().to_string_lossy().into_owned()),
    };
    let outcome =
        run_script_retrying(script, input, &env, &BTreeMap::new(), &PushRetry::default()).await;
    if let Err(failure) = outcome {
        report_push_failure(input, &script.run[0], &failure, reporter).await;
    }
}

/// Record one failed script run, on the server when the role's link can carry
/// it: the event is not lost, it is reported. The fault names the script and
/// the event it was given, so an operator can tell a broken script from a
/// broken platform (`docs/v2.2-IM-GATEWAY-PLAN.md` §8).
async fn report_push_failure(
    input: &PushInput,
    program: &str,
    failure: &PushFailure,
    reporter: Option<&mpsc::Sender<Report>>,
) {
    let role = input.role();
    tracing::warn!(role, program, error = %failure, "{PUSH_FAILED_KIND}");
    let Some(reporter) = reporter else {
        tracing::warn!(
            role,
            "no link was left to carry this {PUSH_FAILED_KIND} fault"
        );
        return;
    };
    let report = Report::Fault {
        task_id: None,
        session_id: None,
        generation: None,
        seq: None,
        kind: PUSH_FAILED_KIND.to_string(),
        reason: format!("push script {program} {failure}"),
        desired: None,
        observed: Some(serde_json::json!({
            "mode": input.mode_word(),
            "event": input.event_id(),
            "script": program,
        })),
    };
    if reporter.send(report).await.is_err() {
        tracing::warn!(role, "the link that would carry this fault is gone");
    }
}
