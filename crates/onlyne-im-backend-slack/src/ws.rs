//! The inbound stream: Slack Morphism's Socket Mode listener, owned here.
//!
//! The listener owns the connection. It mints a URL with
//! `apps.connections.open`, dials it, runs the ping/reader/writer tasks,
//! restarts a retired client from its `disconnect` frame, and acknowledges an
//! envelope after a callback returns. The two things it leaves to its caller
//! are here: the callback, and the translation from an SDK event to a standard
//! event.
//!
//! *Queue before ack.* The listener acks an envelope after the callback
//! succeeds, so the callback answers `Ok` only once the decoded event sits in
//! the queue. A closed queue is the one case that answers `Err`: nothing would
//! read that event, so Slack's redelivery is the right outcome. This is also
//! what keeps a send from starving inbound events: the listener's reader tasks
//! run on their own, and a callback that only enqueues never waits on the
//! gateway.
//!
//! *One connection.* `max_connections_count` is 1: the protocol loop reads
//! events in one place, and a second socket would deliver every event twice.
//!
//! # The callback's queue
//!
//! The SDK's callback type is a plain `fn`, so it cannot close over the queue
//! it belongs to. This backend serves one protocol loop and one stream at a
//! time, so the live stream is process state. The callback reads it, bumps an
//! in-flight count and clones what it needs under a lock it never holds across
//! an `await`.
//!
//! # The socket address
//!
//! The listener dials what its own `apps.connections.open` answers, and nothing
//! in the SDK accepts a connection address from its caller — its listener type
//! is only constructible on the SDK's own connector. `ONLYNE_SLACK_WS_URL` is
//! therefore honoured by the API root a test points at: a fake answers
//! `apps.connections.open` with the local socket's URL, which is
//! `crates/onlyne-im-backend-slack/tests/socket.rs`, and the address a Slack
//! workspace mints is dialled unmodified.

use crate::api::SlackApi;
use crate::update;
use crate::wire::SlackConnector;
use anyhow::{Context, Result};
use onlyne_im_backend::payload::ImEvent;
use slack_morphism::prelude::*;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::mpsc;

/// How many decoded events wait for the protocol loop. The loop drains them as
/// quickly as it writes frames, so this is the size of a burst.
const QUEUE: usize = 256;

/// The queue of one stream, and the names its events carry.
struct Live {
    sender: mpsc::Sender<ImEvent>,
    names: (String, String, Option<String>),
    /// Events the callback took but has not queued yet. The lock is held only
    /// to read or bump this, never across an `await`.
    in_flight: usize,
}

/// One stream's registration: the registry the callback reads, and what this
/// stream put there.
///
/// The SDK's callback type is a plain `fn`, so it cannot close over the queue
/// it belongs to. Each stream therefore owns its registry — an [`Arc`] the
/// callback finds through [`LIVE`] while the stream is the live one — and
/// retiring a stream clears exactly that registry, never another stream's.
#[derive(Clone)]
struct Registry(Arc<Mutex<Option<Live>>>);

static LIVE: LazyLock<Mutex<Registry>> =
    LazyLock::new(|| Mutex::new(Registry(Arc::new(Mutex::new(None)))));

impl Live {
    /// Make this stream the one the callback queues into.
    fn install(&mut self) -> Registry {
        let registry = Registry(Arc::new(Mutex::new(None)));
        *registry.lock() = Some(Live {
            sender: self.sender.clone(),
            names: self.names.clone(),
            in_flight: 0,
        });
        *LIVE.lock().expect("live registry") = registry.clone();
        registry
    }
}

impl Registry {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Live>> {
        self.0.lock().expect("live stream")
    }

    /// Drop this stream's queue from the callback, if the callback is idle.
    ///
    /// A callback that already took an event still holds this queue's sender;
    /// only an idle stream is retired, so nothing taken is dropped.
    fn retire(&self) {
        let mut live = self.lock();
        if live.as_ref().is_some_and(|live| live.in_flight == 0) {
            *live = None;
        }
    }
}

/// The SDK's push-events callback: queue one event, then let the listener ack.
async fn push_event(
    event: SlackPushEventCallback,
    _client: Arc<SlackClient<SlackConnector>>,
    _state: SlackClientEventsUserState,
) -> UserCallbackResult<()> {
    let registry = LIVE.lock().expect("live registry").clone();
    let taken = {
        let mut live = registry.lock();
        live.as_mut().map(|live| {
            live.in_flight += 1;
            (live.sender.clone(), live.names.clone())
        })
    };
    let Some((sender, (gateway, platform, self_id))) = taken else {
        // No stream is installed, so nothing can read this delivery.
        return Err(anyhow::anyhow!("no slack event stream is installed").into());
    };
    let decoded = update::event_from_push(&gateway, &platform, self_id.as_deref(), &event);
    let queued = match decoded {
        None => Ok(()),
        Some(decoded) => sender.send(decoded).await.map_err(|_| {
            anyhow::anyhow!("the event queue is closed; slack should redeliver").into()
        }),
    };
    if let Some(live) = registry.lock().as_mut() {
        live.in_flight = live.in_flight.saturating_sub(1);
    }
    queued
}

/// The Socket Mode connection, from this backend's side.
pub struct Inbound {
    /// The registered listener; `None` once it has been shut down.
    listener: Option<SlackClientSocketModeListener<SlackConnector>>,
    /// This stream's registry, the one the callback reads while it is live.
    live: Registry,
    /// Decoded events, in the order Slack delivered them.
    queue: mpsc::Receiver<ImEvent>,
    /// The app-level token the connection was registered with.
    app_token: String,
}

impl Inbound {
    /// Register one connection, install its queue, and let the listener dial.
    pub async fn start(
        api: &SlackApi,
        gateway: String,
        platform: String,
        self_id: Option<String>,
    ) -> Result<Self> {
        let app_token = api.app_token().to_string();
        let (sender, queue) = mpsc::channel(QUEUE);
        let registry = Live {
            sender,
            names: (gateway, platform, self_id),
            in_flight: 0,
        }
        .install();
        let callbacks = SlackSocketModeListenerCallbacks::new()
            .with_hello_events(|hello, _, _| async move {
                tracing::info!(
                    connections = hello.num_connections,
                    "slack socket mode hello"
                );
            })
            .with_push_events(push_event);
        let client = Arc::new(SlackClient::new(api.connector().clone()));
        let environment = Arc::new(SlackClientEventsListenerEnvironment::new(client));
        let listener = SlackClientSocketModeListener::new(
            &SlackClientSocketModeConfig::new().with_max_connections_count(1),
            environment,
            callbacks,
        );
        let token = SlackApiToken::new(SlackApiTokenValue(app_token.clone()));
        if let Err(error) = listener.listen_for(&token).await {
            registry.retire();
            return Err(error).context("registering a slack socket mode connection");
        }
        listener.start().await;
        Ok(Self {
            listener: Some(listener),
            live: registry,
            queue,
            app_token,
        })
    }

    /// The app-level token this stream was registered with.
    pub fn app_token(&self) -> &str {
        &self.app_token
    }

    /// Wait for one event.
    ///
    /// Dropping this future loses nothing — the event stays queued for the
    /// next call — which is what lets the serve loop poll it inside a
    /// `select!` without stopping the listener's reader tasks.
    pub async fn next_event(&mut self) -> Option<ImEvent> {
        self.queue.recv().await
    }

    /// Close the connection politely, so Slack frees the socket slot at once
    /// instead of waiting for its own timeout.
    pub async fn shutdown(&mut self) {
        if let Some(listener) = self.listener.take() {
            listener.shutdown().await;
        }
        self.live.retire();
    }
}
