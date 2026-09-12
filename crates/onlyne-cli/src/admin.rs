//! The bare admin nouns, which talk to the resolved socket like the message verbs.

use onlyne_proto::{
    AdminOp, ClientOp, EventTier, Frame, HistoryArgs as ProtoHistoryArgs, LedgerQuery, LedgerState,
    Lifecycle, MsgKind, Principal, QueryFaultsArgs, QueryRolesArgs, QuerySessionsArgs, RepairAck,
    RepairAdopt, RepairFail, RepairRebind, RepairTarget, ResBody, Subscribe, new_id,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

use crate::flags::GlobalFlags;
use crate::runtime::{self, EXIT_ANSWER_FAILED, EXIT_NO_SOCKET, EXIT_OK, EXIT_VALIDATION};
use crate::socket::{SocketTarget, Surface};
use crate::wire::{self, ExchangeError, Outbound};

/// One `repair` verb on the admin surface.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum RepairVerb {
    Inspect(RepairTargetArgs),
    Adopt(RepairAdoptArgs),
    Rebind(RepairRebindArgs),
    Retry(RepairTargetArgs),
    Fail(RepairFailArgs),
    Close(RepairTargetArgs),
    Ack(RepairAckArgs),
}

#[derive(Debug, Clone, clap::Args)]
pub struct RepairTargetArgs {
    #[arg(long)]
    pub task: String,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct RepairAdoptArgs {
    #[arg(long)]
    pub task: String,
    #[arg(long)]
    pub session_id: String,
    #[arg(long)]
    pub backend: String,
    #[arg(long)]
    pub backend_ref: Option<String>,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Clone, clap::Args)]
pub struct RepairRebindArgs {
    #[arg(long)]
    pub task: String,
    #[arg(long)]
    pub session_id: String,
    #[arg(long)]
    pub backend: String,
    #[arg(long)]
    pub backend_ref: Option<String>,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Clone, clap::Args)]
pub struct RepairFailArgs {
    #[arg(long)]
    pub task: String,
    #[arg(long)]
    pub reason: String,
    #[arg(long)]
    pub notify: Option<String>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct RepairAckArgs {
    #[arg(long = "fault-id")]
    pub fault_id: i64,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Clone, clap::Args)]
pub struct RolesArgs {
    #[arg(long)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct SessionsArgs {
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub role: Option<String>,
    #[arg(long, value_parser = parse_lifecycle)]
    pub lifecycle: Option<Lifecycle>,
    #[arg(long)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct LedgerArgs {
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub msg_id: Option<String>,
    #[arg(long)]
    pub op_id: Option<String>,
    #[arg(long)]
    pub role: Option<String>,
    #[arg(long, value_parser = parse_ledger_state)]
    pub state: Option<LedgerState>,
    #[arg(long, value_parser = parse_msg_kind)]
    pub kind: Option<MsgKind>,
    #[arg(long)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct FaultsArgs {
    #[arg(long)]
    pub role: Option<String>,
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub open_only: bool,
    #[arg(long)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct WatchArgs {
    #[arg(long)]
    pub since: Option<u64>,
    /// Replay tier to subscribe to; repeatable, every tier when omitted.
    #[arg(long, value_parser = parse_tier)]
    pub tier: Vec<EventTier>,
    /// Keep the connection open and print one JSON line per event frame.
    #[arg(long)]
    pub follow: bool,
}

#[derive(Debug, Clone, clap::Args)]
pub struct HistoryArgs {
    #[arg(long)]
    pub since: Option<u64>,
    #[arg(long)]
    pub limit: Option<u32>,
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct WaitReadyArgs {
    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 200)]
    pub interval_ms: u64,
}

#[derive(Debug, Clone, clap::Args)]
pub struct ExportProseArgs {
    /// Role whose prose is exported; the local role when omitted.
    #[arg(long)]
    pub role: Option<String>,
}

/// Parse a proto enum from its wire spelling, with the flag name in the error.
fn parse_enum<T: DeserializeOwned>(flag: &str, raw: &str) -> Result<T, String> {
    serde_json::from_value(Value::String(raw.to_string()))
        .map_err(|error| format!("onlyne: {flag}: {error}"))
}

pub fn parse_ledger_state(raw: &str) -> Result<LedgerState, String> {
    parse_enum("--state", raw)
}

pub fn parse_lifecycle(raw: &str) -> Result<Lifecycle, String> {
    parse_enum("--lifecycle", raw)
}

pub fn parse_tier(raw: &str) -> Result<EventTier, String> {
    parse_enum("--tier", raw)
}

pub fn parse_msg_kind(raw: &str) -> Result<MsgKind, String> {
    parse_enum("--kind", raw)
}

/// The admin surface for a noun the client surface does not carry.
fn admin_surface(flags: &GlobalFlags, verb: &str) -> Result<SocketTarget, i32> {
    let Some(target) = runtime::target(flags) else {
        return Err(EXIT_NO_SOCKET);
    };
    if target.surface != Surface::Admin {
        return Err(runtime::usage_error(format!(
            "onlyne: {verb} needs the admin surface; pass --server-root <dir>, or --socket <path> with --as admin"
        )));
    }
    Ok(target)
}

/// Pick the request the resolved surface carries.
fn outbound(surface: Surface, admin: AdminOp, client: ClientOp) -> Outbound {
    match surface {
        Surface::Admin => Outbound::admin(new_id(), admin),
        Surface::Client => Outbound::client(new_id(), client),
    }
}

/// Send one request to the target and print the answer.
async fn exchange(flags: &GlobalFlags, target: &SocketTarget, request: &Outbound) -> i32 {
    let mut stream = match runtime::open(flags, target).await {
        Ok(stream) => stream,
        Err(code) => return code,
    };
    match wire::request_res(&mut stream, request, flags.timeout_ms).await {
        Ok(body) => runtime::finish(&body, flags),
        Err(error) => runtime::exchange_error(&error, flags.timeout_ms),
    }
}

/// Send one admin op.
async fn run_admin(flags: &GlobalFlags, target: &SocketTarget, op: AdminOp) -> i32 {
    let request = Outbound::admin(new_id(), op);
    exchange(flags, target, &request).await
}

/// Send one admin op to the admin surface, or fail loudly on the client surface.
fn admin(flags: &GlobalFlags, verb: &str, op: AdminOp) -> i32 {
    match admin_surface(flags, verb) {
        Ok(target) => runtime::block_on(run_admin(flags, &target, op)),
        Err(code) => code,
    }
}

/// Resolve the surface and send the request it carries.
fn query(flags: &GlobalFlags, admin: AdminOp, client: ClientOp) -> i32 {
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    let request = outbound(target.surface, admin, client);
    runtime::block_on(exchange(flags, &target, &request))
}

/// `status` reports the server; it needs the admin surface.
pub fn status(flags: &GlobalFlags) -> i32 {
    admin(
        flags,
        "status",
        AdminOp::Status(Value::Object(Default::default())),
    )
}

/// `roles` lists roles, optionally filtered by `--role`.
pub fn roles(flags: &GlobalFlags, args: RolesArgs) -> i32 {
    let filter = QueryRolesArgs { role: args.role };
    query(
        flags,
        AdminOp::Roles(filter.clone()),
        ClientOp::QueryRoles(filter),
    )
}

/// `sessions` lists sessions, mapped onto `query_sessions`.
pub fn sessions(flags: &GlobalFlags, args: SessionsArgs) -> i32 {
    let filter = QuerySessionsArgs {
        task_id: args.task,
        role: args.role,
        lifecycle: args.lifecycle,
        limit: args.limit.unwrap_or_default(),
    };
    query(
        flags,
        AdminOp::Sessions(filter.clone()),
        ClientOp::QuerySessions(filter),
    )
}

/// `ledger` lists ledger rows, mapped onto `query_ledger`.
pub fn ledger(flags: &GlobalFlags, args: LedgerArgs) -> i32 {
    let filter = LedgerQuery {
        task: args.task,
        op_id: args.op_id,
        msg_id: args.msg_id,
        role: args.role,
        state: args.state,
        kind: args.kind,
        limit: args.limit.unwrap_or_default(),
    };
    query(
        flags,
        AdminOp::Ledger(filter.clone()),
        ClientOp::QueryLedger(filter),
    )
}

/// `faults` lists recorded faults.
pub fn faults(flags: &GlobalFlags, args: FaultsArgs) -> i32 {
    let filter = QueryFaultsArgs {
        role: args.role,
        task_id: args.task,
        kind: args.kind,
        open_only: args.open_only,
        limit: args.limit.unwrap_or_default(),
    };
    query(
        flags,
        AdminOp::Faults(filter.clone()),
        ClientOp::QueryFaults(filter),
    )
}

/// `watch` streams event frames; `--follow` keeps the connection open.
pub fn watch(flags: &GlobalFlags, args: WatchArgs) -> i32 {
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    runtime::block_on(async move {
        let tiers = if args.tier.is_empty() {
            vec![EventTier::Durable, EventTier::Advisory]
        } else {
            args.tier
        };
        let subscribe = Subscribe {
            since_seq: args.since.unwrap_or_default(),
            tiers,
            kinds: Vec::new(),
            roles: Vec::new(),
        };
        let request = match target.surface {
            Surface::Admin => Outbound::admin(new_id(), AdminOp::Watch(subscribe)),
            Surface::Client => Outbound::client(new_id(), ClientOp::Subscribe(subscribe)),
        };
        let mut stream = match runtime::open(flags, &target).await {
            Ok(stream) => stream,
            Err(code) => return code,
        };
        if let Err(error) = wire::send_frame(&mut stream, &request, flags.timeout_ms).await {
            return runtime::exchange_error(&error, flags.timeout_ms);
        }
        loop {
            match wire::recv_frame(&mut stream, flags.timeout_ms).await {
                Ok(Frame::Ev { seq, event }) => {
                    let frame: Frame = Frame::Ev { seq, event };
                    println!(
                        "{}",
                        serde_json::to_string(&frame).expect("serialisable frame")
                    );
                }
                Ok(Frame::Res { body, .. }) if !body.ok => {
                    return runtime::finish(&body, flags);
                }
                Ok(Frame::Bye { .. }) => return EXIT_OK,
                Ok(_) => {}
                Err(ExchangeError::Timeout) => {
                    if args.follow {
                        return runtime::exchange_error(&ExchangeError::Timeout, flags.timeout_ms);
                    }
                    return EXIT_OK;
                }
                Err(ExchangeError::Closed) => return EXIT_OK,
                Err(error) => return runtime::exchange_error(&error, flags.timeout_ms),
            }
        }
    })
}

/// `history` replays recorded envelopes; it needs the admin surface.
pub fn history(flags: &GlobalFlags, args: HistoryArgs) -> i32 {
    admin(
        flags,
        "history",
        AdminOp::History(ProtoHistoryArgs {
            since_seq: args.since.unwrap_or_default(),
            limit: args.limit.unwrap_or_default(),
            kind: args.kind,
            task_id: args.task,
        }),
    )
}

/// `spec_diff` diffs the running spec against the configuration on disk.
pub fn spec_diff(flags: &GlobalFlags) -> i32 {
    admin(
        flags,
        "spec_diff",
        AdminOp::SpecDiff(Value::Object(Default::default())),
    )
}

/// `reload` asks the server to re-read its configuration.
pub fn reload(flags: &GlobalFlags) -> i32 {
    admin(
        flags,
        "reload",
        AdminOp::Reload(Value::Object(Default::default())),
    )
}

enum Probe {
    /// `status` answered `ok: true`.
    Ready(ResBody),
    /// `status` answered, but the answer was not `ok: true`.
    NotReady,
    /// No answer; the server is not listening yet.
    Failed,
}

/// Probe the admin `status` op once.
async fn status_probe(target: &SocketTarget, timeout_ms: u64) -> Probe {
    let Ok(mut stream) = wire::connect(&target.path, timeout_ms).await else {
        return Probe::Failed;
    };
    let request = Outbound::admin(new_id(), AdminOp::Status(Value::Object(Default::default())));
    match wire::request_res(&mut stream, &request, timeout_ms).await {
        Ok(body) if body.ok => Probe::Ready(body),
        Ok(_) => Probe::NotReady,
        Err(_) => Probe::Failed,
    }
}

/// `wait-ready` polls `status` every interval until the answer is `ok: true`
/// or the timeout bound elapses.
pub fn wait_ready(flags: &GlobalFlags, args: WaitReadyArgs) -> i32 {
    let Ok(target) = admin_surface(flags, "wait-ready") else {
        return EXIT_VALIDATION;
    };
    runtime::block_on(async move {
        let bound = Duration::from_millis(flags.timeout_ms);
        let interval = Duration::from_millis(args.interval_ms.max(1));
        let started = Instant::now();
        loop {
            match status_probe(&target, flags.timeout_ms).await {
                Probe::Ready(body) => return runtime::finish(&body, flags),
                _ => {
                    if started.elapsed() + interval > bound {
                        break;
                    }
                    tokio::time::sleep(interval).await;
                }
            }
        }
        eprintln!("onlyne: server not ready after {}ms", flags.timeout_ms);
        EXIT_ANSWER_FAILED
    })
}

/// The target wrapper shared by `inspect`, `retry` and `close`.
fn repair_target(args: RepairTargetArgs) -> RepairTarget {
    RepairTarget {
        task_id: args.task,
        reason: args.reason,
    }
}

/// `repair` drives the recovery verbs on the admin surface.
pub fn repair(flags: &GlobalFlags, verb: RepairVerb) -> i32 {
    let op = match verb {
        RepairVerb::Inspect(args) => AdminOp::RepairInspect(repair_target(args)),
        RepairVerb::Adopt(args) => AdminOp::RepairAdopt(RepairAdopt {
            task_id: args.task,
            session_id: args.session_id,
            backend: args.backend,
            backend_ref: serde_json::json!(args.backend_ref),
            reason: args.reason,
        }),
        RepairVerb::Rebind(args) => AdminOp::RepairRebind(RepairRebind {
            task_id: args.task,
            session_id: args.session_id,
            backend: args.backend,
            backend_ref: serde_json::json!(args.backend_ref),
            reason: args.reason,
        }),
        RepairVerb::Retry(args) => AdminOp::RepairRetry(repair_target(args)),
        RepairVerb::Fail(args) => AdminOp::RepairFail(RepairFail {
            task_id: args.task,
            reason: args.reason,
            notify: args.notify.map(|role| Principal::role(&role)),
        }),
        RepairVerb::Close(args) => AdminOp::RepairClose(repair_target(args)),
        RepairVerb::Ack(args) => AdminOp::RepairAck(RepairAck {
            fault_id: args.fault_id,
            reason: args.reason,
        }),
    };
    admin(flags, "repair", op)
}

/// Pull the prose out of a role query answer, in whichever shape it arrives.
fn prose_of(data: &Value, role: &str) -> Option<String> {
    if let Some(prose) = data.get("prose").and_then(Value::as_str) {
        return Some(prose.to_string());
    }
    let entries = data
        .get("roles")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| data.as_array().cloned())
        .unwrap_or_default();
    entries
        .iter()
        .find(|entry| entry.get("role").and_then(Value::as_str) == Some(role))
        .or_else(|| entries.first())
        .and_then(|entry| {
            entry
                .get("prose")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// `cluster export-prose` prints one role's prose, raw by default so it can be
/// pasted into a TOML multi-line string; `--json` wraps it.
pub fn export_prose(flags: &GlobalFlags, args: ExportProseArgs) -> i32 {
    let Some(target) = runtime::target(flags) else {
        return EXIT_NO_SOCKET;
    };
    runtime::block_on(async move {
        let role = args.role.unwrap_or_else(|| flags.local_role());
        let filter = QueryRolesArgs {
            role: Some(role.clone()),
        };
        let request = outbound(
            target.surface,
            AdminOp::Roles(filter.clone()),
            ClientOp::QueryRoles(filter),
        );
        let mut stream = match runtime::open(flags, &target).await {
            Ok(stream) => stream,
            Err(code) => return code,
        };
        let body = match wire::request_res(&mut stream, &request, flags.timeout_ms).await {
            Ok(body) => body,
            Err(error) => return runtime::exchange_error(&error, flags.timeout_ms),
        };
        if !body.ok {
            return runtime::finish(&body, flags);
        }
        let Some(prose) = body.data.and_then(|data| prose_of(&data, &role)) else {
            return runtime::usage_error(format!(
                "onlyne: no prose in the role query answer for role {role}"
            ));
        };
        if flags.json {
            println!(
                "{}",
                serde_json::to_string(&json!({ "role": role, "prose": prose }))
                    .expect("serialisable value")
            );
        } else {
            println!("{prose}");
        }
        EXIT_OK
    })
}
