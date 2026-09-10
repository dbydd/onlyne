//! Onlyne adapter conformance fixtures.

use std::collections::{HashMap, HashSet};
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
use onlyne_proto::{
    AdapterMsg, AgentMount, AssignAckArgs, AssignArgs, Body, Capability, Causality, Delivery,
    DetachArgs, Envelope, ErrorCode, HealthArgs, HelloAck, HelloArgs, HostOp,
    IMAGE_DATA_MAX_BYTES, LedgerState, Mount, MsgKind, Outcome, PROTOCOL_VERSION,
    Principal, Receipt, RegisterChannelArgs, Report, RenderSendArgs, ResBody, ServerInfo,
    SessionRegisterArgs, TypingArgs, new_envelope, new_id, new_op_id, new_task_id,
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
    pub fn agent(role: impl Into<String>, prose: impl Into<String>, expected_capabilities: Vec<Capability>) -> Self {
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

    pub fn pair(spec: HostSimSpec) -> (Arc<Self>, AgentHandle, JoinHandle<onlyne_adapter::Result<()>>) {
        let sim = Self::new(spec);
        let (agent, task) = sim.clone().connect_agent();
        (sim, agent, task)
    }

    pub fn connect_agent(self: Arc<Self>) -> (AgentHandle, JoinHandle<onlyne_adapter::Result<()>>) {
        let (client, server) = tokio::io::duplex(16 * 1024 * 1024);
        let agent = AdapterClient::connect_with_timeouts(client, Duration::from_secs(5), Duration::from_secs(5));
        let sim = self.clone();
        let task = tokio::spawn(async move { sim.serve_stream(server).await });
        (agent, task)
    }

    pub fn connect_gateway(self: Arc<Self>) -> (onlyne_adapter::GatewayHandle, JoinHandle<onlyne_adapter::Result<()>>) {
        let (client, server) = tokio::io::duplex(16 * 1024 * 1024);
        let gateway = AdapterClient::gateway_with_timeouts(client, Duration::from_secs(5), Duration::from_secs(5));
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
        self.emit_scripted().await.map_err(|err| onlyne_adapter::AdapterError::Unexpected(err.to_string()))?;
        self.emit_ready_assigns().await.map_err(|err| onlyne_adapter::AdapterError::Unexpected(err.to_string()))?;
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

    pub async fn missing_capabilities(&self) -> Vec<Capability> {
        self.state.lock().await.missing_capabilities.clone()
    }

    pub async fn queue_assign(&self, assign: AssignArgs) -> Result<()> {
        let emit = {
            let mut state = self.state.lock().await;
            let task_id = assign.task_id.clone();
            state.pending_assigns.insert(task_id.clone(), assign.clone());
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

    pub async fn handle_missing_recycle(&self, task_id: impl Into<String>, timeout: Duration) -> Result<()> {
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
        self.emit_host(HostOp::Probe(json!({ "task_id": task_id }))).await?;
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
        self.record("hello", serde_json::to_value(args).unwrap_or(Value::Null)).await;
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
        self.record("report", serde_json::to_value(report).unwrap_or(Value::Null)).await;
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

    async fn session_register(&self, args: &SessionRegisterArgs) -> std::result::Result<(), (ErrorCode, String)> {
        self.record("session_register", serde_json::to_value(args).unwrap_or(Value::Null)).await;
        Ok(())
    }

    async fn assign_ack(&self, ack: &AssignAckArgs) -> std::result::Result<(), (ErrorCode, String)> {
        self.record("assign_ack", serde_json::to_value(ack).unwrap_or(Value::Null)).await;
        if ack.accepted {
            self.state.lock().await.pending_assigns.remove(&ack.task_id);
        }
        Ok(())
    }

    async fn send(&self, envelope: &Envelope) -> std::result::Result<Receipt, (ErrorCode, String)> {
        self.record("send", serde_json::to_value(envelope).unwrap_or(Value::Null)).await;
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
            duplicate: false,
        };
        if let Some(op_id) = &envelope.op_id {
            state.receipts.insert(op_id.clone(), (fingerprint, receipt.clone()));
        }
        Ok(receipt)
    }

    async fn deliver(&self, delivery: &Delivery) -> std::result::Result<(), (ErrorCode, String)> {
        self.record("deliver", serde_json::to_value(delivery).unwrap_or(Value::Null)).await;
        Ok(())
    }

    async fn register_channel(&self, args: &RegisterChannelArgs) -> std::result::Result<(), (ErrorCode, String)> {
        self.record("register_channel", serde_json::to_value(args).unwrap_or(Value::Null)).await;
        Ok(())
    }

    async fn health(&self, args: &HealthArgs) -> std::result::Result<(), (ErrorCode, String)> {
        self.record("health", serde_json::to_value(args).unwrap_or(Value::Null)).await;
        Ok(())
    }

    async fn typing(&self, args: &TypingArgs) -> std::result::Result<(), (ErrorCode, String)> {
        self.record("typing", serde_json::to_value(args).unwrap_or(Value::Null)).await;
        Ok(())
    }

    async fn detach(&self, args: &DetachArgs) -> std::result::Result<(), (ErrorCode, String)> {
        self.record("detach", serde_json::to_value(args).unwrap_or(Value::Null)).await;
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
}

impl FakeAgent {
    pub fn new(role: impl Into<String>, capabilities: Vec<Capability>, script: AgentScript, workspace: impl Into<PathBuf>) -> Self {
        FakeAgent {
            role: role.into(),
            capabilities,
            script,
            workspace: workspace.into(),
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
        let mut state = FakeAgentState {
            welcome,
            last_assign: None,
        };
        for step in &self.script.steps {
            self.run_step(handle, &mut state, step).await?;
        }
        Ok(())
    }

    async fn run_step(&self, handle: &AgentHandle, state: &mut FakeAgentState, step: &Value) -> Result<()> {
        let object = step.as_object().ok_or_else(|| anyhow!("script step must be an object"))?;
        let Some((name, value)) = object.iter().next() else {
            bail!("unknown step: empty");
        };
        match name.as_str() {
            "wait_assign" => {
                if value.as_bool() != Some(true) {
                    bail!("wait_assign must be true");
                }
                state.last_assign = Some(handle.wait_assign().await.context("wait assign")?);
            }
            "report" => {
                let kind = value.as_str().ok_or_else(|| anyhow!("report step requires a string"))?;
                let assign = state.last_assign.as_ref().ok_or_else(|| anyhow!("report requires assign"))?;
                match kind {
                    "ready" => {
                        handle.report_ready(assign.task_id.clone(), "sim-session").await?;
                    }
                    "heartbeat" => {
                        handle.report_heartbeat(assign.task_id.clone(), json!({ "state": "running" })).await?;
                    }
                    other => bail!("unknown step: report.{other}"),
                }
            }
            "complete" => {
                let assign = state.last_assign.as_ref().ok_or_else(|| anyhow!("complete requires assign"))?;
                let outcome = value
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("done");
                let outcome = parse_outcome(outcome)?;
                let head = match value.get("head_from").and_then(Value::as_str) {
                    Some("assign_body") => assign.envelope.body.text.clone(),
                    Some(other) => bail!("unknown step: complete.head_from.{other}"),
                    None => value.get("head").and_then(Value::as_str).map(str::to_string),
                };
                handle.report_complete(assign.task_id.clone(), outcome, head).await?;
            }
            "fail" => {
                let reason = value.as_str().unwrap_or("fake agent failure").to_string();
                let task_id = state.last_assign.as_ref().map(|assign| assign.task_id.clone());
                handle.report_fault(task_id, "fake_agent", reason).await?;
            }
            "exit" => {
                let reason = value.as_str().unwrap_or("script exit");
                handle.exit(reason).await?;
            }
            "sleep_ms" => {
                let ms = value.as_u64().ok_or_else(|| anyhow!("sleep_ms requires a number"))?;
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
            "assert_prose_equals" => {
                let expected = value.as_str().ok_or_else(|| anyhow!("assert_prose_equals requires a string"))?;
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
                let path = value.get("path").and_then(Value::as_str).ok_or_else(|| anyhow!("assert_field.path required"))?;
                let expected = value.get("equals").ok_or_else(|| anyhow!("assert_field.equals required"))?;
                let state_value = self.state_value(state)?;
                let actual = value_path(&state_value, path).ok_or_else(|| anyhow!("assert_field missing path {path}"))?;
                if actual != expected {
                    bail!("assert_field {path} failed: expected {expected}, got {actual}");
                }
            }
            "echo_prose_to" => {
                let file = value.as_str().ok_or_else(|| anyhow!("echo_prose_to requires a path"))?;
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

    fn state_value(&self, state: &FakeAgentState) -> Result<Value> {
        Ok(json!({
            "welcome": state.welcome,
            "assign": state.last_assign,
        }))
    }
}

struct FakeAgentState {
    welcome: HelloAck,
    last_assign: Option<AssignArgs>,
}

fn value_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn path_in_workspace(workspace: &Path, file: &str) -> PathBuf {
    let path = PathBuf::from(file);
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
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

    pub fn inbound_delivery(&self, conversation: impl Into<String>, text: impl Into<String>) -> Result<Delivery> {
        let conversation = conversation.into();
        let envelope = new_envelope(
            MsgKind::Note,
            Principal::Gateway {
                gateway: self.gateway_id.clone(),
                channel: self.platform.clone(),
                conversation: Some(conversation),
            },
            Principal::role("gateway"),
            Body::text(text.into()),
            None,
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
            let value: Value = serde_json::from_str(&line).context("parse fake gateway stdin line")?;
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
                    gateway.deliver_inbound(self.inbound_delivery(conversation, text)?).await?;
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
    vec![Capability::Report, Capability::Typing, Capability::Conversations]
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
        envelope: Box::new(envelope),
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
    let backend = onlyne_session::backend_by_name("fake", Arc::new(onlyne_session::ProcessRunner));
    match backend {
        Ok(backend) => format!("onlyne-session:{}", backend.name()),
        Err(err) => format!("hostsim-stub:{err}"),
    }
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

pub fn socket_from_workspace(workspace: &Path) -> PathBuf {
    workspace.join(".onlyne").join("run").join("s")
}

pub async fn read_script_from_stdin() -> Result<AgentScript> {
    let mut buf = Vec::new();
    let mut stdin = tokio::io::stdin();
    tokio::io::AsyncReadExt::read_to_end(&mut stdin, &mut buf).await?;
    serde_json::from_slice(&buf).context("parse stdin script")
}

pub fn script_from_path(path: &Path) -> Result<AgentScript> {
    let file = std::fs::File::open(path).with_context(|| format!("open script {}", path.display()))?;
    AgentScript::from_reader(file)
}

pub async fn run_fake_gateway_render_printer(handle: Arc<onlyne_adapter::GatewayHandle>) -> Result<()> {
    loop {
        let args = handle.wait_render_send().await?;
        let line = FakeGateway::render_line(&args);
        let mut stdout = tokio::io::stdout();
        stdout.write_all(serde_json::to_string(&line)?.as_bytes()).await?;
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
