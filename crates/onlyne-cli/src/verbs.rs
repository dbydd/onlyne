//! The message verbs: send, reply, complete, handoff, ack, reject, control, who, ping.

use chrono::{DateTime, Utc};
use onlyne_proto::envelope::CAUSALITY_LABEL_MAX_ENTRIES;
use onlyne_proto::{
    AckArgs as ProtoAckArgs, AdminControl, AdminOp, AdminSend, Body, Causality, ClientOp,
    ControlArgs, ControlOp, Envelope, ErrorCode, Frame, ImagePart, LedgerQuery, MsgKind, Outcome,
    Principal, QueryRolesArgs, QuerySessionsArgs, Report, ResBody, new_envelope, new_id,
    new_task_id,
};
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::flags::GlobalFlags;
use crate::ledger;
use crate::media;
use crate::render;
use crate::runtime::{self, EXIT_ANSWER_FAILED, EXIT_NO_SOCKET, EXIT_OK, EXIT_REFUSAL};
use crate::socket::{SocketTarget, Surface};
use crate::wire::{self, ExchangeError, Outbound};

/// Sender flag shared by every verb that builds an `AdminSend` on the admin surface.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SenderArgs {
    /// Sender role, required on the admin surface. Global so it may follow the
    /// verb it belongs to, which is where an operator reaches for it.
    #[arg(long, global = true)]
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

/// The flags every verb a role speaks through take, spelled as the operator
/// types them. Both are named in every refusal, and both belong to those seven
/// verbs alone: `generate` and `skill export` carry a `--force` of their own.
const SUPERVISOR_FLAGS: &str = "--force and --yes-i-am-supervisor-not-other-role";

/// The plugin tool `send` stands in for, named in its refusal with the arguments
/// that tool takes and with what each kind does. A `task` send starts a family
/// of its own, so continuing one goes through `onlyne_handoff`.
const PLUGIN_SEND: &str = "sends with its plugin's own tool, onlyne_send (to, text, kind, image), where kind=\"task\" \
     starts a new task family at hop 0, kind=\"note\" leaves free text, and onlyne_handoff \
     continues the family this session was handed";

/// The plugin tool `handoff` stands in for, which reads the parent row back to
/// continue the task family the way this verb does.
const PLUGIN_HANDOFF: &str = "hands work on with its plugin's own tool, onlyne_handoff (task_id, to, text, image), which \
     names this task as the child's parent_task and carries the family's hop budget, origin, \
     deadline, and labels";

/// The plugin tool `complete` stands in for, which is the only path a session
/// has to `done`.
const PLUGIN_COMPLETE: &str =
    "reports its ending with its plugin's own tool, onlyne_complete (outcome, text, force, reason)";

/// What a role inside a session does for `reply`, which has no plugin tool of
/// its own. The plugin carries the session's own connection, so it is what
/// answers for the act, and a role has no reason to reach for this verb.
const PLUGIN_REPLY: &str = "replies through its plugin, which answers for its session and offers no reply tool that a \
     role would reach for";

/// The same for `ack`.
const PLUGIN_ACK: &str = "settles a delivered envelope through its plugin, which answers for its session and offers \
     no ack tool that a role would reach for";

/// The same for `reject`.
const PLUGIN_REJECT: &str = "refuses a delivered envelope through its plugin, which answers for its session and offers \
     no reject tool that a role would reach for";

/// The same for `control`.
const PLUGIN_CONTROL: &str = "runs a control op through its plugin, which answers for its session and offers no control \
     tool that a role would reach for";

/// The two flags a gated verb takes, flattened into every one of them.
///
/// Both carry `global = true` so `control` reads them on either side of its op
/// token, the way its own `--task` and `--from` do. Nothing else in the tree
/// declares a `--force` above a gated verb, so the pair collides with no
/// forwarder: `generate --force` and `skill export --force` stay their own.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SupervisorArgs {
    /// Acknowledge this verb as a supervisor maintenance command; required with
    /// `--yes-i-am-supervisor-not-other-role`.
    #[arg(long, global = true)]
    pub force: bool,
    /// Name the caller a supervisor driving this role from outside a session;
    /// required with `--force`.
    #[arg(long, global = true)]
    pub yes_i_am_supervisor_not_other_role: bool,
}

/// Refuse a verb a role inside a session reaches through its plugin, when the
/// caller omitted either flag. The refusal is local: it is decided before the
/// socket resolves and before anything is written.
///
/// The plugin keeps the session's own record of the act. A verb that reaches the
/// daemon from a shell leaves that record untouched: a bash handoff is invisible
/// to the relay guard, and a bash completion settles a task the plugin still
/// holds open. These verbs serve an operator or a supervisor driving a role from
/// outside, and the two flags are how such a caller says so. `tool` is the path
/// a role reads instead, which is what a caller that reached for the shell by
/// mistake is meant to take.
fn supervisor_gate(verb: &str, tool: &str, args: &SupervisorArgs) -> Option<i32> {
    if args.force && args.yes_i_am_supervisor_not_other_role {
        return None;
    }
    Some(runtime::usage_error(format!(
        "onlyne: {verb} requires {SUPERVISOR_FLAGS}: a role inside a session {tool}; this verb is \
         a supervisor maintenance command for an operator or a supervisor driving a role from \
         outside"
    )))
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

/// One `--label key=value` entry, refused when it carries no key or no `=`.
fn parse_label(raw: &str) -> Result<(String, String), String> {
    match raw.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
        _ => Err(format!("onlyne: --label needs key=value, got {raw}")),
    }
}

/// `--deadline` as the RFC 3339 instant the wire carries.
fn parse_deadline(raw: &str) -> Result<DateTime<Utc>, String> {
    raw.parse()
        .map_err(|_| format!("onlyne: --deadline needs an RFC 3339 timestamp, got {raw}"))
}

/// The label map a send carries, refused past the protocol's own entry ceiling.
/// The per-key bounds are the protocol's business, and it checks them when the
/// envelope is validated.
fn collect_labels(
    entries: &[(String, String)],
) -> Result<Option<BTreeMap<String, String>>, String> {
    if entries.is_empty() {
        return Ok(None);
    }
    if entries.len() > CAUSALITY_LABEL_MAX_ENTRIES {
        return Err(format!(
            "onlyne: --label carries at most {CAUSALITY_LABEL_MAX_ENTRIES} entries, got {}",
            entries.len()
        ));
    }
    Ok(Some(entries.iter().cloned().collect()))
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
        "focus" => Ok(ControlOp::Focus { task_id: task }),
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

/// The role a send speaks as, which is the rule [`build_send`] reads off the
/// same two inputs to pick the principal: `--from` on the admin surface, and the
/// local role on the client one.
fn sender_role(sender: &SenderArgs, target: &SocketTarget) -> String {
    match (target.surface, sender.from.as_deref()) {
        (Surface::Admin, Some(role)) => role.to_string(),
        _ => sender.local_role(),
    }
}

fn build_send(
    flags: &GlobalFlags,
    target: &SocketTarget,
    spec: SendSpec,
) -> Result<SendPayload, String> {
    let sender_role = match (target.surface, spec.from) {
        (Surface::Admin, Some(role)) => Some(role),
        (Surface::Admin, None) => {
            return Err("onlyne: --from is required on the admin surface".to_string());
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
        Some(role) => SendPayload::Admin(AdminSend {
            from: role,
            envelope: Box::new(envelope),
        }),
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
            Ok(Outbound::client(
                new_id(),
                ClientOp::Send(Box::new(envelope)),
            ))
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
    run_on(&mut stream, flags, request).await
}

/// Send one request on an open stream, print the answer, and return its exit code.
async fn run_on(
    stream: &mut onlyne_layout::LocalStream,
    flags: &GlobalFlags,
    request: &Outbound,
) -> i32 {
    match wire::request_res(stream, request, flags.timeout_ms).await {
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
    stream: &mut onlyne_layout::LocalStream,
    flags: &GlobalFlags,
    target: &SocketTarget,
    args: LedgerQuery,
) -> Result<Option<serde_json::Value>, ExchangeError> {
    let rows = ledger::query(stream, flags.timeout_ms, target, new_id(), args).await?;
    Ok(rows.into_iter().next())
}

/// The task id of a ledger row, from the column or the stored envelope.
fn row_task(row: &serde_json::Value) -> Option<String> {
    ledger::row_text(row, "task")
        .map(str::to_string)
        .or_else(|| row_causality_text(row, "task"))
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
        // A reply answers inside a family it does not describe: it starts none
        // and carries none of a family's metadata.
        family: None,
        hop_budget: None,
        origin: None,
        deadline: None,
        labels: None,
    }
}

/// Build the send envelope for `handoff`, a child of the parent task.
///
/// The child continues the parent row's family, which is what the ledger reads
/// a run's arc from: it carries the row's `family` column beside the parent
/// link, the hops that family may spend, the role it reports home to, its
/// deadline, and its labels. A row that minted before those columns existed
/// names none of them, and the parent's own task id stands in as the family its
/// child carries.
fn handoff_causality(row: &serde_json::Value, parent: &str) -> Causality {
    Causality {
        task: new_task_id(),
        parent_task: Some(parent.to_string()),
        reply_to: None,
        hop: ledger::row_hop(row).unwrap_or(0) + 1,
        attempt: 0,
        family: ledger::row_family(row).or_else(|| Some(parent.to_string())),
        hop_budget: ledger::row_hop_budget(row),
        origin: ledger::row_origin(row),
        deadline: ledger::row_deadline(row),
        labels: ledger::row_labels(row),
    }
}

/// Read the recipient a reply goes to, from the row it answers.
fn reply_target(row: &serde_json::Value) -> Option<String> {
    let principal =
        ledger::row_principal(row, "to_json").or_else(|| ledger::row_principal(row, "to"));
    principal.and_then(|principal| match principal {
        Principal::Role { role, .. } => Some(role),
        _ => None,
    })
}

pub fn send(flags: &GlobalFlags, sender: &SenderArgs, args: SendArgs) -> i32 {
    if let Some(code) = supervisor_gate("send", PLUGIN_SEND, &args.supervisor) {
        return code;
    }
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
    let kind = if args.note {
        MsgKind::Note
    } else {
        MsgKind::Task
    };
    let ttl_ms = if args.note { args.ttl } else { None };
    let labels = match collect_labels(&args.label) {
        Ok(labels) => labels,
        Err(message) => return runtime::usage_error(message),
    };
    // A send with no task of its own starts a family, and the task it mints is
    // that family's root. The family's own figures ride with it: the role it
    // reports home to, the hops it may spend, its wall-clock bound, and its
    // labels.
    let mut causality = Causality::root(args.task.unwrap_or_else(new_task_id));
    causality.origin = Some(sender_role(sender, target));
    causality.hop_budget = args.hop_budget;
    causality.deadline = args.deadline;
    causality.labels = labels;
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
    if let Some(code) = supervisor_gate("reply", PLUGIN_REPLY, &args.supervisor) {
        return code;
    }
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
    if let Some(code) = supervisor_gate("complete", PLUGIN_COMPLETE, &args.supervisor) {
        return code;
    }
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
    // The head `complete` files has one of two sources. `--head-from local`
    // truncates `--text`, and that flag is required there. `--head-from ledger`
    // reads the row's `out_head`, and `--text` stays an optional payload there.
    let local_head = match args.head_from {
        HeadFrom::Local => match args.text.as_deref() {
            Some(text) => Some(head_of(text)),
            None => {
                return runtime::usage_error("onlyne: --text is required with --head-from local");
            }
        },
        HeadFrom::Ledger => None,
    };
    let mut stream = match runtime::open(flags, target).await {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    let head = match local_head {
        Some(head) => head,
        None => {
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
                None => {
                    return runtime::usage_error(format!(
                        "onlyne: ledger row for task {} has no out_head",
                        args.task
                    ));
                }
            }
        }
    };
    // The completion names the task it settles and claims nothing about the family.
    // This door holds no family in hand: it reads a row only on the `--head-from
    // ledger` branch, and a completion of a downstream task would otherwise name that
    // task as the root of a family it sits inside. The session that settles its own
    // task writes the family off the causality it holds, so a run's receipts reach
    // the ledger with their figures from that path.
    let causality = Causality {
        task: args.task.clone(),
        ..Default::default()
    };
    let to = args.to.unwrap_or_else(|| sender.local_role());
    let payload = match build_send(
        flags,
        target,
        SendSpec {
            kind: MsgKind::Completion,
            to,
            from: sender.from.clone(),
            text: args.text,
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
            // The command line speaks as a role, whose cluster identity comes
            // from the server's spec, so a CLI-authored report never names one.
            cluster_ref: None,
        }),
    );
    match wire::request_res(&mut stream, &report, flags.timeout_ms).await {
        Ok(body) => runtime::finish(&body, flags),
        Err(error) => runtime::exchange_error(&error, flags.timeout_ms),
    }
}

pub fn handoff(flags: &GlobalFlags, sender: &SenderArgs, args: HandoffArgs) -> i32 {
    if let Some(code) = supervisor_gate("handoff", PLUGIN_HANDOFF, &args.supervisor) {
        return code;
    }
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
    if let Some(code) = supervisor_gate("control", PLUGIN_CONTROL, &args.supervisor) {
        return code;
    }
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
        && args
            .reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return runtime::usage_error(format!("onlyne: --reason is required for {op_name}"));
    }
    match target.surface {
        Surface::Client => {
            let request =
                Outbound::client(new_id(), ClientOp::Control(ControlArgs { to: args.to, op }));
            run_one(flags, target, &request).await
        }
        Surface::Admin => match sender.from.clone() {
            Some(from) => admin_control(flags, target, from, args.to, op).await,
            None => runtime::usage_error("onlyne: --from is required on the admin surface"),
        },
    }
}

/// Drive one control op on the admin surface.
///
/// Without `--to` the op goes to the role that owns the task, read out of the
/// task's own session row before anything is written: the row already names the
/// role a control op has to reach, so no caller has to state it twice. A task
/// whose session row names no role has nobody to answer the op, and the verb
/// refuses with nothing written.
async fn admin_control(
    flags: &GlobalFlags,
    target: &SocketTarget,
    from: String,
    to: Option<String>,
    op: ControlOp,
) -> i32 {
    let mut stream = match runtime::open(flags, target).await {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    let to = match to {
        // An explicit `--to` is the whole answer, so nothing is read to reach it.
        Some(to) => to,
        None => match owning_role(&mut stream, flags, op.task_id()).await {
            Ok(Some(role)) => role,
            Ok(None) => {
                eprintln!(
                    "onlyne: no session owns task {}; pass --to <role> to say where the control goes",
                    op.task_id()
                );
                return EXIT_REFUSAL;
            }
            Err(code) => return code,
        },
    };
    let request = Outbound::admin(
        new_id(),
        AdminOp::Control(AdminControl {
            from,
            op,
            to: Some(to),
        }),
    );
    run_on(&mut stream, flags, &request).await
}

/// The role that owns `task`, read through `query_sessions`.
///
/// The read is the one `onlyne sessions --task` answers with, and the column is
/// the one the server reads when it resolves a control op's owner. `Ok(None)` is
/// an answer that names no owner: no session row for the task, or a row carrying
/// no role. `Err` carries the exit code of an exchange that has already reported
/// itself.
async fn owning_role(
    stream: &mut onlyne_layout::LocalStream,
    flags: &GlobalFlags,
    task: &str,
) -> Result<Option<String>, i32> {
    let filter = QuerySessionsArgs {
        task_id: Some(task.to_string()),
        limit: 1,
        ..QuerySessionsArgs::default()
    };
    let request = Outbound::admin(new_id(), AdminOp::Sessions(filter));
    let body = match wire::request_res(stream, &request, flags.timeout_ms).await {
        Ok(body) => body,
        Err(error) => return Err(runtime::exchange_error(&error, flags.timeout_ms)),
    };
    if !body.ok {
        return Err(runtime::finish(&body, flags));
    }
    Ok(owner_of(&body))
}

/// The owning role out of a `query_sessions` answer.
fn owner_of(body: &ResBody) -> Option<String> {
    body.data
        .as_ref()?
        .get("sessions")?
        .as_array()?
        .first()?
        .get("role")?
        .as_str()
        .map(str::to_string)
}

pub fn ack(flags: &GlobalFlags, args: AckArgs) -> i32 {
    ack_decision(flags, args, true, "ack", PLUGIN_ACK)
}

pub fn reject(flags: &GlobalFlags, args: AckArgs) -> i32 {
    ack_decision(flags, args, false, "reject", PLUGIN_REJECT)
}

fn ack_decision(flags: &GlobalFlags, args: AckArgs, accepted: bool, verb: &str, tool: &str) -> i32 {
    if let Some(code) = supervisor_gate(verb, tool, &args.supervisor) {
        return code;
    }
    if flags.request.is_some() {
        return runtime::usage_error(format!("onlyne: --request is not supported by {verb}"));
    }
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    if target.surface != Surface::Client {
        return runtime::usage_error(format!(
            "onlyne: {verb} requires a role workspace or client socket"
        ));
    }
    let request = Outbound::client(
        new_id(),
        ClientOp::Ack(ProtoAckArgs {
            msg_id: args.msg_id,
            op_id: args.op_id,
            accepted,
            reason: Some(args.reason),
        }),
    );
    runtime::block_on(async { run_one(flags, &target, &request).await })
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
                    println!("{}", render::render_pong(&answer, flags));
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
    /// Hops the family this send starts may spend; every hop of the family
    /// carries the number, so the role that meets it can decide to keep the
    /// task instead of handing it on.
    #[arg(long)]
    pub hop_budget: Option<u32>,
    /// Family metadata as `key=value`; repeatable, at most 8 entries.
    #[arg(long = "label", value_parser = parse_label)]
    pub label: Vec<(String, String)>,
    /// Wall-clock bound for the whole family, as an RFC 3339 timestamp.
    #[arg(long, value_parser = parse_deadline)]
    pub deadline: Option<DateTime<Utc>>,
    #[command(flatten)]
    pub supervisor: SupervisorArgs,
}

#[derive(Debug, Clone, clap::Args)]
pub struct ReplyArgs {
    /// Envelope id being answered.
    #[arg(long)]
    pub to: String,
    /// Reply text.
    #[arg(long)]
    pub text: String,
    #[command(flatten)]
    pub supervisor: SupervisorArgs,
}

#[derive(Debug, Clone, clap::Args)]
pub struct CompleteArgs {
    /// Recipient role; the local role when omitted.
    #[arg(long)]
    pub to: Option<String>,
    /// Task being completed.
    #[arg(long)]
    pub task: String,
    /// Completion text. `--head-from local` requires it: that head is this text
    /// truncated to the character ceiling.
    #[arg(long)]
    pub text: Option<String>,
    /// Terminal outcome: done, failed, cancelled.
    #[arg(long, value_parser = parse_outcome)]
    pub outcome: Outcome,
    /// Head source: local, or ledger. Omitted means `local`.
    #[arg(long, value_parser = parse_head_from, default_value = "local")]
    pub head_from: HeadFrom,
    #[command(flatten)]
    pub supervisor: SupervisorArgs,
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
    #[command(flatten)]
    pub supervisor: SupervisorArgs,
}

#[derive(Debug, Clone, clap::Args)]
pub struct AckArgs {
    /// Delivered envelope id being settled.
    #[arg(long)]
    pub msg_id: String,
    /// Operation id the envelope carried, when present.
    #[arg(long)]
    pub op_id: Option<String>,
    /// Reason recorded with the decision.
    #[arg(long)]
    pub reason: String,
    #[command(flatten)]
    pub supervisor: SupervisorArgs,
}

#[derive(Debug, Clone)]
pub struct ControlVerbArgs {
    pub to: Option<String>,
    pub reason: Option<String>,
    pub op: ControlOp,
    pub supervisor: SupervisorArgs,
}
