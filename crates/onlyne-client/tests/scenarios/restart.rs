//! A client that restarts over its own work: the rows the server requeued when the
//! previous process died, the ending a task whose agent is gone has to reach, and
//! the accept gate those two paths read.
//!
//! The fixture is a fake cluster (the TLS endpoint, the handshake, and one ledger
//! row, plus the mirrored projection its publishes write) that speaks the shipped
//! protocol: `hello` requeues every in-flight row the registering role does not
//! claim, `pull` hands one row at a time and arms its ticket, `ack` settles it, and
//! a projection-carrying heartbeat lands in the mirror. What the client does with a
//! row the server requeued, how it leaves a row it has already answered alone on the
//! way back up, what it records for a row whose session died under it, what that
//! session's own ending tells the mirror, and who may shut intake, are the five
//! facts the cases below pin.

use crate::common::{complete_plugin, plugin_beat};
use onlyne_adapter::AdapterIo;
use onlyne_client::ClientInit;
use onlyne_client::session::dispatch::{DispatchState, Outbox, send_frame};
use onlyne_frame::{read_frame, write_frame};
use onlyne_layout::{RoleWorkspace, connect_local};
use onlyne_net::NetError;
use onlyne_net::{
    KeyPair, TcpListen, TlsConn, accept as accept_handshake, gen_self_signed, server_config,
    table_from,
};
use onlyne_proto::{
    AckArgs, AdapterMsg, AgentMount, Body, Capability, Causality, ClientOp, Delivery, Envelope,
    Frame, HandshakeArgs, HelloArgs, HostOp, LedgerEntry, LedgerQuery, LedgerState, Lifecycle,
    Mount, MountKind, MsgKind, Outcome, PROTOCOL_VERSION, PluginOp, Presence, Principal, PullReply,
    Report, ResBody, RoleInfo, SessionProjection, Welcome, new_envelope,
};
use onlyne_session::backend::fake::FakeBackend;
use onlyne_session::{SessionLedger, TaskState};
use onlyne_store::ClientStore;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tempfile::TempDir;
use tempfile::tempdir;
use tokio::sync::mpsc::UnboundedReceiver;

/// One delivery row in the fake cluster's ledger.
#[derive(Debug)]
struct Row {
    msg_id: String,
    task_id: String,
    body: String,
    state: LedgerState,
    /// Whether a delivery ticket holds the row out of `pull`'s reach. Every pull in
    /// this fixture is a role-level one, so a live ticket carries no session id —
    /// the shape `relay::pull` arms, and the shape the server's release path
    /// compares against.
    armed: bool,
    /// Deliveries handed out, counting the one the previous process died holding.
    handed: u32,
    /// Times the row went back to `queued`, which is what a hello that claims
    /// nothing does to a row a dead link left in flight.
    requeued: u32,
    reason: Option<String>,
    acked_at: Option<chrono::DateTime<chrono::Utc>>,
    enqueued_at: chrono::DateTime<chrono::Utc>,
}

impl Row {
    fn new(msg_id: &str, task_id: &str, state: LedgerState, handed: u32) -> Self {
        Self {
            msg_id: msg_id.to_string(),
            task_id: task_id.to_string(),
            body: format!("work for {task_id}"),
            state,
            armed: false,
            handed,
            requeued: 0,
            reason: None,
            acked_at: None,
            enqueued_at: fixture_time(),
        }
    }

    fn entry(&self) -> LedgerEntry {
        LedgerEntry {
            msg_id: self.msg_id.clone(),
            op_id: None,
            kind: MsgKind::Task,
            from: Principal::role("sender"),
            to: Principal::role("planner"),
            task: Some(self.task_id.clone()),
            parent_task: None,
            hop: 0,
            attempt: 0,
            state: self.state,
            reason: self.reason.clone(),
            out_head: Some(self.body.clone()),
            body_json: Some(serde_json::to_string(&Body::text(self.body.clone())).unwrap()),
            enqueued_at: self.enqueued_at,
            acked_at: self.acked_at,
        }
    }
}

/// The fake cluster: one role's ledger rows, the claims each hello declared, and
/// everything the client reported or acknowledged.
#[derive(Default)]
struct Cluster {
    rows: Vec<Row>,
    claims: Vec<Vec<String>>,
    reports: Vec<Report>,
    /// The envelopes the client sent, which is where a task's completion travels:
    /// the terminal receipt is a `Completion` addressed to the task's origin, not a
    /// `report`.
    sends: Vec<Envelope>,
    acks: Vec<AckArgs>,
    handed: u32,
    /// Pulls answered. Each one follows the hello, subscribe, and intent flush of
    /// the link it arrived on, so a test reads this as "the client has been
    /// through its post-connect order again".
    pulls: u32,
    /// Connections the fixture closes after answering their first pull. One flap
    /// is what `link_loop` answers with a fresh `hello`, a fresh flush, and — for
    /// the sweep that used to run per link — a fresh pass over the ledger.
    closes: u32,
    /// The mirrored row per session, as the server keeps it: the last projection a
    /// heartbeat from this client carried, behind the same `(generation, seq)` gate
    /// the real mirror applies. Nothing else writes it — this fixture runs no
    /// observer — so what a row reads here is exactly what the client reported.
    mirror: HashMap<String, Mirrored>,
}

/// One session's mirrored projection with the watermark that admitted it, which is
/// what the real mirror keeps of a client's publish.
#[derive(Clone)]
struct Mirrored {
    generation: u64,
    seq: u64,
    projection: SessionProjection,
}

impl Cluster {
    fn row(&self, msg_id: &str) -> &Row {
        self.rows
            .iter()
            .find(|row| row.msg_id == msg_id)
            .expect("the fixture seeded this row")
    }

    /// Adopt one `hello`: rows the registering role does not claim go back to the
    /// queue and their tickets are dropped (`relay::requeue_role_rows` plus the
    /// `keep_deliveries` that makes a requeued row claimable again).
    fn hello(&mut self, args: &HandshakeArgs) -> Welcome {
        self.claims.push(args.live_tasks.clone());
        let claimed: HashSet<String> = args.live_tasks.iter().cloned().collect();
        for row in self.rows.iter_mut() {
            if row.state == LedgerState::InFlight && !claimed.contains(&row.task_id) {
                row.state = LedgerState::Queued;
                row.armed = false;
                row.requeued += 1;
            }
        }
        welcome()
    }

    /// Hand out one row the way `relay::pull` does: the oldest queued row, or an
    /// in-flight row no ticket holds. Nothing is offered while the role asked for
    /// control rows only, which this fixture carries none of.
    fn take(&mut self, control_only: bool) -> Option<Delivery> {
        if control_only {
            return None;
        }
        let index = self
            .rows
            .iter()
            .position(|row| row.state == LedgerState::Queued)
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|row| row.state == LedgerState::InFlight && !row.armed)
            })?;
        let row = &mut self.rows[index];
        row.state = LedgerState::InFlight;
        row.armed = true;
        row.handed += 1;
        self.handed += 1;
        Some(delivery_of(row))
    }

    /// Settle one row the way `relay::ack` does, at-least-once and idempotent.
    fn ack(&mut self, args: &AckArgs) {
        self.acks.push(args.clone());
        let Some(row) = self.rows.iter_mut().find(|row| row.msg_id == args.msg_id) else {
            return;
        };
        row.armed = false;
        if matches!(
            row.state,
            LedgerState::Acked | LedgerState::Rejected | LedgerState::Expired
        ) {
            return;
        }
        if args.accepted {
            row.state = LedgerState::Acked;
        } else {
            row.state = LedgerState::Rejected;
            row.reason = args.reason.clone();
        }
        row.acked_at = Some(fixture_time());
    }

    /// Apply one projection-carrying heartbeat the way `projection::write` does:
    /// the projection lands in the row verbatim, and only when its
    /// `(generation, seq)` is ahead of the watermark that row already holds.
    fn publish(
        &mut self,
        session_id: &str,
        generation: u64,
        seq: u64,
        projection: &SessionProjection,
    ) {
        let ahead = self
            .mirror
            .get(session_id)
            .is_none_or(|held| (generation, seq) > (held.generation, held.seq));
        if ahead {
            self.mirror.insert(
                session_id.to_string(),
                Mirrored {
                    generation,
                    seq,
                    projection: projection.clone(),
                },
            );
        }
    }

    /// What the mirrored row for one session reads, or `None` when no publish has
    /// named it.
    fn mirrored(&self, session_id: &str) -> Option<&SessionProjection> {
        self.mirror.get(session_id).map(|held| &held.projection)
    }

    /// The ledger read the client's startup reconcile asks for.
    fn ledger(&self, query: &LedgerQuery) -> Vec<LedgerEntry> {
        self.rows
            .iter()
            .filter(|row| query.state.is_none_or(|state| state == row.state))
            .map(Row::entry)
            .collect()
    }
}

/// Spend one scheduled close, and answer whether this link is the one to drop.
fn take_close(cluster: &Arc<Mutex<Cluster>>) -> bool {
    let mut cluster = cluster.lock();
    if cluster.closes == 0 {
        return false;
    }
    cluster.closes -= 1;
    true
}

/// The fake cluster's endpoint and the client that dials it.
struct Fixture {
    _dir: TempDir,
    workspace: PathBuf,
    socket: PathBuf,
    init: ClientInit,
    cluster: Arc<Mutex<Cluster>>,
    server: tokio::task::JoinHandle<()>,
}

/// Bring up one fake cluster with the workspace and key pair a role client needs.
///
/// `adjust` is the client's own configuration for the case at hand, so a test never
/// has to re-state the TLS, workspace, or backend setup to change one knob.
async fn fixture(cluster: Cluster, adjust: impl FnOnce(ClientInit) -> ClientInit) -> Fixture {
    let dir = tempdir().unwrap();
    let workspace = RoleWorkspace::resolve(dir.path());
    workspace.bootstrap().unwrap();

    let keypair = KeyPair::generate();
    let key_path = workspace.key_path();
    std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
    keypair.save(&key_path).unwrap();

    let certificate = gen_self_signed("127.0.0.1", 1).unwrap();
    let config = server_config(&certificate).unwrap();
    let mut listener = TcpListen::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let table = table_from([(
        "planner".to_string(),
        keypair.public_str(),
        false,
        Vec::new(),
        Vec::new(),
    )])
    .unwrap();

    let shared = Arc::new(Mutex::new(cluster));
    let serving = shared.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok(accepted) = listener.accept_next(&config).await else {
                break;
            };
            let table = table.clone();
            let serving = serving.clone();
            tokio::spawn(async move {
                let TlsConn::Server(mut stream) = accepted else {
                    return;
                };
                if accept_handshake(&mut stream, &table, PROTOCOL_VERSION)
                    .await
                    .is_err()
                {
                    return;
                }
                while let Ok(Some(frame)) = read_frame::<_, Frame<ClientOp>>(&mut stream).await {
                    let Frame::Req { id, op } = frame else {
                        continue;
                    };
                    let pulled = matches!(op, ClientOp::Pull(_));
                    let body = respond(&serving, op);
                    if write_frame(&mut stream, &Frame::res(id, body))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    // The flap a case asked for: the answer is on the wire and the
                    // link dies behind it, which `watch_readiness` reads as
                    // `Reconnecting` and the runloop's accept gate as new work it is
                    // no longer taking.
                    if pulled && take_close(&serving) {
                        break;
                    }
                }
            });
        }
    });

    let init = adjust(
        ClientInit::new(
            workspace.root(),
            "planner",
            &endpoint,
            &key_path,
            certificate.spki_pin.clone(),
        )
        // CI and `env -u` local runs have no herdr/orca/zellij; `fake` is the
        // backend that needs no host surface, and the session this case stages
        // answers through the plugin the test mounts.
        .with_backend("fake"),
    );
    Fixture {
        _dir: dir,
        workspace: workspace.root().to_path_buf(),
        socket: workspace.socket_path(),
        init,
        cluster: shared,
        server,
    }
}

fn respond(cluster: &Arc<Mutex<Cluster>>, op: ClientOp) -> ResBody {
    match op {
        ClientOp::Hello(args) => {
            let welcome = cluster.lock().hello(&args);
            ResBody::ok(serde_json::to_value(welcome).unwrap_or_default())
        }
        ClientOp::Subscribe(_) => ResBody::ok(serde_json::json!({"subscribed": true})),
        ClientOp::Pull(args) => {
            let mut cluster = cluster.lock();
            cluster.pulls += 1;
            let delivery = cluster.take(args.control_only.unwrap_or(false));
            ResBody::ok(
                serde_json::to_value(PullReply {
                    deliveries: delivery.into_iter().collect(),
                    seq: 1,
                })
                .unwrap_or_default(),
            )
        }
        ClientOp::Ack(args) => {
            cluster.lock().ack(&args);
            ResBody::ok(serde_json::json!({"state": "settled"}))
        }
        ClientOp::Report(report) => {
            let mut cluster = cluster.lock();
            // The projection-carrying heartbeat is the state publish, and this
            // fixture mirrors it the way the server does. A bare beat carries no
            // projection and moves no row.
            if let Report::Heartbeat {
                session_id,
                generation,
                seq,
                projection: Some(projection),
                ..
            } = &report
            {
                cluster.publish(session_id, *generation, *seq, projection);
            }
            cluster.reports.push(report);
            ResBody::ok(serde_json::json!({"applied": true}))
        }
        ClientOp::Send(envelope) => {
            cluster.lock().sends.push(*envelope);
            ResBody::ok(serde_json::json!({"task": "queued"}))
        }
        ClientOp::QueryLedger(query) => {
            let rows = cluster.lock().ledger(&query);
            ResBody::ok(serde_json::json!({ "ledger": rows }))
        }
        ClientOp::QueryRoles(_) => ResBody::ok(serde_json::json!({ "roles": [role_info()] })),
        _ => ResBody::ok(serde_json::json!({})),
    }
}

/// One fixed instant for the fixture's ledger stamps.
fn fixture_time() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn delivery_of(row: &Row) -> Delivery {
    Delivery {
        msg_id: row.msg_id.clone(),
        envelope: Box::new(
            new_envelope(
                MsgKind::Task,
                Principal::role("sender"),
                Principal::role("planner"),
                Body::text(row.body.clone()),
                Some(Causality::root(row.task_id.clone())),
            )
            .expect("the task envelope"),
        ),
    }
}

fn welcome() -> Welcome {
    Welcome {
        cluster: "cluster-a".into(),
        server: "srv".into(),
        role: "planner".into(),
        admin: false,
        max_sessions: 1,
        prose: "planner prose".into(),
        spec_hash: "hash".into(),
        aggregate: None,
        allowed_targets: vec![],
        allowed_senders: vec![],
        session_command: None,
        timeout_ready_ms: None,
        timeout_idle_ms: None,
        intent_attempts: Some(3),
        intent_backoff_ms: Some(vec![10, 20]),
        relay_required: None,
        relay_count: None,
        seq: 1,
    }
}

fn role_info() -> RoleInfo {
    RoleInfo {
        name: "planner".into(),
        admin: false,
        max_sessions: 1,
        session_command: Vec::new(),
        spec_hash: "hash".into(),
        prose: Some("planner prose".into()),
        state: Presence::Online,
        sessions: 0,
        queued: 0,
        detail: None,
        edges: Vec::new(),
        aggregate: None,
        relay_required: None,
        relay_count: None,
    }
}

/// Mount one always-running plugin once the client's socket is up.
///
/// The socket is bound by the client's own acceptor, so the first attempts race
/// the bind rather than the plugin.
async fn mount_when_ready(socket: &Path) -> (AdapterIo, UnboundedReceiver<String>) {
    for _ in 0..400 {
        if let Ok(mounted) = try_mount(socket).await {
            return mounted;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the role socket never admitted a plugin: {socket:?}");
}

async fn try_mount(socket: &Path) -> Result<(AdapterIo, UnboundedReceiver<String>), ()> {
    let stream = connect_local(socket).await.map_err(|_| ())?;
    let (io, mut inbound) =
        AdapterIo::new_with_inbound(stream, Duration::from_secs(5), Duration::from_secs(5));
    let (assigns_tx, assigns_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(frame) = inbound.recv().await {
            if let AdapterMsg::Host(HostOp::Assign(assign)) = frame.msg {
                let _ = assigns_tx.send(assign.task_id);
            }
        }
    });
    let hello = HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: "onlyne-agent-test".into(),
        version: "1.0.0".into(),
        kind: MountKind::Agent,
        capabilities: vec![Capability::Report, Capability::Inject, Capability::Recycle],
        mount: Some(Mount::Agent(AgentMount {
            role: "planner".into(),
            // The always-running agent: it names no session and parks as the
            // connection for the next staged session.
            session: None,
            task_id: None,
            pid: None,
        })),
    };
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Hello(hello)))
        .await
        .map_err(|_| ())?;
    if !body.ok {
        return Err(());
    }
    Ok((io, assigns_rx))
}

/// Poll a condition without racing the socket-level hand-off.
async fn eventually(mut predicate: impl FnMut() -> bool, what: &str) {
    for _ in 0..400 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A client that restarts over work left in flight drains that work and runs it.
///
/// The row is the shape a killed process leaves behind: delivered once, never
/// acknowledged, its ticket armed on a link that no longer exists. The restart
/// claims nothing — `live_tasks` is the live pane's own declaration and a fresh
/// process holds no slots — so the server requeues the row, and the client's pull
/// is the only thing that can bring it back. What proves it ran is the agent's
/// half: the mounted plugin is handed the assignment, its completion reaches the
/// server, and the row settles `acked`.
#[tokio::test]
async fn a_restart_drains_the_work_left_in_flight() {
    let task = onlyne_proto::new_task_id();
    let mut cluster = Cluster::default();
    cluster
        .rows
        .push(Row::new("msg-restart", &task, LedgerState::InFlight, 1));
    let fixture = fixture(cluster, |init| init).await;
    let client = tokio::spawn(onlyne_client::run(fixture.init.clone()));
    let (io, mut assigns) = mount_when_ready(&fixture.socket).await;

    let assigned = tokio::time::timeout(Duration::from_secs(10), assigns.recv())
        .await
        .expect("the requeued row reaches the agent")
        .expect("the plugin connection stays open");
    assert_eq!(
        assigned, task,
        "the restarted client staged a session for the row it left in flight"
    );
    complete_plugin(&io, &task, Outcome::Done).await;

    let settled = fixture.cluster.clone();
    eventually(
        move || settled.lock().row("msg-restart").state == LedgerState::Acked,
        "the redelivered row settles acked",
    )
    .await;

    let cluster = fixture.cluster.lock();
    assert_eq!(
        cluster.claims,
        vec![Vec::<String>::new()],
        "a restarted process declares no live task at hello"
    );
    assert_eq!(
        cluster.row("msg-restart").requeued,
        1,
        "the hello requeued the row the dead link left in flight"
    );
    assert_eq!(
        cluster.row("msg-restart").handed,
        2,
        "the row was handed to the restarted client"
    );
    assert!(
        cluster
            .acks
            .iter()
            .any(|ack| ack.msg_id == "msg-restart" && ack.accepted),
        "the redelivery was answered with an accepted ack: {:?}",
        cluster.acks
    );
    assert!(
        cluster.sends.iter().any(|envelope| {
            envelope.kind == MsgKind::Completion
                && envelope.task_id().is_some_and(|named| named == task)
                && envelope
                    .body
                    .text
                    .as_deref()
                    .is_some_and(|head| !head.is_empty())
        }),
        "the completion of the redelivered task reached the server: {:?}",
        cluster.sends
    );
    drop(cluster);

    client.abort();
    fixture.server.abort();
}

/// A row this role already answered is never re-reported as failed.
///
/// `Acked` is what the ledger reads after a settle: this client acks inside
/// `on_out`, in the same section that writes the task's own record, so an acked
/// inbound row is work this role answered rather than work still owed. The startup
/// self-check read that state as "delivered and acked, with a session that is
/// gone" and reported `failed{session_dead}` for it, which the server mirrors as
/// the session's ending — so a task that had ended `Done` read `failed` in
/// `onlyne sessions` after the next start, the oldest rows first. The ending a
/// session owes is the sweep's to write
/// (`a_task_whose_agent_is_gone_reaches_a_recorded_ending`), so nothing reports
/// anything for a row the role already answered, on the link it starts with or on
/// the one it reconnects to.
///
/// The other half of that sentence is in this case too: the row whose agent left
/// before answering is buried by the same sweep, past the reconnect grace, with the
/// reason the ledger and `onlyne sessions` read for a death.
#[tokio::test]
async fn an_answered_row_is_left_alone_while_a_dead_session_still_ends() {
    let answered = onlyne_proto::new_task_id();
    let owed = onlyne_proto::new_task_id();
    let mut cluster = Cluster::default();
    let mut row = Row::new("msg-answered", &answered, LedgerState::Acked, 1);
    // Older than any grace the client could still be holding.
    row.acked_at = Some(fixture_time());
    cluster.rows.push(row);
    // The work this client does take, and whose agent leaves before it answers.
    cluster
        .rows
        .push(Row::new("msg-owed", &owed, LedgerState::Queued, 0));
    let fixture = fixture(cluster, |init| init.with_reconnect_grace_secs(1)).await;

    // The other half of the acked row: the verdict this role's own settle wrote,
    // which is what the mirror had already been told.
    let store = ClientStore::open(fixture.workspace.join(".onlyne/client.db")).unwrap();
    store
        .open_task(&Causality::root(answered.clone()), "root")
        .unwrap();
    store.settle_task(&answered, TaskState::Done).unwrap();

    let client = tokio::spawn(onlyne_client::run(fixture.init.clone()));
    // An always-running agent mounts, and the queued row is handed to it.
    let (io, mut assigns) = mount_when_ready(&fixture.socket).await;
    let assigned = tokio::time::timeout(Duration::from_secs(10), assigns.recv())
        .await
        .expect("the queued row reaches the agent")
        .expect("the plugin connection stays open");
    assert_eq!(assigned, owed, "the agent takes the row still owed");

    // The link flaps under a client that is already serving, so the role comes back
    // over its own ledger and runs the whole post-connect order a second time. The
    // flap is armed here rather than at the fixture: the first pass over the ledger
    // has to have had its chance, and an armed flap swallows the very request that
    // would take it.
    fixture.cluster.lock().closes = 1;
    let again = fixture.cluster.clone();
    eventually(
        move || {
            let cluster = again.lock();
            cluster.claims.len() >= 2 && cluster.pulls >= 4
        },
        "the client reconnects and settles into its post-connect order again",
    )
    .await;

    // The one ending that is still this client's to write: the agent that left
    // before answering. Dropping the plugin's half of the socket is the death the
    // sweep reads, and past `reconnect_grace_secs` it buries the session and files
    // what the work owed.
    drop(io);
    let buried = fixture.cluster.clone();
    eventually(
        move || buried.lock().row("msg-owed").state == LedgerState::Rejected,
        "the dead session's row is refused rather than left in flight",
    )
    .await;

    let cluster = fixture.cluster.lock();
    assert!(
        cluster.reports.iter().all(|report| !matches!(
            report,
            Report::Complete { task_id, .. } if task_id == &answered
        )),
        "a later connect reports no ending for a row this role answered: {:?}",
        cluster.reports
    );
    assert_eq!(
        cluster.row("msg-answered").state,
        LedgerState::Acked,
        "the row keeps the answer it was given"
    );
    let ended = cluster.row("msg-owed");
    assert_eq!(ended.state, LedgerState::Rejected, "{ended:?}");
    assert_eq!(
        ended.reason.as_deref(),
        Some(onlyne_client::session::dispatch::SESSION_DEAD),
        "the ending names the death the client judged: {ended:?}"
    );
    drop(cluster);

    // The role's own account says the same: the answered task keeps its verdict, the
    // task whose agent left is settled failed, and nothing is left open for the
    // server to offer again.
    let store = ClientStore::open(fixture.workspace.join(".onlyne/client.db")).unwrap();
    let record = store
        .task(&answered)
        .unwrap()
        .expect("the answered task has a record");
    assert_eq!(record.task_state, TaskState::Done, "{record:?}");
    assert!(record.settled_at.is_some(), "{record:?}");
    let record = store
        .task(&owed)
        .unwrap()
        .expect("the task whose agent left has a record");
    assert_eq!(record.task_state, TaskState::Failed, "{record:?}");
    assert!(record.settled_at.is_some(), "{record:?}");

    client.abort();
    fixture.server.abort();
}

/// A task whose agent is gone reaches a recorded ending, and does not come back on
/// its own.
///
/// The session is staged before any agent exists — the order an always-running
/// plugin takes, and the reason a staged session is allowed to wait — and no plugin
/// ever mounts. Past `[client] reconnect_grace_secs` the sweep retires the session
/// and files the verdict the work owed, so the task reads `failed` here. The
/// delivery row has to read the death too: a row left `in_flight` is handed to a
/// pull no longer, so nothing would answer for this task until the link dropped, and
/// an operator reading `onlyne ledger` would see a session that has been buried as
/// one still holding its delivery. The ending is recorded as a refusal carrying the
/// reason the residual account already uses — terminal, visible, and brought back
/// only by an operator's `repair retry`.
#[tokio::test]
async fn a_task_whose_agent_is_gone_reaches_a_recorded_ending() {
    let task = onlyne_proto::new_task_id();
    let mut cluster = Cluster::default();
    cluster
        .rows
        .push(Row::new("msg-dead", &task, LedgerState::Queued, 0));
    let fixture = fixture(cluster, |init| init.with_reconnect_grace_secs(1)).await;
    let workspace = fixture.workspace.clone();
    let client = tokio::spawn(onlyne_client::run(fixture.init.clone()));

    let claimed = fixture.cluster.clone();
    eventually(
        move || claimed.lock().row("msg-dead").state == LedgerState::InFlight,
        "the client pulls the row it has no agent for",
    )
    .await;

    let recorded = fixture.cluster.clone();
    eventually(
        move || recorded.lock().row("msg-dead").state == LedgerState::Rejected,
        "the death of the session is recorded on the delivery row",
    )
    .await;

    let cluster = fixture.cluster.lock();
    let row = cluster.row("msg-dead");
    assert_eq!(
        row.reason.as_deref(),
        Some(onlyne_client::session::dispatch::SESSION_DEAD),
        "the refusal names the death the client judged: {row:?}",
    );
    assert_eq!(
        row.handed, 1,
        "nothing hands the work out again on its own: a refusal is terminal"
    );
    assert!(
        cluster
            .acks
            .iter()
            .any(|ack| ack.msg_id == "msg-dead" && !ack.accepted),
        "the delivery the client could not run was refused, not left in flight: {:?}",
        cluster.acks
    );
    drop(cluster);

    // The workspace's own record carries the same ending: the task was settled
    // failed by the sweep, its session reads agent-gone and resource-closed, and
    // nothing is left open for the server to re-offer.
    let store = ClientStore::open(workspace.join(".onlyne/client.db")).unwrap();
    let record = store.task(&task).unwrap().expect("the task has a record");
    assert_eq!(record.task_state, TaskState::Failed, "{record:?}");
    assert!(record.settled_at.is_some(), "{record:?}");
    let session = store
        .get_session(&task)
        .unwrap()
        .expect("the session has a row");
    assert_eq!(session.agent_state, "gone", "{session:?}");
    assert_eq!(session.resource_state, "closed", "{session:?}");
    assert!(
        store.open_tasks(10).unwrap().is_empty(),
        "nothing is left open for the server to re-offer"
    );

    client.abort();
    fixture.server.abort();
}

/// A session the reconnect grace retires publishes its own exit, so the mirrored
/// row stops reading `working` without the server's observer running.
///
/// The mirror holds what this client last reported for a session. A plugin that
/// mounts and beats publishes the session `working`; its connection then dies
/// without a `detach`, and past `[client] reconnect_grace_secs` the sweep is what
/// ends the session. Before this, the sweep filed the ending locally only — the task
/// settled `failed`, the delivery row refused with `session_dead` — while the
/// server's row kept reading `working` until its own observer recorded a
/// `stale_working` or `heartbeat_missing` fault, and that fault names a silence
/// without moving the row. So the retired session travels the report an ordinary
/// ending already travels, in the same sweep pass and after the row is final.
///
/// This fixture runs no observer at all: the mirrored row is written by the
/// client's own publishes and nothing else, so a row that reads `exited` here is
/// the client's doing, beside the verdict and the delivery row the same sweep
/// filed.
#[tokio::test]
async fn a_retired_session_publishes_its_exit_to_the_mirror() {
    let task = onlyne_proto::new_task_id();
    let mut cluster = Cluster::default();
    cluster
        .rows
        .push(Row::new("msg-retired", &task, LedgerState::Queued, 0));
    let fixture = fixture(cluster, |init| init.with_reconnect_grace_secs(1)).await;
    let workspace = fixture.workspace.clone();
    let client = tokio::spawn(onlyne_client::run(fixture.init.clone()));

    // An always-running agent mounts and takes the row.
    let (io, mut assigns) = mount_when_ready(&fixture.socket).await;
    let assigned = tokio::time::timeout(Duration::from_secs(10), assigns.recv())
        .await
        .expect("the queued row reaches the agent")
        .expect("the plugin connection stays open");
    assert_eq!(assigned, task, "the agent takes the row");
    // One beat says the agent is working, and the client republishes the session's
    // projection the way it does for every beat it takes: the mirror now reads the
    // session `working`, the one reading the server's own observer can fault about.
    let beat_seq = 10;
    let witnessed = onlyne_session::Observation::build(
        onlyne_session::Version::new(1, beat_seq),
        true,
        onlyne_session::DEFAULT_ISOLATE_AFTER,
        onlyne_session::DEFAULT_TERMINATE_AFTER,
        0,
        onlyne_session::AgentState::Running,
        onlyne_session::DeliveryState::None,
        onlyne_session::ResourceState::Attached,
        onlyne_session::RecoveryState::None,
    );
    let beat = io
        .request(AdapterMsg::Plugin(PluginOp::Report(plugin_beat(
            &task,
            1,
            beat_seq,
            serde_json::to_value(&witnessed).unwrap(),
        ))))
        .await
        .expect("the beat is answered");
    assert!(beat.ok, "the client applies the agent's beat: {beat:?}");
    let working = fixture.cluster.clone();
    let reported = task.clone();
    eventually(
        move || {
            working
                .lock()
                .mirrored(&reported)
                .is_some_and(|row| row.lifecycle == Lifecycle::Working)
        },
        "the working session's projection reaches the mirror",
    )
    .await;

    // The connection dies without a `detach`, and the window runs from there.
    drop(io);
    let buried = fixture.cluster.clone();
    eventually(
        move || buried.lock().row("msg-retired").state == LedgerState::Rejected,
        "the dead session's delivery row is refused rather than left in flight",
    )
    .await;
    let published = fixture.cluster.clone();
    let exited = task.clone();
    eventually(
        move || {
            published
                .lock()
                .mirrored(&exited)
                .is_some_and(|row| row.lifecycle == Lifecycle::Exited)
        },
        "the retired session's exit reaches the mirror",
    )
    .await;

    let cluster = fixture.cluster.lock();
    let row = cluster.row("msg-retired");
    assert_eq!(
        row.reason.as_deref(),
        Some(onlyne_client::session::dispatch::SESSION_DEAD),
        "the delivery row the sweep filed names the death: {row:?}",
    );
    let mirrored = cluster
        .mirrored(&task)
        .expect("the mirror holds the session the client published");
    assert_eq!(
        mirrored.lifecycle,
        Lifecycle::Exited,
        "the mirror reads the exit the client published, with no observer running: {mirrored:?}"
    );
    drop(cluster);

    // The role's own account carries the ending the mirror now shows.
    let store = ClientStore::open(workspace.join(".onlyne/client.db")).unwrap();
    let record = store.task(&task).unwrap().expect("the task has a record");
    assert_eq!(record.task_state, TaskState::Failed, "{record:?}");
    assert!(record.settled_at.is_some(), "{record:?}");

    client.abort();
    fixture.server.abort();
}

/// A send that cannot leave queues its frame durably, and leaves the link-level
/// accept gate alone.
///
/// `send_frame` tries the live outbox first and falls back to the intent table, and
/// the fallback says one thing only: this frame did not go out. A request that gave
/// up with the link still up is a real shape — the wire's deadline belongs to the
/// caller, and `onlyne_net::conn` records that "the silent peer keeps the link up;
/// only this call gave up" — and a client that read it as a dead link closed the
/// shared gate for good. Nothing re-opens that gate but a readiness transition
/// (`runloop::link`), so the pull loop stopped draining the role's inbox for the
/// life of that link: the work sat queued on the server and nothing took it. A
/// delivery the role still had in hand then owed the server no answer either —
/// `accept_delivery` leaves a row the gate kept out in flight — so the latch's
/// damage was the stall itself, long enough to outlive the operator's patience.
#[tokio::test]
async fn a_send_that_cannot_leave_does_not_shut_the_accept_gate() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        1,
        Arc::new(FakeBackend::new()),
        store.clone(),
    );
    state.attach_outbox(Arc::new(SilentOutbox));
    let gate = state.accept_new();
    assert!(
        gate.load(Ordering::SeqCst),
        "a fresh client accepts new work"
    );

    send_frame(
        &state,
        ClientOp::Report(Report::Ready {
            task_id: "task-1".into(),
            session_id: "task-1".into(),
            generation: 1,
            seq: 1,
            cluster_ref: None,
        }),
    )
    .await
    .expect("the durable path takes the frame");

    assert!(
        gate.load(Ordering::SeqCst),
        "a send that gave up is not a link that went down; the runloop owns this gate"
    );
    assert_eq!(
        store.flush_order().unwrap().len(),
        1,
        "the frame waits in the intent table for the flusher"
    );
}

/// An outbox that gives up the way the wire does when the peer goes silent: the
/// caller's own deadline fires and the connection stays `Ready`.
struct SilentOutbox;

impl Outbox for SilentOutbox {
    fn send(
        &self,
        _op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>> {
        Box::pin(async { Err(NetError::RequestTimeout) })
    }

    fn request(
        &self,
        _op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<ResBody, NetError>> + Send + '_>> {
        Box::pin(async { Err(NetError::RequestTimeout) })
    }
}
