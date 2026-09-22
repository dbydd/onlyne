use super::*;

use super::env::{AGENT, REQUEST_TIMEOUT};
use super::projection::with_cluster;
use super::state::{DispatchInner, DispatchState};

/// Write one ack into the durable intent queue.
pub(super) fn store_ack(inner: &DispatchInner, mut ack: AckArgs) {
    if ack.op_id.is_none() {
        ack.op_id = Some(onlyne_proto::new_op_id());
    }
    let Some(op_id) = ack.op_id.clone() else {
        return;
    };
    match serde_json::to_value(ClientOp::Ack(ack)) {
        Ok(value) => {
            if let Err(error) = inner.store.enqueue_intent(&op_id, &value) {
                tracing::warn!(error = %error, "settled ack was not stored");
            }
        }
        Err(error) => tracing::warn!(error = %error, "settled ack did not serialize"),
    }
}

/// Queue one envelope into the durable intent table while the dispatch lock is
/// already held. The `op_id` rule and the validation are `enqueue_outbound`'s.
pub(super) fn queue_outbound_locked(
    inner: &mut DispatchInner,
    envelope: &Envelope,
) -> Result<String> {
    let mut stamped = envelope.clone();
    let op_id = stamp_op_id(&mut stamped);
    stamped
        .validate()
        .map_err(|error| anyhow!(error.to_string()))?;
    inner
        .store
        .enqueue_intent(&op_id, &serde_json::to_value(&stamped)?)?;
    Ok(op_id)
}

/// Hand one envelope to the live link, or to the intent queue when it is down.
pub(super) async fn transport_envelope(state: &DispatchState, envelope: &Envelope) -> Result<()> {
    let op = ClientOp::Send(Box::new(envelope.clone()));
    let outbox = { state.inner.lock().outbox.clone() };
    match outbox {
        Some(outbox) => match outbox.send(op).await {
            Ok(()) => Ok(()),
            Err(error) => {
                tracing::warn!(error = %error, "completion fell back to the intent queue");
                state.enqueue_outbound(envelope).map(|_| ())
            }
        },
        None => state.enqueue_outbound(envelope).map(|_| ()),
    }
}

/// One authenticated server link.
///
/// The first five methods are the whole transport surface the runloop uses, so
/// a change of transport touches this struct alone. The remaining two read the
/// supervision state of the connection the handle keeps across redials.
#[derive(Clone)]
pub struct ClientLink {
    handle: ClientConn,
    welcome: Arc<Welcome>,
    hello: HandshakeArgs,
}

impl ClientLink {
    /// Dial, verify the certificate pin, sign the server challenge, then read
    /// the role slice with `hello`.
    pub async fn connect(init: &ClientInit, live_tasks: Vec<String>) -> Result<Self, NetError> {
        let keypair = KeyPair::load(&init.key_path)?;
        let settings = ConnSettings {
            agent: AGENT.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            ..ConnSettings::new(PROTOCOL_VERSION)
        };
        let handle: ClientConn =
            dial(&init.server, &keypair, &init.cert_pin, &init.role, settings).await?;
        let hello = HandshakeArgs {
            protocol: PROTOCOL_VERSION,
            role: init.role.clone(),
            key: keypair.public_str(),
            signature: String::new(),
            agent: AGENT.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            aggregate: false,
            live_tasks: Vec::new(),
        };
        let body = handle
            .request(
                Frame::req(
                    String::new(),
                    ClientOp::Hello(hello_with_live_tasks(&hello, live_tasks)),
                ),
                REQUEST_TIMEOUT,
            )
            .await?;
        if !body.ok {
            let error = body.error.clone().unwrap_or(onlyne_proto::ErrorPayload {
                code: onlyne_proto::ErrorCode::Internal,
                message: "hello refused".to_string(),
                field: None,
            });
            return Err(NetError::Rejected {
                code: wire_code(error.code),
                message: error.message,
            });
        }
        let data = body.data().cloned().ok_or(NetError::BadFrame)?;
        let welcome: Welcome = serde_json::from_value(data).map_err(|_| NetError::BadFrame)?;
        Ok(Self {
            handle,
            welcome: Arc::new(welcome),
            hello,
        })
    }

    /// Send the routed `hello` again on the connection this link now holds.
    ///
    /// The net layer redials on its own, and a fresh connection carries no role
    /// binding until this frame lands, so a caller replays it whenever readiness
    /// returns (plan §7 line 310).
    pub async fn authenticate(&self, live_tasks: Vec<String>) -> Result<(), NetError> {
        let body = self
            .handle
            .request(
                Frame::req(
                    String::new(),
                    ClientOp::Hello(hello_with_live_tasks(&self.hello, live_tasks)),
                ),
                REQUEST_TIMEOUT,
            )
            .await?;
        if !body.ok {
            let error = body.error.clone().unwrap_or(onlyne_proto::ErrorPayload {
                code: onlyne_proto::ErrorCode::Internal,
                message: "hello refused".to_string(),
                field: None,
            });
            return Err(NetError::Rejected {
                code: wire_code(error.code),
                message: error.message,
            });
        }
        Ok(())
    }

    /// Role slice the server bound this link to.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Send one request frame and answer with its body. A refusal from the
    /// server arrives inside the body, which keeps the retry decision in the
    /// intent machine.
    pub async fn request(&self, op: ClientOp) -> Result<ResBody, NetError> {
        self.handle
            .request(Frame::req(String::new(), op), REQUEST_TIMEOUT)
            .await
    }

    /// Clone the server observation stream.
    pub fn events(&self) -> broadcast::Receiver<Frame<ClientOp>> {
        self.handle.events()
    }

    /// Send `bye` and drain in-flight requests.
    pub async fn close(&self) -> Result<(), NetError> {
        self.handle.close().await
    }

    /// Liveness of the connection behind this link.
    pub fn readiness(&self) -> ConnReadiness {
        self.handle.readiness()
    }
    /// Reason the supervisor stopped redialing.
    pub async fn failure(&self) -> Option<NetError> {
        self.handle.failure().await
    }
}

/// Stamp dispatch live-slot task ids onto a hello skeleton.
///
/// Slots exist from assign until release. A fresh process has none, so hello
/// sends an empty list and the server requeues. A live client whose link
/// flaps still holds its slots, so those rows stay in_flight.
pub(crate) fn hello_with_live_tasks(
    hello: &HandshakeArgs,
    live_tasks: Vec<String>,
) -> HandshakeArgs {
    let mut hello = hello.clone();
    hello.live_tasks = live_tasks;
    hello
}

/// Wire code of a refusal, in the snake_case spelling both sides share.
fn wire_code(code: onlyne_proto::ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "internal".to_string())
}

/// Where lifecycle frames leave the dispatcher.
///
/// `send` completes once the frame is on the wire. The ready report awaits this
/// before the payload reaches the agent, which is the causal order §6 fixes.
pub trait Outbox: Send + Sync {
    fn send(&self, op: ClientOp)
    -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>>;

    /// One request round trip, for a caller that needs the server's answer
    /// rather than a queued frame: the local CLI reports the verdict a send got.
    fn request(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<ResBody, NetError>> + Send + '_>>;
}

impl Outbox for ClientLink {
    fn send(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>> {
        Box::pin(async move { self.request(op).await.map(|_| ()) })
    }

    fn request(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<ResBody, NetError>> + Send + '_>> {
        Box::pin(async move { ClientLink::request(self, op).await })
    }
}

/// Deliver one lifecycle frame, and queue it durably when the link is down.
///
/// §6 line 289: a running session reaches its terminal state while the outbound
/// work waits in `client.db` intents for the flusher.
///
/// The frame that could not leave says nothing about the link, so the shared
/// accept gate is left exactly where the runloop put it. A send fails with the
/// link still `Ready` — the request deadline belongs to the caller, and
/// `onlyne_net::conn` records that "the silent peer keeps the link up; only this
/// call gave up" — and dropping the flag here latched it: `watch_readiness` only
/// re-arms `accept_new` on a readiness transition, so the pull loop stopped
/// draining the role's inbox for the life of that link while the work sat queued
/// on the server, and the next delivery to arrive found a client that refused
/// new work. The connection's own state is the flag's only author
/// (`runloop::link`).
pub async fn send_frame(state: &DispatchState, op: ClientOp) -> Result<()> {
    let op = match op {
        ClientOp::Report(report) => ClientOp::Report(with_cluster(state, report)),
        other => other,
    };
    if let Some(outbox) = state.outbox() {
        if outbox.send(op.clone()).await.is_ok() {
            return Ok(());
        }
    }
    state.enqueue_op(&op)?;
    Ok(())
}

impl DispatchState {
    /// Queue an outbound envelope before its first write and answer its op_id.
    ///
    /// The queue keys every row by an `op_id`, and the proto requires that key
    /// only for the non-note kinds, so a note that arrives without one gets a
    /// fresh client-minted id here: the row is keyed and what it replays is the
    /// whole stamped envelope. A non-note keeps the id it brought, so a
    /// re-delivered task still dedups on its original one.
    pub fn enqueue_outbound(&self, envelope: &Envelope) -> Result<String> {
        queue_outbound_locked(&mut self.inner.lock(), envelope)
    }

    /// The flag the runloop and the dispatcher share.
    pub fn accept_new(&self) -> Arc<AtomicBool> {
        self.inner.lock().accept_new.clone()
    }

    /// Whether the role holds a ready server link.
    pub fn link_up(&self) -> bool {
        self.inner.lock().link_up.load(Ordering::SeqCst)
    }

    /// Record that the server link came up or went down.
    pub fn set_link_up(&self, up: bool) {
        self.inner.lock().link_up.store(up, Ordering::SeqCst);
    }

    /// Aggregate name this role supervises, empty for a plain role.
    pub fn cluster_ref(&self) -> String {
        self.inner.lock().cluster_ref.clone()
    }

    /// Record the server's topology name, read from `welcome.cluster`.
    ///
    /// Each spawned session carries it as `ONLYNE_CLUSTER`, which is how a host
    /// backend (herdr) addresses the tree it splits panes into. The runloop calls
    /// this on every welcome, so a server that reloads under a new name is
    /// followed by the sessions spawned after that point.
    pub fn set_topology(&self, cluster: &str) {
        self.inner.lock().topology = cluster.trim().to_string();
    }

    /// The topology name recorded from `welcome`, empty before the first welcome.
    pub fn topology(&self) -> String {
        self.inner.lock().topology.clone()
    }

    /// Record the aggregate name once, so every report keeps the same value
    /// across a reconnect.
    pub fn set_cluster_ref(&self, aggregate: impl Into<String>) {
        self.inner.lock().cluster_ref = aggregate.into();
    }

    /// Ask the server one question over the live link.
    ///
    /// Err means the link is down, never a refusal: a refusal arrives as an
    /// `Ok` body carrying `ok: false`, which is what the local CLI shows.
    pub async fn request(&self, op: ClientOp) -> Result<ResBody, NetError> {
        let outbox = { self.inner.lock().outbox.clone() };
        let Some(outbox) = outbox else {
            return Err(NetError::NotReady);
        };
        outbox.request(op).await
    }

    /// Install the live link as the outbound path.
    pub fn attach_outbox(&self, outbox: Arc<dyn Outbox>) {
        self.inner.lock().outbox = Some(outbox);
    }

    /// Remove the outbound path; lifecycle frames then queue as intents.
    pub fn detach_outbox(&self) {
        self.inner.lock().outbox = None;
    }

    fn outbox(&self) -> Option<Arc<dyn Outbox>> {
        self.inner.lock().outbox.clone()
    }

    /// Queue one client op in the durable intent table and answer its op_id.
    pub fn enqueue_op(&self, op: &ClientOp) -> Result<String> {
        let op_id = onlyne_proto::new_id();
        self.inner
            .lock()
            .store
            .enqueue_intent(&op_id, &serde_json::to_value(op)?)?;
        Ok(op_id)
    }
}
