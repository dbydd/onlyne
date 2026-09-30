//! Onlyne adapter conformance fixtures.

pub mod harness;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use chrono::Utc;
use onlyne_adapter::{
    AdapterClient, AdapterIo, AdapterServer, AgentHandle, Host, HostDispatcher, IncomingFrame,
    accept_report_generation, degrade_for,
};
use onlyne_proto::adapter::HandoffArgs;
use onlyne_proto::{
    AdapterMsg, AgentMount, AssignAckArgs, AssignArgs, Body, Capability, Causality, Delivery,
    DetachArgs, Envelope, ErrorCode, HealthArgs, HelloAck, HelloArgs, HostOp, IMAGE_DATA_MAX_BYTES,
    LedgerState, Mount, MsgKind, OpenArgs, Outcome, PROTOCOL_VERSION, Principal, Receipt,
    RegisterChannelArgs, RenderSendArgs, Report, ResBody, ServerInfo, SessionRegisterArgs,
    TypingArgs, new_envelope, new_id, new_op_id, new_task_id,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct HostSimSpec {
    pub role: String,
    pub prose: String,
    pub expected_capabilities: Vec<Capability>,
    pub scripted: Vec<HostOp>,
}

impl HostSimSpec {
    pub fn agent(
        role: impl Into<String>,
        prose: impl Into<String>,
        expected_capabilities: Vec<Capability>,
    ) -> Self {
        HostSimSpec {
            role: role.into(),
            prose: prose.into(),
            expected_capabilities,
            scripted: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecordedFrame {
    pub op: String,
    pub value: Value,
}

#[derive(Default)]
struct HostSimState {
    io: Option<AdapterIo>,
    recorded: Vec<RecordedFrame>,
    faults: Vec<Report>,
    watermark: (u64, u64),
    ready_tasks: HashSet<String>,
    pending_assigns: HashMap<String, AssignArgs>,
    receipts: HashMap<String, (String, Receipt)>,
    recovery: Option<String>,
    missing_capabilities: Vec<Capability>,
    /// Every session this runtime was asked to open, in order. A hosting test
    /// reads it to say the client asked once rather than twice.
    opened: Vec<String>,
}

pub struct HostSim {
    spec: HostSimSpec,
    state: Mutex<HostSimState>,
}

impl HostSim {
    pub fn new(spec: HostSimSpec) -> Arc<Self> {
        Arc::new(HostSim {
            spec,
            state: Mutex::new(HostSimState {
                watermark: (1, 0),
                ..HostSimState::default()
            }),
        })
    }

    pub fn pair(
        spec: HostSimSpec,
    ) -> (
        Arc<Self>,
        AgentHandle,
        JoinHandle<onlyne_adapter::Result<()>>,
    ) {
        let sim = Self::new(spec);
        let (agent, task) = sim.clone().connect_agent();
        (sim, agent, task)
    }

    pub fn connect_agent(self: Arc<Self>) -> (AgentHandle, JoinHandle<onlyne_adapter::Result<()>>) {
        let (client, server) = tokio::io::duplex(16 * 1024 * 1024);
        let agent = AdapterClient::connect_with_timeouts(
            client,
            Duration::from_secs(5),
            Duration::from_secs(5),
        );
        let sim = self.clone();
        let task = tokio::spawn(async move { sim.serve_stream(server).await });
        (agent, task)
    }

    pub fn connect_gateway(
        self: Arc<Self>,
    ) -> (
        onlyne_adapter::GatewayHandle,
        JoinHandle<onlyne_adapter::Result<()>>,
    ) {
        let (client, server) = tokio::io::duplex(16 * 1024 * 1024);
        let gateway = AdapterClient::gateway_with_timeouts(
            client,
            Duration::from_secs(5),
            Duration::from_secs(5),
        );
        let sim = self.clone();
        let task = tokio::spawn(async move { sim.serve_stream(server).await });
        (gateway, task)
    }

    async fn serve_stream<S>(self: Arc<Self>, stream: S) -> onlyne_adapter::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let sim = self.clone();
        let accepted = AdapterServer::accept_async(stream, move |hello| {
            let sim = sim.clone();
            async move { sim.hello(&hello).await }
        })
        .await?;
        self.install_io(accepted.io.clone()).await;
        self.emit_scripted()
            .await
            .map_err(|err| onlyne_adapter::AdapterError::Unexpected(err.to_string()))?;
        self.emit_ready_assigns()
            .await
            .map_err(|err| onlyne_adapter::AdapterError::Unexpected(err.to_string()))?;
        let dispatcher = HostDispatcher::new(accepted.hello.kind, self.clone());
        dispatcher.serve(accepted.io, accepted.inbound).await
    }

    async fn install_io(&self, io: AdapterIo) {
        self.state.lock().await.io = Some(io);
    }

    pub async fn recorded(&self) -> Vec<RecordedFrame> {
        self.state.lock().await.recorded.clone()
    }

    pub async fn faults(&self) -> Vec<Report> {
        self.state.lock().await.faults.clone()
    }

    pub async fn recovery(&self) -> Option<String> {
        self.state.lock().await.recovery.clone()
    }

    pub async fn watermark(&self) -> (u64, u64) {
        self.state.lock().await.watermark
    }

    pub async fn set_watermark(&self, watermark: (u64, u64)) {
        self.state.lock().await.watermark = watermark;
    }

    /// The sessions this runtime was asked to open, in order.
    pub async fn opened_sessions(&self) -> Vec<String> {
        self.state.lock().await.opened.clone()
    }

    pub async fn missing_capabilities(&self) -> Vec<Capability> {
        self.state.lock().await.missing_capabilities.clone()
    }

    pub async fn queue_assign(&self, assign: AssignArgs) -> Result<()> {
        let task_id = assign.task_id.clone();
        let emit = {
            let mut state = self.state.lock().await;
            state
                .pending_assigns
                .insert(task_id.clone(), assign.clone());
            if state.ready_tasks.contains(&task_id) {
                state.io.clone().map(|io| (io, assign))
            } else {
                None
            }
        };
        if let Some((io, assign)) = emit {
            io.notify(AdapterMsg::Host(HostOp::Assign(assign))).await?;
        }
        Ok(())
    }

    pub async fn emit_host(&self, op: HostOp) -> Result<()> {
        let io = self
            .state
            .lock()
            .await
            .io
            .clone()
            .ok_or_else(|| anyhow!("host sim has no active connection"))?;
        io.notify(AdapterMsg::Host(op)).await?;
        Ok(())
    }

    async fn emit_scripted(&self) -> Result<()> {
        for op in self.spec.scripted.clone() {
            self.emit_host(op).await?;
        }
        Ok(())
    }

    async fn emit_ready_assigns(&self) -> Result<()> {
        let emits = {
            let state = self.state.lock().await;
            let Some(io) = state.io.clone() else {
                return Ok(());
            };
            state
                .pending_assigns
                .values()
                .filter(|assign| state.ready_tasks.contains(&assign.task_id))
                .cloned()
                .map(|assign| (io.clone(), assign))
                .collect::<Vec<_>>()
        };
        for (io, assign) in emits {
            io.notify(AdapterMsg::Host(HostOp::Assign(assign))).await?;
        }
        Ok(())
    }

    pub async fn handle_missing_recycle(
        &self,
        task_id: impl Into<String>,
        timeout: Duration,
    ) -> Result<()> {
        let task_id = task_id.into();
        let missing = self
            .state
            .lock()
            .await
            .missing_capabilities
            .contains(&Capability::Recycle);
        if missing {
            self.probe_with_timeout(task_id, timeout).await
        } else {
            self.emit_host(HostOp::Recycle(onlyne_proto::RecycleArgs {
                task_id,
                reason: "host recycle".to_string(),
                outcome: Some(Outcome::Cancelled),
            }))
            .await
        }
    }

    pub async fn probe_with_timeout(&self, task_id: String, timeout: Duration) -> Result<()> {
        let before = self.recorded().await.len();
        self.emit_host(HostOp::Probe(json!({ "task_id": task_id })))
            .await?;
        tokio::time::sleep(timeout).await;
        let answered = self
            .recorded()
            .await
            .into_iter()
            .skip(before)
            .any(|frame| frame.op == "report" && frame.value.to_string().contains("heartbeat"));
        if !answered {
            let fault = Report::Fault {
                task_id: Some(task_id),
                session_id: Some("sim-session".to_string()),
                generation: Some(self.watermark().await.0),
                seq: Some(self.watermark().await.1 + 1),
                kind: "probe_timeout".to_string(),
                reason: "probe unanswered".to_string(),
                desired: None,
                observed: None,
            };
            self.state.lock().await.faults.push(fault);
        }
        Ok(())
    }

    fn welcome_for(&self, args: &HelloArgs) -> HelloAck {
        let role = match &args.mount {
            Some(Mount::Agent(AgentMount { role, .. })) => role.clone(),
            _ => self.spec.role.clone(),
        };
        HelloAck {
            protocol: PROTOCOL_VERSION,
            role,
            session_id: Some("sim-session".to_string()),
            generation: 1,
            prose: self.spec.prose.clone(),
            server: ServerInfo {
                connected: true,
                cluster: "sim".to_string(),
                name: "host-sim".to_string(),
            },
            host_capabilities: self.spec.expected_capabilities.clone(),
            delivered_tasks: Vec::new(),
        }
    }

    async fn record(&self, op: impl Into<String>, value: Value) {
        self.state.lock().await.recorded.push(RecordedFrame {
            op: op.into(),
            value,
        });
    }
}

#[async_trait]
impl Host for HostSim {
    async fn hello(&self, args: &HelloArgs) -> std::result::Result<HelloAck, (ErrorCode, String)> {
        self.record("hello", serde_json::to_value(args).unwrap_or(Value::Null))
            .await;
        let missing = args.missing(&self.spec.expected_capabilities);
        let mut state = self.state.lock().await;
        state.missing_capabilities = missing.clone();
        for gap in degrade_for(&missing) {
            if gap.capability == Capability::Report {
                state.recovery = Some("idle_fault".to_string());
                let watermark = state.watermark;
                state.faults.push(Report::Fault {
                    task_id: None,
                    session_id: Some("sim-session".to_string()),
                    generation: Some(watermark.0),
                    seq: Some(watermark.1 + 1),
                    kind: "capability_gap".to_string(),
                    reason: gap.action.to_string(),
                    desired: Some(json!({ "capability": gap.capability.as_str() })),
                    observed: None,
                });
            }
        }
        Ok(self.welcome_for(args))
    }

    async fn report(&self, report: &Report) -> std::result::Result<(), (ErrorCode, String)> {
        self.record(
            "report",
            serde_json::to_value(report).unwrap_or(Value::Null),
        )
        .await;
        let emit = {
            let mut state = self.state.lock().await;
            if let Some(version) = report.version() {
                if !accept_report_generation(state.watermark, version) {
                    return Err((ErrorCode::Conflict, "stale report generation".to_string()));
                }
                state.watermark = version;
            }
            if let Report::Ready { task_id, .. } = report {
                state.ready_tasks.insert(task_id.clone());
                state
                    .pending_assigns
                    .get(task_id)
                    .cloned()
                    .and_then(|assign| state.io.clone().map(|io| (io, assign)))
            } else {
                if matches!(report, Report::Fault { .. }) {
                    state.faults.push(report.clone());
                }
                None
            }
        };
        if let Some((io, assign)) = emit {
            io.notify(AdapterMsg::Host(HostOp::Assign(assign)))
                .await
                .map_err(|err| (ErrorCode::Internal, err.to_string()))?;
        }
        Ok(())
    }

    async fn session_register(
        &self,
        args: &SessionRegisterArgs,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        self.record(
            "session_register",
            serde_json::to_value(args).unwrap_or(Value::Null),
        )
        .await;
        Ok(())
    }

    /// Open a session inside the process that is already running, and answer the
    /// client with a name and a handle a later open can hand back.
    ///
    /// The handle is this fake's own word for where the conversation is, which is
    /// the whole point: the client stores it unread and gives it back. A runtime
    /// that resumes by handle is the `task` scope; one that answers without one
    /// is a fresh conversation every time, and the client must not paper over the
    /// difference with a summary of its own.
    async fn open_session(
        &self,
        args: &OpenArgs,
    ) -> std::result::Result<Value, (ErrorCode, String)> {
        self.record("open", serde_json::to_value(args).unwrap_or(Value::Null))
            .await;
        self.state.lock().await.opened.push(args.session_id.clone());
        let handle = match args.resume_handle.as_deref() {
            Some(handle) => handle.to_string(),
            None => format!("sim-conv-{}", self.state.lock().await.opened.len()),
        };
        Ok(json!({
            "session_id": args.session_id,
            "conversation": handle,
            "resume_handle": handle,
        }))
    }

    async fn assign_ack(
        &self,
        ack: &AssignAckArgs,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        self.record(
            "assign_ack",
            serde_json::to_value(ack).unwrap_or(Value::Null),
        )
        .await;
        Ok(())
    }

    async fn send(&self, envelope: &Envelope) -> std::result::Result<Receipt, (ErrorCode, String)> {
        self.record(
            "send",
            serde_json::to_value(envelope).unwrap_or(Value::Null),
        )
        .await;
        if let Err(err) = envelope.validate() {
            return Err((ErrorCode::Invalid, err.message().to_string()));
        }
        let fingerprint = envelope.fingerprint();
        let mut state = self.state.lock().await;
        if let Some(op_id) = &envelope.op_id {
            if let Some((seen_fingerprint, receipt)) = state.receipts.get(op_id) {
                if seen_fingerprint == &fingerprint {
                    return Ok(receipt.clone());
                }
                return Err((
                    ErrorCode::Conflict,
                    onlyne_proto::OP_ID_CONFLICT_MESSAGE.to_string(),
                ));
            }
        }
        let receipt = Receipt {
            msg_id: envelope.id.clone(),
            op_id: envelope.op_id.clone(),
            kind: envelope.kind,
            task: envelope.task_id().map(str::to_string),
            state: LedgerState::Acked,
            enqueued_at: Utc::now(),
        };
        if let Some(op_id) = &envelope.op_id {
            state
                .receipts
                .insert(op_id.clone(), (fingerprint, receipt.clone()));
        }
        Ok(receipt)
    }

    async fn deliver(&self, delivery: &Delivery) -> std::result::Result<(), (ErrorCode, String)> {
        self.record(
            "deliver",
            serde_json::to_value(delivery).unwrap_or(Value::Null),
        )
        .await;
        Ok(())
    }

    async fn register_channel(
        &self,
        args: &RegisterChannelArgs,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        self.record(
            "register_channel",
            serde_json::to_value(args).unwrap_or(Value::Null),
        )
        .await;
        Ok(())
    }

    async fn health(&self, args: &HealthArgs) -> std::result::Result<(), (ErrorCode, String)> {
        self.record("health", serde_json::to_value(args).unwrap_or(Value::Null))
            .await;
        Ok(())
    }

    async fn typing(&self, args: &TypingArgs) -> std::result::Result<(), (ErrorCode, String)> {
        self.record("typing", serde_json::to_value(args).unwrap_or(Value::Null))
            .await;
        Ok(())
    }

    async fn detach(&self, args: &DetachArgs) -> std::result::Result<(), (ErrorCode, String)> {
        self.record("detach", serde_json::to_value(args).unwrap_or(Value::Null))
            .await;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentScript {
    #[serde(default)]
    pub hello: ScriptHello,
    #[serde(default)]
    pub steps: Vec<Value>,
    /// Serve every assign that arrives instead of just the first.
    ///
    /// A script without this flag runs its steps once, which is all a case that
    /// sends one task needs. An agent that stays mounted and takes the next
    /// task — the running-lights ring hands the same role two of them — sets it
    /// and loops back to `wait_assign` after the last step.
    #[serde(default)]
    pub repeat: bool,
}

impl AgentScript {
    pub fn from_reader<R: std::io::Read>(reader: R) -> Result<Self> {
        serde_json::from_reader(reader).context("read fake agent script")
    }

    pub fn from_json_str(src: &str) -> Result<Self> {
        serde_json::from_str(src).context("parse fake agent script")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ScriptHello {
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

pub struct FakeAgent {
    pub role: String,
    pub capabilities: Vec<Capability>,
    pub script: AgentScript,
    pub workspace: PathBuf,
    /// Every session the host asked this runtime to open, in order.
    ///
    /// The reader writes it when it answers an `open`; a `require_opened` step
    /// reads it. Without the shared ledger a script could only see the wait for
    /// an assign, which a parked connection serves too — so a scenario could not
    /// tell an asked-for session from a handed-over one.
    pub opened: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl FakeAgent {
    pub fn new(
        role: impl Into<String>,
        capabilities: Vec<Capability>,
        script: AgentScript,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        FakeAgent {
            role: role.into(),
            capabilities,
            script,
            workspace: workspace.into(),
            opened: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    pub async fn run(&self, handle: &AgentHandle) -> Result<()> {
        let capabilities = if self.script.hello.capabilities.is_empty() {
            self.capabilities.clone()
        } else {
            self.script.hello.capabilities.clone()
        };
        let welcome = handle
            .hello_role(self.role.clone(), "onlyne-agent-fake", capabilities)
            .await
            .context("fake agent hello")?;
        let mut state = FakeAgentPhase {
            welcome,
            last_assign: None,
            beats: 0,
            turn_reported: false,
        };
        loop {
            for step in &self.script.steps {
                self.run_step(handle, &mut state, step).await?;
            }
            if !self.script.repeat {
                break;
            }
        }
        Ok(())
    }

    async fn run_step(
        &self,
        handle: &AgentHandle,
        state: &mut FakeAgentPhase,
        step: &Value,
    ) -> Result<()> {
        let object = step
            .as_object()
            .ok_or_else(|| anyhow!("script step must be an object"))?;
        let Some((name, value)) = object.iter().next() else {
            bail!("unknown step: empty");
        };
        match name.as_str() {
            "wait_assign" => {
                if value.as_bool() != Some(true) {
                    bail!("wait_assign must be true");
                }
                state.last_assign = Some(
                    wait_for_assign(handle, &self.opened)
                        .await
                        .context("wait assign")?,
                );
                // Each assignment opens its own turn, and the script's next one
                // has to report a beat before it may complete: see `complete`.
                state.turn_reported = false;
            }
            "mark_mounted" => {
                // A file the case can wait on. `wait_role_online` answers for the
                // client's server link, which says nothing about whether a plugin
                // has mounted, and a delivery sent in that gap is staged before
                // the runtime exists to be asked — which reads as a hosting
                // failure and is not one.
                let file = value
                    .as_str()
                    .ok_or_else(|| anyhow!("mark_mounted requires a path"))?;
                let path = path_in_workspace(&self.workspace, file);
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await.ok();
                }
                tokio::fs::write(&path, b"mounted").await?;
            }
            "require_opened" => {
                // The client asks a standing runtime for its session instead of
                // handing the next staged one to whoever mounted. A script that
                // needs the ask says so here, because the wait below is not a
                // witness for it: a parked connection is handed a session too.
                let opened = self.opened.lock().await.clone();
                if opened.is_empty() {
                    bail!(
                        "the client never asked this runtime for a session: it was \
                         handed one instead, so the mount was treated as parked"
                    );
                }
            }
            "report" => {
                let kind = value
                    .as_str()
                    .ok_or_else(|| anyhow!("report step requires a string"))?;
                let assign = state
                    .last_assign
                    .as_ref()
                    .ok_or_else(|| anyhow!("report requires assign"))?;
                match kind {
                    "ready" => {
                        handle
                            .report_ready(assign.task_id.clone(), "sim-session")
                            .await?;
                    }
                    "heartbeat" | "idle" => {
                        state.beats += 1;
                        let running = kind == "heartbeat";
                        // A running beat is also the turn: its `agent: running`
                        // moves the session past `ready`, and a completion
                        // before one is refused (`SETTLE_WITHOUT_TURN`). An
                        // idle beat is the runtime's post-turn at-rest report.
                        if running {
                            state.turn_reported = true;
                        }
                        // The shape a real plugin reports: a whole `Observation`,
                        // whose only optional field is the host binding. A beat that
                        // omits the dimensions cannot deserialize in the client, and
                        // the client then treats it as liveness alone — a suite whose
                        // beats all take that door never reaches the write the reducer
                        // runs on a readable tuple, which is the path a live agent's
                        // unchanged `running` beat travels every ten seconds. The
                        // sequence base matches the pi plugin's `SEQ_BASE`, and it
                        // belongs on the frame: the client stamps a beat with the
                        // reporter's own `(generation, seq)` and overwrites the version
                        // inside `observed`, so a frame numbered from the sender's
                        // counter — which starts at one, below the dispatch events the
                        // client already wrote — is dropped as a duplicate and teaches
                        // the suite nothing. `report_heartbeat` allocates that low
                        // number, so the frame is built here.
                        let sender = handle.report_sender();
                        sender
                            .send(Report::Heartbeat {
                                task_id: assign.task_id.clone(),
                                session_id: String::new(),
                                generation: sender.generation(),
                                seq: SEQ_BASE + state.beats,
                                observed: json!({
                                    "version": { "generation": 1, "seq": SEQ_BASE + state.beats },
                                    "generation_live": true,
                                    "isolate_after": 1,
                                    "terminate_after": 3,
                                    "mismatch_count": 0,
                                    "agent": if running { "running" } else { "idle" },
                                    "delivery": "none",
                                    "resource": "attached",
                                    "recovery": "none",
                                }),
                                projection: None,
                                cluster_ref: None,
                            })
                            .await?;
                    }
                    other => bail!("unknown step: report.{other}"),
                }
            }
            "complete" => {
                let assign = state
                    .last_assign
                    .as_ref()
                    .ok_or_else(|| anyhow!("complete requires assign"))?;
                if !state.turn_reported {
                    bail!(
                        "complete needs a turn this script reported: the client records a turn \
                         only from a heartbeat whose agent phase reads `running`, and a \
                         completion for a session that never ran one is refused whole \
                         (`settle_without_turn`) while the task stays open. Add a \
                         {{\"report\": \"heartbeat\"}} step before this one."
                    );
                }
                let outcome = value
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("done");
                let outcome = parse_outcome(outcome)?;
                let head = match value.get("head_from").and_then(Value::as_str) {
                    Some("assign_body") => assign.envelope.body.text.clone(),
                    Some(other) => bail!("unknown step: complete.head_from.{other}"),
                    None => value
                        .get("head")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                };
                let details = value
                    .get("details")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let files = match value.get("files") {
                    Some(Value::Array(files)) => files
                        .iter()
                        .map(|file| {
                            file.as_str()
                                .map(str::to_string)
                                .ok_or_else(|| anyhow!("unknown step: complete.files.{file}"))
                        })
                        .collect::<anyhow::Result<Vec<String>>>()?,
                    Some(_) => bail!("unknown step: complete.files"),
                    None => Vec::new(),
                };
                handle
                    .report_complete(assign.task_id.clone(), outcome, head, details, files)
                    .await?;
            }
            "fail" => {
                let reason = value.as_str().unwrap_or("fake agent failure").to_string();
                let task_id = state
                    .last_assign
                    .as_ref()
                    .map(|assign| assign.task_id.clone());
                handle.report_fault(task_id, "fake_agent", reason).await?;
            }
            "exit" => {
                let reason = value.as_str().unwrap_or("script exit");
                handle.exit(reason).await?;
            }
            "sleep_ms" => {
                let ms = value
                    .as_u64()
                    .ok_or_else(|| anyhow!("sleep_ms requires a number"))?;
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            "assert_prose_equals" => {
                let expected = value
                    .as_str()
                    .ok_or_else(|| anyhow!("assert_prose_equals requires a string"))?;
                let actual = state
                    .last_assign
                    .as_ref()
                    .map(|assign| assign.prose.as_str())
                    .unwrap_or(state.welcome.prose.as_str());
                if actual != expected {
                    bail!("assert_prose_equals failed: expected {expected:?}, got {actual:?}");
                }
            }
            "assert_field" => {
                let path = value
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("assert_field.path required"))?;
                let expected = value
                    .get("equals")
                    .ok_or_else(|| anyhow!("assert_field.equals required"))?;
                let state_value = self.state_value(state)?;
                let actual = value_path(&state_value, path)
                    .ok_or_else(|| anyhow!("assert_field missing path {path}"))?;
                if actual != expected {
                    bail!("assert_field {path} failed: expected {expected}, got {actual}");
                }
            }
            "handoff" => {
                let assign = state
                    .last_assign
                    .as_ref()
                    .ok_or_else(|| anyhow!("handoff requires assign"))?;
                let step = HandoffStep::parse(value)?;
                let hop = assign_hop(assign);
                if matches!(step.max_hop, Some(max) if hop >= max) {
                    return Ok(());
                }
                let values = self.template_values(assign);
                let to = expand_placeholders(&step.to, &values)?;
                let text = expand_placeholders(&step.text, &values)?;
                // A role hands work on through its plugin's own tool, so the
                // fixture sends the adapter protocol's `handoff` op and the host
                // mints the child. Driving the `onlyne handoff` CLI here would
                // exercise a role-side verb v2 deletes, along with the two
                // supervisor flags it needed to pass itself off as one.
                handle
                    .handoff(HandoffArgs {
                        task_id: assign.task_id.clone(),
                        to: to.clone(),
                        text,
                        image: None,
                    })
                    .await
                    .with_context(|| format!("hand off {} to {to}", assign.task_id))?;
            }
            "echo_field_to" => {
                let path_field = value
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("echo_field_to.path required"))?;
                let file = value
                    .get("file")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("echo_field_to.file required"))?;
                let state_value = self.state_value(state)?;
                let found = value_path(&state_value, path_field)
                    .ok_or_else(|| anyhow!("echo_field_to missing path {path_field}"))?;
                let line = match found {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                let path = path_in_workspace(&self.workspace, file);
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let mut written = tokio::fs::read_to_string(&path).await.unwrap_or_default();
                written.push_str(&line);
                written.push('\n');
                tokio::fs::write(path, written).await?;
            }
            "echo_prose_to" => {
                let file = value
                    .as_str()
                    .ok_or_else(|| anyhow!("echo_prose_to requires a path"))?;
                let prose = state
                    .last_assign
                    .as_ref()
                    .map(|assign| assign.prose.as_str())
                    .unwrap_or(state.welcome.prose.as_str());
                let path = path_in_workspace(&self.workspace, file);
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(path, prose).await?;
            }
            other => bail!("unknown step: {other}"),
        }
        Ok(())
    }

    fn state_value(&self, state: &FakeAgentPhase) -> Result<Value> {
        Ok(json!({
            "welcome": state.welcome,
            "assign": state.last_assign,
        }))
    }

    /// Values a `handoff` template may name, read off the incoming assign.
    ///
    /// `next_role` is the one value the script cannot name itself: it describes
    /// the ring the agent sits in, not the agent, so it arrives in the spawn
    /// environment beside the workspace the agent was started with.
    fn template_values(&self, assign: &AssignArgs) -> BTreeMap<&'static str, String> {
        let hop = assign_hop(assign);
        let mut values = BTreeMap::new();
        values.insert("role", self.role.clone());
        values.insert("workspace", self.workspace.display().to_string());
        values.insert("task", assign.task_id.clone());
        values.insert("hop", hop.to_string());
        values.insert("next_hop", (hop + 1).to_string());
        if let Ok(next) = std::env::var(NEXT_ROLE_ENV) {
            values.insert("next_role", next);
        }
        values
    }
}

/// The sequence a fake agent's beats start above, matching the pi plugin's
/// `SEQ_BASE`. The client stamps a beat with the reporter's own sequence, so a
/// fake starting at one would have its first beats refused as nothing newer than
/// the row the dispatch path already wrote, and the beat would teach the suite
/// nothing about the accepted path.
const SEQ_BASE: u64 = 1000;

struct FakeAgentPhase {
    welcome: HelloAck,
    last_assign: Option<AssignArgs>,
    /// Beats this agent has reported, counting from one. The sequence it puts on
    /// a beat rides on top of this, so each beat is newer than the last and the
    /// client reads it as a fresh frame rather than a replay.
    beats: u64,
    /// Whether the assignment in hand has had the beat that opens its turn.
    /// `complete` reads it, because the host refuses a completion for a session
    /// whose agent phase never left `ready`.
    turn_reported: bool,
}

fn value_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Wait for the host's `assign` frame, and refuse to wait for one that is not
/// coming.
///
/// `inject` is the capability that makes a plugin reachable by `assign`; a
/// plugin that mounts without it is handed its task through the plugin's own
/// stdin, which on this socket is a `config_get` frame whose only key is
/// `stdin:{task text}` (`crates/onlyne-adapter/PROTOCOL.md`, "Mounts and
/// capabilities"). A step that waits for an `assign` anyway waits out the
/// scenario's whole timeout and reports nothing about why, which is how three
/// scripts kept a capability set their own steps could never work under. The
/// frame is the answer, so the fixture names it here.
async fn wait_for_assign(
    handle: &AgentHandle,
    opened: &std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
) -> Result<AssignArgs> {
    loop {
        match handle.next_host_frame().await? {
            // The host asked for a session because it had none to hand over. A
            // runtime that answers here is the reason a session exists at all:
            // naming the conversation is what lets the same family find it again.
            (Some(reply_to), HostOp::Open(args)) => {
                let mut log = opened.lock().await;
                log.push(args.session_id.clone());
                let conversation = format!("sim-conv-{}", log.len());
                handle
                    .answer_open(reply_to, &args.session_id, &conversation)
                    .await?;
            }
            (_, HostOp::Assign(assign)) => return Ok(assign),
            (_, HostOp::ConfigGet(args)) if args.key.starts_with("stdin:") => bail!(
                "the host delivered the task through this plugin's stdin ({}), so no `assign` \
                 will arrive: the script's hello must declare the `inject` capability",
                args.key
            ),
            (_, HostOp::Bye(bye)) => {
                bail!("the host said goodbye before an assign: {}", bye.reason)
            }
            _ => {}
        }
    }
}

fn path_in_workspace(workspace: &Path, file: &str) -> PathBuf {
    let path = PathBuf::from(file);
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}

/// Environment variable naming the role a `handoff` step passes the task to.
pub const NEXT_ROLE_ENV: &str = "ONLYNE_NEXT_ROLE";
/// One `handoff` step: where the incoming task goes next, and how deep the
/// chain may run before the agent keeps the task instead of passing it on.
///
/// `to` and `text` carry `{name}` placeholders (`role`, `task`, `hop`,
/// `next_hop`, `next_role`, `workspace`). The step sends the adapter protocol's
/// `handoff` op, so the fixture exercises the path a real plugin takes — the host
/// reads the parent row, mints the child, and carries the family's figures —
/// rather than building a second envelope itself.
#[derive(Debug, Clone)]
struct HandoffStep {
    to: String,
    text: String,
    /// Highest hop that still passes the task on. Absent means every hop does.
    max_hop: Option<u32>,
}

impl HandoffStep {
    fn parse(value: &Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("handoff step requires an object"))?;
        for key in object.keys() {
            if !matches!(key.as_str(), "to" | "text" | "max_hop") {
                bail!("unknown handoff field: {key}");
            }
        }
        let field = |name: &str| -> Result<&str> {
            object
                .get(name)
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("handoff.{name} requires a string"))
        };
        let max_hop = match object.get("max_hop") {
            Some(value) => {
                let hop = value
                    .as_u64()
                    .ok_or_else(|| anyhow!("handoff.max_hop requires an integer"))?;
                Some(u32::try_from(hop).context("handoff.max_hop exceeds u32")?)
            }
            None => None,
        };
        Ok(Self {
            to: field("to")?.to_string(),
            text: field("text")?.to_string(),
            max_hop,
        })
    }
}

/// Hop count the incoming assign carries, from the envelope's causality.
fn assign_hop(assign: &AssignArgs) -> u32 {
    assign
        .envelope
        .causality
        .as_ref()
        .map(|causality| causality.hop)
        .unwrap_or(0)
}

/// Fill every `{name}` in a `handoff` template from `values`.
///
/// A name with no value stops the step instead of reaching the CLI: the text
/// is an argument to a real process, so a typo has to fail before it runs.
fn expand_placeholders(template: &str, values: &BTreeMap<&'static str, String>) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let end = tail
            .find('}')
            .ok_or_else(|| anyhow!("handoff template {template:?} has an unclosed placeholder"))?;
        let name = &tail[..end];
        let value = values.get(name).ok_or_else(|| {
            if name == "next_role" {
                anyhow!("handoff names {{next_role}}, which needs {NEXT_ROLE_ENV} set")
            } else {
                anyhow!("unknown handoff placeholder {{{name}}}")
            }
        })?;
        out.push_str(value);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn parse_outcome(value: &str) -> Result<Outcome> {
    match value {
        "done" => Ok(Outcome::Done),
        "failed" => Ok(Outcome::Failed),
        "cancelled" => Ok(Outcome::Cancelled),
        other => bail!("unknown outcome: {other}"),
    }
}

#[derive(Debug, Clone)]
pub struct FakeGateway {
    pub platform: String,
    pub gateway_id: String,
}

impl FakeGateway {
    pub fn new(platform: impl Into<String>, gateway_id: impl Into<String>) -> Self {
        FakeGateway {
            platform: platform.into(),
            gateway_id: gateway_id.into(),
        }
    }

    pub fn render_line(args: &RenderSendArgs) -> Value {
        json!({
            "op": "rendered",
            "conversation": args.conversation,
            "text": args.envelope.body.text.clone().unwrap_or_default(),
            "has_image": args.envelope.body.image.is_some(),
        })
    }

    /// One inbound platform message, shaped as the work a role should do.
    ///
    /// The human's line is a `Task` with a fresh task id, because the receiving
    /// client dispatches work by `causality.task` (plan §3) and a `Note` starts
    /// no session. The target role comes from the server's `[[route]]` table,
    /// so the principal here is the deferring placeholder the plugin sends.
    pub fn inbound_delivery(
        &self,
        conversation: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Delivery> {
        let conversation = conversation.into();
        // The message starts a family of its own, and the task it mints is that
        // family's root.
        let causality = Causality::root(onlyne_proto::new_task_id());
        let envelope = new_envelope(
            MsgKind::Task,
            Principal::Gateway {
                gateway: self.gateway_id.clone(),
                channel: self.platform.clone(),
                conversation: Some(conversation),
            },
            Principal::role("unrouted"),
            Body::text(text.into()),
            Some(causality),
        )?;
        Ok(Delivery {
            msg_id: envelope.id.clone(),
            envelope: Box::new(envelope),
        })
    }

    pub async fn run_stdin_stdout<R, W>(
        &self,
        gateway: Arc<onlyne_adapter::GatewayHandle>,
        reader: R,
        mut writer: W,
    ) -> Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            let value: Value =
                serde_json::from_str(&line).context("parse fake gateway stdin line")?;
            match value.get("op").and_then(Value::as_str) {
                Some("inbound") => {
                    let conversation = value
                        .get("conversation")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("inbound conversation required"))?;
                    let text = value
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("inbound text required"))?;
                    gateway
                        .deliver_inbound(self.inbound_delivery(conversation, text)?)
                        .await?;
                }
                Some(other) => bail!("unknown gateway op: {other}"),
                None => bail!("gateway op required"),
            }
            writer.flush().await?;
        }
        Ok(())
    }
}

pub fn default_agent_capabilities() -> Vec<Capability> {
    vec![
        Capability::Register,
        Capability::Report,
        Capability::Inject,
        Capability::Recycle,
    ]
}

pub fn default_gateway_capabilities() -> Vec<Capability> {
    vec![
        Capability::Report,
        Capability::Typing,
        Capability::Conversations,
    ]
}

pub fn sample_task_envelope(text: &str) -> Envelope {
    new_envelope(
        MsgKind::Task,
        Principal::role("planner"),
        Principal::role("builder"),
        Body::text(text),
        Some(Causality::root(new_task_id())),
    )
    .expect("sample task envelope")
}

pub fn sample_assign(text: &str, prose: &str) -> AssignArgs {
    let envelope = sample_task_envelope(text);
    AssignArgs {
        task_id: envelope.task_id().unwrap_or("task").to_string(),
        generation: 1,
        prose: prose.to_string(),
        // The text a client renders for this delivery. The fixture builds the
        // template's own shape rather than a client's answer, so a case reading
        // it sees a delivery text and not an envelope body.
        text: format!("From planner:\n\n{text}"),
        attachments: Vec::new(),
        envelope: Box::new(envelope),
        // Neither field: this fixture stands for a frame from a host that
        // predates them, which is the shape a runtime must still read as its
        // only conversation and no scope at all.
        session_id: None,
        scope: None,
        parent: None,
    }
}

pub fn oversized_image_envelope(decoded_len: usize) -> Envelope {
    use base64::Engine;
    let data = vec![7_u8; decoded_len];
    Envelope {
        protocol: PROTOCOL_VERSION,
        id: new_id(),
        op_id: Some(new_op_id()),
        kind: MsgKind::Task,
        from: Principal::role("planner"),
        to: Principal::role("builder"),
        control: None,
        causality: Some(Causality::root(new_task_id())),
        body: Body {
            text: None,
            head: None,
            image: Some(onlyne_proto::ImagePart {
                data_base64: base64::engine::general_purpose::STANDARD.encode(data),
                mime: "image/png".to_string(),
                name: None,
            }),
        },
        ts: Utc::now(),
        ttl_ms: None,
        admin: false,
    }
}

pub fn empty_body_envelope() -> Envelope {
    Envelope {
        protocol: PROTOCOL_VERSION,
        id: new_id(),
        op_id: Some(new_op_id()),
        kind: MsgKind::Task,
        from: Principal::role("planner"),
        to: Principal::role("builder"),
        control: None,
        causality: Some(Causality::root(new_task_id())),
        body: Body::default(),
        ts: Utc::now(),
        ttl_ms: None,
        admin: false,
    }
}

pub fn session_backend_choice() -> String {
    "hostsim-stub: a session backend lives in onlyne-client, and a leaf crate does not take a \
     dependency on a sibling daemon"
        .to_string()
}

pub async fn write_response(io: &AdapterIo, frame: IncomingFrame, body: ResBody) -> Result<()> {
    if let Some(id) = frame.id {
        io.respond(id, body).await?;
    }
    Ok(())
}

pub fn image_limit_message() -> String {
    format!("image exceeds {IMAGE_DATA_MAX_BYTES} bytes")
}

/// The socket a role workspace serves.
///
/// A workspace whose canonical spelling fits the Unix socket bound answers with
/// `.onlyne/run/s`; a deeper one has its served path in the `.onlyne/run/socket`
/// marker its daemon published at bind, with a short derived spelling available
/// before any bind. An adapter fixture that joined `run/s` itself would dial a
/// file nothing listens on.
pub fn socket_from_workspace(workspace: &Path) -> PathBuf {
    onlyne_config::layout::RoleWorkspace::resolve(workspace).socket_path()
}

/// Role one workspace serves, read from its `.onlyne/config.toml`.
///
/// The fake agent mounts the role its workspace owns. `--role` stays available
/// as an override for a workspace whose config is not the source of the name.
pub fn role_from_workspace(workspace: &Path) -> Result<String> {
    let path = workspace.join(".onlyne").join("config.toml");
    let config = onlyne_config::ClientConfig::load(&path)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(config.role)
}

pub async fn read_script_from_stdin() -> Result<AgentScript> {
    let mut buf = Vec::new();
    let mut stdin = tokio::io::stdin();
    tokio::io::AsyncReadExt::read_to_end(&mut stdin, &mut buf).await?;
    serde_json::from_slice(&buf).context("parse stdin script")
}

pub fn script_from_path(path: &Path) -> Result<AgentScript> {
    let file =
        std::fs::File::open(path).with_context(|| format!("open script {}", path.display()))?;
    AgentScript::from_reader(file)
}

pub async fn run_fake_gateway_render_printer(
    handle: Arc<onlyne_adapter::GatewayHandle>,
) -> Result<()> {
    loop {
        let args = handle.wait_render_send().await?;
        let line = FakeGateway::render_line(&args);
        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(serde_json::to_string(&line)?.as_bytes())
            .await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
}

pub fn parse_capability_csv(csv: &str) -> Result<Vec<Capability>> {
    if csv.trim().is_empty() {
        return Ok(Vec::new());
    }
    csv.split(',')
        .map(|part| {
            let name = part.trim();
            Capability::ALL
                .iter()
                .copied()
                .find(|capability| capability.as_str() == name)
                .ok_or_else(|| anyhow!("unknown capability: {name}"))
        })
        .collect()
}
