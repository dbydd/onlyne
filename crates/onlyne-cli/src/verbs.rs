//! The message verbs: send, reply, complete, handoff, control, who, ping.

use onlyne_proto::{
    AdminControl, AdminOp, AdminSend, Body, ClientOp, ControlArgs, ControlOp, Causality,
    Envelope, ErrorCode, Frame, ImagePart, LedgerQuery, MsgKind, Outcome, Principal,
    QueryRolesArgs, Report, new_envelope, new_id, new_task_id,
};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::flags::GlobalFlags;
use crate::ledger;
use crate::media;
use crate::render;
use crate::runtime::{
    self, EXIT_ANSWER_FAILED, EXIT_NO_SOCKET, EXIT_OK,
};
use crate::socket::{Surface, SocketTarget};
use crate::wire::{self, ExchangeError, Outbound};

/// Sender flag shared by every verb that builds an `AdminSend` on the admin surface.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SenderArgs {
    /// Sender role, required on the admin surface.
    #[arg(long)]
    pub from: Option<String>,
}

impl SenderArgs {
    /// Reject `--from` on the client surface, where the local role is the sender.
    pub fn check(&self, target: &SocketTarget) -> Option<String> {
        if self.from.is_some() && target.surface == Surface::Client {
            return Some("onlyne: --from is only valid on the admin surface".to_string());
        }
        None
    }

    /// The local sender role, read from `ONLYNE_ROLE`, defaulting to `cli`.
    pub fn local_role(&self) -> String {
        std::env::var("ONLYNE_ROLE").unwrap_or_else(|_| "cli".to_string())
    }
}

/// The outcome of a `complete`, in wire spelling.
pub fn parse_outcome(raw: &str) -> Result<Outcome, String> {
    match raw {
        "done" => Ok(Outcome::Done),
        "failed" => Ok(Outcome::Failed),
        "cancelled" => Ok(Outcome::Cancelled),
        other => Err(format!(
            "onlyne: --outcome must be one of done, failed, cancelled, got {other}"
        )),
    }
}

/// Whether `complete` reads its head from the ledger or truncates locally.
pub fn parse_head_from(raw: &str) -> Result<HeadFrom, String> {
    match raw {
        "local" => Ok(HeadFrom::Local),
        "ledger" => Ok(HeadFrom::Ledger),
        other => Err(format!(
            "onlyne: --head-from must be one of local, ledger, got {other}"
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadFrom {
    Local,
    Ledger,
}

/// Character ceiling for a locally truncated completion head.
pub const HEAD_CHARS: usize = 200;

/// Truncate `text` to [`HEAD_CHARS`] characters at a char boundary.
pub fn head_of(text: &str) -> String {
    if text.chars().count() <= HEAD_CHARS {
        return text.to_string();
    }
    text.chars().take(HEAD_CHARS).collect()
}

/// Whether the daemon requires a reason for a control op.
pub fn control_requires_reason(op: &ControlOp) -> bool {
    matches!(op, ControlOp::Recycle { .. } | ControlOp::Cancel { .. })
}

/// Build the control op a CLI verb names.
pub fn build_control_op(
    verb: &str,
    task: String,
    reason: Option<String>,
) -> Result<ControlOp, String> {
    match verb {
        "recycle" => Ok(ControlOp::Recycle {
            task_id: task,
            reason: reason.unwrap_or_default(),
        }),
        "probe" => Ok(ControlOp::Probe { task_id: task }),
        "snapshot" => Ok(ControlOp::Snapshot { task_id: task }),
        "cancel" => Ok(ControlOp::Cancel {
            task_id: task,
            reason: reason.unwrap_or_default(),
        }),
        other => Err(format!("onlyne: unknown control verb {other}")),
    }
}

/// The payload a send carries, on whichever surface it travels.
enum SendPayload {
    Client(Box<Envelope>),
    Admin(AdminSend),
}

/// The six values a send needs, grouped so [`build_send`] stays short.
struct SendSpec {
    kind: MsgKind,
    to: String,
    from: Option<String>,
    text: Option<String>,
    image: Option<ImagePart>,
    causality: Causality,
    ttl_ms: Option<u64>,
}

fn build_send(
    flags: &GlobalFlags,
    target: &SocketTarget,
    spec: SendSpec,
) -> Result<SendPayload, String> {
    let sender_role = match (target.surface, spec.from) {
        (Surface::Admin, Some(role)) => Some(role),
        (Surface::Admin, None) => {
            return Err("onlyne: --from is required on the admin surface".to_string())
        }
        (Surface::Client, _) => None,
    };
    let principal = match sender_role.as_deref() {
        Some(role) => Principal::role(role),
        None => Principal::role(flags.local_role()),
    };
    let envelope = build_envelope(
        spec.kind,
        principal,
        &spec.to,
        spec.text,
        spec.image,
        spec.causality,
        spec.ttl_ms,
    )?;
    Ok(match sender_role {
        Some(role) => SendPayload::Admin(AdminSend { from: role, envelope: Box::new(envelope) }),
        None => SendPayload::Client(Box::new(envelope)),
    })
}

/// Read the text a send carries, from `--text` or `--file`.
fn read_text(text: Option<String>, file: Option<PathBuf>) -> Result<Option<String>, String> {
    if let Some(text) = text {
        return Ok(Some(text));
    }
    let Some(path) = file else {
        return Ok(None);
    };
    if path == Path::new("-") {
        let mut buffer = String::new();
        io::stdin()
            .lock()
            .read_to_string(&mut buffer)
            .map_err(|error| format!("onlyne: cannot read stdin: {error}"))?;
        return Ok(Some(buffer));
    }
    std::fs::read_to_string(&path)
        .map_err(|error| format!("onlyne: cannot read {}: {error}", path.display()))
        .map(Some)
}

/// Build one validated envelope, carrying the protocol error when it fails.
fn build_envelope(
    kind: MsgKind,
    from: Principal,
    to: &str,
    text: Option<String>,
    image: Option<ImagePart>,
    causality: Causality,
    ttl_ms: Option<u64>,
) -> Result<Envelope, String> {
    let body = Body { text, image };
    match new_envelope(kind, from, Principal::role(to), body, Some(causality)) {
        Ok(mut envelope) => {
            envelope.ttl_ms = ttl_ms;
            Ok(envelope)
        }
        Err(error) => Err(format!("onlyne: {error}")),
    }
}

/// A `--request` failure: a malformed override, or an override the protocol
/// rejects.
enum RequestError {
    /// The override did not parse into the shape this verb sends.
    Override(String),
    /// The override parsed, and the protocol rejected the envelope it carries.
    Invalid(String),
}

impl RequestError {
    /// Print the failure in this crate's local validation shape.
    fn report(self) -> i32 {
        match self {
            RequestError::Override(message) => runtime::request_error(message),
            RequestError::Invalid(message) => runtime::usage_error(message),
        }
    }
}

/// Apply `--request` to the payload, validate the envelope it carries, and wrap
/// it in the surface's op. A pinned object that breaks a protocol rule is
/// refused here, so the operator reads the field name in its own shell.
fn request_of(flags: &GlobalFlags, payload: SendPayload) -> Result<Outbound, RequestError> {
    match payload {
        SendPayload::Client(envelope) => {
            let envelope = override_envelope(flags, *envelope, |envelope| envelope)?;
            Ok(Outbound::client(new_id(), ClientOp::Send(Box::new(envelope))))
        }
        SendPayload::Admin(admin_send) => {
            let admin_send = override_envelope(flags, admin_send, |send| &send.envelope)?;
            Ok(Outbound::admin(new_id(), AdminOp::Send(admin_send)))
        }
    }
}

/// Replace the args with `--request` when given, then run `onlyne-proto`'s
/// validator over the envelope they carry, so no second rule set lives here.
fn override_envelope<T: serde::de::DeserializeOwned>(
    flags: &GlobalFlags,
    built: T,
    envelope: impl Fn(&T) -> &Envelope,
) -> Result<T, RequestError> {
    let value = flags.override_args(built).map_err(RequestError::Override)?;
    envelope(&value)
        .validate()
        .map_err(|error| RequestError::Invalid(format!("onlyne: {error}")))?;
    Ok(value)
}

/// Connect, send one request, print the answer, and return its exit code.
async fn run_one(flags: &GlobalFlags, target: &SocketTarget, request: &Outbound) -> i32 {
    let mut stream = match runtime::open(flags, target).await {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    match wire::request_res(&mut stream, request, flags.timeout_ms).await {
        Ok(body) => runtime::finish(&body, flags),
        Err(error) => runtime::exchange_error(&error, flags.timeout_ms),
    }
}

/// Print the missing-ledger-row answer and exit 1.
fn missing_row(flags: &GlobalFlags, kind: &str, id: &str) -> i32 {
    let body = onlyne_proto::ResBody::err(
        ErrorCode::UnknownRole,
        ledger::missing_row_message(kind, id),
        None,
    );
    runtime::finish(&body, flags)
}

/// Read the first ledger row back through `query_ledger`.
async fn lookup_row(
    stream: &mut tokio::net::UnixStream,
    flags: &GlobalFlags,
    target: &SocketTarget,
    args: LedgerQuery,
) -> Result<Option<serde_json::Value>, ExchangeError> {
    let rows = ledger::query(stream, flags.timeout_ms, target, new_id(), args).await?;
    Ok(rows.into_iter().next())
}

/// The task id of a ledger row, from the column or the stored envelope.
fn row_task(row: &serde_json::Value) -> Option<String> {
    ledger::row_text(row, "task").map(str::to_string).or_else(|| {
        row_causality_text(row, "task")
    })
}

/// A field of the stored envelope's causality, when the row keeps no column.
fn row_causality_text(row: &serde_json::Value, key: &str) -> Option<String> {
    for body in [
        row.get("body"),
        row.get("body_json")
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .as_ref(),
    ]
    .iter()
    .flatten()
    {
        if let Some(text) = body
            .get("causality")
            .and_then(|causality| causality.get(key))
            .and_then(serde_json::Value::as_str)
        {
            return Some(text.to_string());
        }
    }
    None
}

/// Build the send envelope for `reply`, linked back to the row it answers.
fn reply_causality(row: &serde_json::Value, to: &str) -> Causality {
    Causality {
        task: row_task(row).unwrap_or_else(new_task_id),
        parent_task: None,
        reply_to: Some(to.to_string()),
        hop: ledger::row_hop(row).unwrap_or(0),
        attempt: 0,
    }
}

/// Build the send envelope for `handoff`, a child of the parent task.
fn handoff_causality(row: &serde_json::Value, parent: &str) -> Causality {
    Causality {
        task: new_task_id(),
        parent_task: Some(parent.to_string()),
        reply_to: None,
        hop: ledger::row_hop(row).unwrap_or(0) + 1,
        attempt: 0,
    }
}

/// Read the recipient a reply goes to, from the row it answers.
fn reply_target(row: &serde_json::Value) -> Option<String> {
    let principal = ledger::row_principal(row, "to_json").or_else(|| ledger::row_principal(row, "to"));
    principal.and_then(|principal| match principal {
        Principal::Role { role, .. } => Some(role),
        _ => None,
    })
}

pub fn send(flags: &GlobalFlags, sender: &SenderArgs, args: SendArgs) -> i32 {
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    runtime::block_on(send_inner(flags, &target, sender, args))
}

async fn send_inner(
    flags: &GlobalFlags,
    target: &SocketTarget,
    sender: &SenderArgs,
    args: SendArgs,
) -> i32 {
    if let Some(message) = sender.check(target) {
        return runtime::usage_error(message);
    }
    if args.ttl.is_some() && !args.note {
        return runtime::usage_error("onlyne: --ttl requires --note");
    }
    let text = match read_text(args.text, args.file) {
        Ok(text) => text,
        Err(message) => return runtime::usage_error(message),
    };
    let image = match &args.image {
        Some(path) => match media::load_image_part(path) {
            Ok(part) => Some(part),
            Err(error) => return runtime::usage_error(error.message()),
        },
        None => None,
    };
    let kind = if args.note { MsgKind::Note } else { MsgKind::Task };
    let ttl_ms = if args.note { args.ttl } else { None };
    let causality = Causality {
        task: args.task.unwrap_or_else(new_task_id),
        parent_task: None,
        reply_to: None,
        hop: 0,
        attempt: 0,
    };
    let payload = match build_send(
        flags,
        target,
        SendSpec {
            kind,
            to: args.to,
            from: sender.from.clone(),
            text,
            image,
            causality,
            ttl_ms,
        },
    ) {
        Ok(payload) => payload,
        Err(message) => return runtime::usage_error(message),
    };
    let request = match request_of(flags, payload) {
        Ok(request) => request,
        Err(error) => return error.report(),
    };
    run_one(flags, target, &request).await
}

pub fn reply(flags: &GlobalFlags, sender: &SenderArgs, args: ReplyArgs) -> i32 {
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    runtime::block_on(reply_inner(flags, &target, sender, args))
}

async fn reply_inner(
    flags: &GlobalFlags,
    target: &SocketTarget,
    sender: &SenderArgs,
    args: ReplyArgs,
) -> i32 {
    if let Some(message) = sender.check(target) {
        return runtime::usage_error(message);
    }
    let mut stream = match runtime::open(flags, target).await {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    let row = match lookup_row(
        &mut stream,
        flags,
        target,
        LedgerQuery {
            msg_id: Some(args.to.clone()),
            ..Default::default()
        },
    )
    .await
    {
        Ok(row) => row,
        Err(error) => return runtime::exchange_error(&error, flags.timeout_ms),
    };
    let Some(row) = row else {
        return missing_row(flags, "envelope", &args.to);
    };
    let Some(to) = reply_target(&row) else {
        return runtime::usage_error(format!(
            "onlyne: ledger row for envelope {} has no recipient",
            args.to
        ));
    };
    let payload = match build_send(
        flags,
        target,
        SendSpec {
            kind: MsgKind::Note,
            to,
            from: sender.from.clone(),
            text: Some(args.text),
            image: None,
            causality: reply_causality(&row, &args.to),
            ttl_ms: None,
        },
    ) {
        Ok(payload) => payload,
        Err(message) => return runtime::usage_error(message),
    };
    let request = match request_of(flags, payload) {
        Ok(request) => request,
        Err(error) => return error.report(),
    };
    match wire::request_res(&mut stream, &request, flags.timeout_ms).await {
        Ok(body) => runtime::finish(&body, flags),
        Err(error) => runtime::exchange_error(&error, flags.timeout_ms),
    }
}

pub fn complete(flags: &GlobalFlags, sender: &SenderArgs, args: CompleteArgs) -> i32 {
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    runtime::block_on(complete_inner(flags, &target, sender, args))
}

async fn complete_inner(
    flags: &GlobalFlags,
    target: &SocketTarget,
    sender: &SenderArgs,
    args: CompleteArgs,
) -> i32 {
    if let Some(message) = sender.check(target) {
        return runtime::usage_error(message);
    }
    let mut stream = match runtime::open(flags, target).await {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    let head = match args.head_from {
        HeadFrom::Local => head_of(&args.text),
        HeadFrom::Ledger => {
            let row = match lookup_row(
                &mut stream,
                flags,
                target,
                LedgerQuery {
                    task: Some(args.task.clone()),
                    ..Default::default()
                },
            )
            .await
            {
                Ok(row) => row,
                Err(error) => return runtime::exchange_error(&error, flags.timeout_ms),
            };
            let Some(row) = row else {
                return missing_row(flags, "task", &args.task);
            };
            match ledger::row_text(&row, "out_head") {
                Some(head) => head.to_string(),
                None => return runtime::usage_error(format!(
                    "onlyne: ledger row for task {} has no out_head",
                    args.task
                )),
            }
        }
    };
    let causality = Causality {
        task: args.task.clone(),
        parent_task: None,
        reply_to: None,
        hop: 0,
        attempt: 0,
    };
    let to = args.to.unwrap_or_else(|| sender.local_role());
    let payload = match build_send(
        flags,
        target,
        SendSpec {
            kind: MsgKind::Completion,
            to,
            from: sender.from.clone(),
            text: Some(args.text),
            image: None,
            causality,
            ttl_ms: None,
        },
    ) {
        Ok(payload) => payload,
        Err(message) => return runtime::usage_error(message),
    };
    let reply_to = match &payload {
        SendPayload::Client(envelope) => envelope.id.clone(),
        SendPayload::Admin(admin_send) => admin_send.envelope.id.clone(),
    };
    let request = match request_of(flags, payload) {
        Ok(request) => request,
        Err(error) => return error.report(),
    };
    match wire::request_res(&mut stream, &request, flags.timeout_ms).await {
        Ok(body) if !body.ok => return runtime::finish(&body, flags),
        Ok(_) => {}
        Err(error) => return runtime::exchange_error(&error, flags.timeout_ms),
    }
    let report = Outbound::client(
        new_id(),
        ClientOp::Report(Report::Complete {
            task_id: args.task,
            outcome: args.outcome,
            head: Some(head),
            reply_to: Some(reply_to),
        }),
    );
    match wire::request_res(&mut stream, &report, flags.timeout_ms).await {
        Ok(body) => runtime::finish(&body, flags),
        Err(error) => runtime::exchange_error(&error, flags.timeout_ms),
    }
}

pub fn handoff(flags: &GlobalFlags, sender: &SenderArgs, args: HandoffArgs) -> i32 {
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    runtime::block_on(handoff_inner(flags, &target, sender, args))
}

async fn handoff_inner(
    flags: &GlobalFlags,
    target: &SocketTarget,
    sender: &SenderArgs,
    args: HandoffArgs,
) -> i32 {
    if let Some(message) = sender.check(target) {
        return runtime::usage_error(message);
    }
    let mut stream = match runtime::open(flags, target).await {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    let ledger_rows = match ledger::query(
        &mut stream,
        flags.timeout_ms,
        target,
        new_id(),
        LedgerQuery {
            task: Some(args.task.clone()),
            ..Default::default()
        },
    )
    .await
    {
        Ok(rows) => rows,
        Err(error) => return runtime::exchange_error(&error, flags.timeout_ms),
    };
    let Some(row) = ledger::deepest(&ledger_rows) else {
        return missing_row(flags, "task", &args.task);
    };
    let payload = match build_send(
        flags,
        target,
        SendSpec {
            kind: MsgKind::Task,
            to: args.to,
            from: sender.from.clone(),
            text: Some(args.text),
            image: None,
            causality: handoff_causality(row, &args.task),
            ttl_ms: None,
        },
    ) {
        Ok(payload) => payload,
        Err(message) => return runtime::usage_error(message),
    };
    let request = match request_of(flags, payload) {
        Ok(request) => request,
        Err(error) => return error.report(),
    };
    match wire::request_res(&mut stream, &request, flags.timeout_ms).await {
        Ok(body) => runtime::finish(&body, flags),
        Err(error) => runtime::exchange_error(&error, flags.timeout_ms),
    }
}

pub fn control(flags: &GlobalFlags, sender: &SenderArgs, args: ControlVerbArgs) -> i32 {
    if flags.request.is_some() {
        return runtime::usage_error("onlyne: --request is not supported by control");
    }
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    runtime::block_on(control_inner(flags, &target, sender, args))
}

async fn control_inner(
    flags: &GlobalFlags,
    target: &SocketTarget,
    sender: &SenderArgs,
    args: ControlVerbArgs,
) -> i32 {
    if let Some(message) = sender.check(target) {
        return runtime::usage_error(message);
    }
    let op = args.op;
    let op_name = op.name().to_string();
    if control_requires_reason(&op)
        && args.reason.as_deref().map(str::trim).unwrap_or("").is_empty()
    {
        return runtime::usage_error(format!("onlyne: --reason is required for {op_name}"));
    }
    let request = match target.surface {
        Surface::Client => Outbound::client(
            new_id(),
            ClientOp::Control(ControlArgs {
                to: args.to,
                op,
            }),
        ),
        Surface::Admin => match sender.from.clone() {
            Some(from) => Outbound::admin(
                new_id(),
                AdminOp::Control(AdminControl {
                    from,
                    op,
                    to: args.to,
                }),
            ),
            None => return runtime::usage_error("onlyne: --from is required on the admin surface"),
        },
    };
    run_one(flags, target, &request).await
}

pub fn who(flags: &GlobalFlags) -> i32 {
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    runtime::block_on(async move {
        let args = QueryRolesArgs { role: None };
        let request = match target.surface {
            Surface::Admin => Outbound::admin(new_id(), AdminOp::Roles(args)),
            Surface::Client => Outbound::client(new_id(), ClientOp::QueryRoles(args)),
        };
        run_one(flags, &target, &request).await
    })
}

pub fn ping(flags: &GlobalFlags) -> i32 {
    if flags.request.is_some() {
        return runtime::usage_error("onlyne: --request is not supported by ping");
    }
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    runtime::block_on(async move {
        let mut stream = match runtime::open(flags, &target).await {
            Ok(stream) => stream,
            Err(code) => return code,
        };
        let probe: Frame = Frame::Ping { t: now_millis() };
        if let Err(error) = wire::send_frame(&mut stream, &probe, flags.timeout_ms).await {
            return runtime::exchange_error(&error, flags.timeout_ms);
        }
        match wire::recv_frame(&mut stream, flags.timeout_ms).await {
            Ok(answer) => match &answer {
                Frame::Pong { .. } => {
                    println!(
                        "{}",
                        render::render_pong(&answer, flags)
                    );
                    EXIT_OK
                }
                other => {
                    println!(
                        "{}",
                        render::local_error_json(
                            ErrorCode::BadFrame,
                            format!(
                                "expected a pong frame, got a {} frame",
                                wire::frame_name(other)
                            ),
                            None
                        )
                    );
                    EXIT_ANSWER_FAILED
                }
            },
            Err(error) => runtime::exchange_error(&error, flags.timeout_ms),
        }
    })
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, clap::Args)]
pub struct SendArgs {
    /// Recipient role.
    #[arg(long)]
    pub to: String,
    /// Task family id to attach to; minted when omitted.
    #[arg(long)]
    pub task: Option<String>,
    /// Message text; mutually exclusive with `--file`.
    #[arg(long, conflicts_with = "file")]
    pub text: Option<String>,
    /// Read the message text from a file, or from stdin when `-`.
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// Inline image to attach.
    #[arg(long)]
    pub image: Option<PathBuf>,
    /// Deliver as a note, the only kind that may carry a ttl.
    #[arg(long)]
    pub note: bool,
    /// Expiry in milliseconds; requires `--note`.
    #[arg(long)]
    pub ttl: Option<u64>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct ReplyArgs {
    /// Envelope id being answered.
    #[arg(long)]
    pub to: String,
    /// Reply text.
    #[arg(long)]
    pub text: String,
}

#[derive(Debug, Clone, clap::Args)]
pub struct CompleteArgs {
    /// Recipient role; the local role when omitted.
    #[arg(long)]
    pub to: Option<String>,
    /// Task being completed.
    #[arg(long)]
    pub task: String,
    /// Completion text.
    #[arg(long)]
    pub text: String,
    /// Terminal outcome: done, failed, cancelled.
    #[arg(long, value_parser = parse_outcome)]
    pub outcome: Outcome,
    /// Head source: local, or ledger.
    #[arg(long, value_parser = parse_head_from)]
    pub head_from: HeadFrom,
}

#[derive(Debug, Clone, clap::Args)]
pub struct HandoffArgs {
    /// Recipient role.
    #[arg(long)]
    pub to: String,
    /// Task being handed off.
    #[arg(long)]
    pub task: String,
    /// Handoff text.
    #[arg(long)]
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct ControlVerbArgs {
    pub to: Option<String>,
    pub reason: Option<String>,
    pub op: ControlOp,
}
