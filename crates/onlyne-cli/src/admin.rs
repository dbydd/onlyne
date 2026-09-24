//! The bare admin nouns, which talk to the resolved socket like the message verbs.

use onlyne_proto::{
    AdminOp, ClientOp, EventTier, Frame, HistoryArgs as ProtoHistoryArgs, LedgerQuery, LedgerState,
    Lifecycle, MsgKind, QueryFaultsArgs, QueryRolesArgs, QuerySessionsArgs, RepairAck, RepairAdopt,
    RepairFail, RepairRebind, RepairTarget, ResBody, Subscribe, new_id,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

use crate::flags::GlobalFlags;
use crate::runtime::{self, EXIT_ANSWER_FAILED, EXIT_NO_SOCKET, EXIT_OK};
use crate::socket::{SocketTarget, Surface};
use crate::wire::{self, ExchangeError, Outbound};

/// the two things an operator must not confuse from the foot of `onlyne repair
/// --help`: which verb re-points a binding and which one re-bases the row, and
/// who is allowed to move state at all.
const REPAIR_AFTER_HELP: &str = "\
`adopt` rewrites the desired backend binding and keeps the row's session id and
generation. `rebind` rewrites the same binding and moves the row to the
`--session-id` you give, bumping its generation and resetting its seq to 0, so
reports under the old generation stop being believed.
Use `adopt` when the resource is the one the row already names, and `rebind` when
the task is now carried by a different session.

The server detects and records faults; it never repairs them. A fault stays open
until a person or the supervisor runs one of these `repair_*` verbs, so nothing
here is racing an automatic recovery, and a task whose rows have settled past
in flight has no edge back to the queue.";

/// One `repair` verb on the admin surface.
#[derive(Debug, Clone, clap::Subcommand)]
#[command(after_help = REPAIR_AFTER_HELP)]
pub enum RepairVerb {
    /// Print one task's session projection with every fault recorded against it.
    Inspect(RepairTargetArgs),
    /// Re-point the row's desired backend binding without touching its generation.
    Adopt(RepairAdoptArgs),
    /// Move the row to another session id, bumping its generation and resetting seq.
    Rebind(RepairRebindArgs),
    /// Put the task's still in flight rows back in the queue, dropping their tickets.
    /// A task whose own row has settled is refused: the ledger keeps no edge from a
    /// terminal state, and re-running finished work means sending a new task.
    Retry(RepairTargetArgs),
    /// Settle the task as failed, rejecting its undelivered rows and cancelling its owner.
    Fail(RepairFailArgs),
    /// Settle the task as cancelled, with the same row sweep and cancel as `fail`.
    Close(RepairTargetArgs),
    /// Close one fault record by id as handled, leaving ledger rows alone.
    Ack(RepairAckArgs),
}

#[derive(Debug, Clone, clap::Args)]
pub struct RepairTargetArgs {
    #[arg(long)]
    pub task: String,
    /// Recorded as the reason on the faults the verb moves; `retry` and `close`
    /// substitute their own default text when it is omitted, and `inspect` records
    /// nothing, so the value goes unread there.
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct RepairAdoptArgs {
    #[arg(long)]
    pub task: String,
    /// Backend name written into the row's desired binding.
    #[arg(long)]
    pub backend: String,
    /// Handle the backend uses for this session. A spelling that parses as JSON
    /// travels as its parsed value; other text travels as one JSON string; an
    /// omitted flag travels as null.
    #[arg(long)]
    pub backend_ref: Option<String>,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Clone, clap::Args)]
pub struct RepairRebindArgs {
    #[arg(long)]
    pub task: String,
    /// Written into the row as its session id, beside the generation bump.
    #[arg(long)]
    pub session_id: String,
    /// Backend name written into the row's desired binding.
    #[arg(long)]
    pub backend: String,
    /// Handle the backend uses for this session. A spelling that parses as JSON
    /// travels as its parsed value; other text travels as one JSON string; an
    /// omitted flag travels as null.
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
    /// Role name to keep, matched against the spec entry. Omitting it lists
    /// every registered role.
    #[arg(long)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct SessionsArgs {
    /// Task whose session row is kept; the session table is keyed by task.
    #[arg(long)]
    pub task: Option<String>,
    /// Role owning the session row.
    #[arg(long)]
    pub role: Option<String>,
    /// Public lifecycle to keep: `created`, `working`, `idle`, `exited`.
    #[arg(long, value_parser = parse_lifecycle)]
    pub lifecycle: Option<Lifecycle>,
    /// Rows to print. Omitted, or `0`, asks for the server default of 100; the
    /// server reads at most 500.
    #[arg(long)]
    pub limit: Option<u32>,
    /// Ask the task's owning client to probe its plugin, and answer with the
    /// observation that probe produced. Needs `--task`, and lives inside
    /// `--timeout`: a probe that does not land answers the stored row with a
    /// `fresh` marker instead of blocking past the bound.
    #[arg(long)]
    pub fresh: bool,
}

#[derive(Debug, Clone, clap::Args)]
pub struct LedgerArgs {
    /// Task family whose rows are kept, matched against the row's own task.
    #[arg(long)]
    pub task: Option<String>,
    /// One envelope id, keeping exactly the row that send wrote.
    #[arg(long)]
    pub msg_id: Option<String>,
    /// One operation id, keeping every row that send attempt carries it. The
    /// server dedups an idempotent send on this column.
    #[arg(long)]
    pub op_id: Option<String>,
    /// Role on either end of the hop, keeping rows it sent or received.
    #[arg(long)]
    pub role: Option<String>,
    /// Settled state to keep: `queued` for a recipient not connected yet,
    /// `in_flight` for one handed out awaiting `ack`, `acked`, `rejected` for a
    /// refusal at the gate or an exhausted requeue budget, `expired` for a note
    /// or row past its age limit.
    #[arg(long, value_parser = parse_ledger_state)]
    pub state: Option<LedgerState>,
    /// Delivery intent to keep: `task`, `completion`, `note`, `control`.
    #[arg(long, value_parser = parse_msg_kind)]
    pub kind: Option<MsgKind>,
    /// Rows to print, newest enqueue first. Omitted, or `0`, asks for the server
    /// default of 100; the server reads at most 500.
    #[arg(long)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct FaultsArgs {
    /// Role the fault was recorded against.
    #[arg(long)]
    pub role: Option<String>,
    /// Task the fault was recorded for.
    #[arg(long)]
    pub task: Option<String>,
    /// Recorded fault kind, matched verbatim. The server stores whatever the
    /// reporter named, so `probe_dead`, `mismatch_terminate`, and
    /// `intent_exhausted` are the names this filter sees.
    #[arg(long)]
    pub kind: Option<String>,
    /// Keep rows still wanting a decision, dropping the settled ones.
    #[arg(long)]
    pub open_only: bool,
    /// Rows to print, oldest recording first. Omitted, or `0`, asks for the
    /// server default of 100; the server reads at most 500.
    #[arg(long)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct GhostsArgs {
    /// Audit rows to print, newest sweep first. Omitted, or `0`, asks for the
    /// server default of 100; the server reads at most 500.
    #[arg(long)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct WatchArgs {
    /// Event cursor to resume after. Omitting it, or `0`, starts at the current
    /// head, so the stream carries only what follows the handshake.
    #[arg(long)]
    pub since: Option<u64>,
    /// Replay tier to subscribe to; repeatable, every tier when omitted.
    /// `durable` rows are persisted and replayable from a cursor; `advisory`
    /// rows are best effort, and a lagging subscriber resyncs by querying.
    #[arg(long, value_parser = parse_tier)]
    pub tier: Vec<EventTier>,
    /// Keep the connection open and print one JSON line per event frame.
    #[arg(long)]
    pub follow: bool,
}

#[derive(Debug, Clone, clap::Args)]
pub struct HistoryArgs {
    /// Event cursor to read after. Omitting it, or `0`, reads from the first
    /// retained event.
    #[arg(long)]
    pub since: Option<u64>,
    /// Events to read before filtering. Omitted, or `0`, asks for the replay
    /// default of 256; the store reads at most 500.
    #[arg(long)]
    pub limit: Option<u32>,
    /// Task named by the event, keeping `session_state` rows for it, the
    /// `ledger_state` rows it owns, and the `fault` rows raised against it. An
    /// event carrying no task never matches.
    #[arg(long)]
    pub task: Option<String>,
    /// Event type name to keep: `role_presence`, `session_state`,
    /// `ledger_state`, `fault`, `gateway_presence`, `spec_reloaded`.
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

/// Share of one read's `--timeout` kept for the frames around the probe: the
/// request out, the client's control round trip, and the answer back.
///
/// The server's wait is bounded by what is left, which is what keeps a fresh
/// read inside the bound the operator set. The reserve does not grow with
/// `--timeout`, and the wait is never widened past it to give the probe more
/// room than the operator allowed.
const FRESH_RESERVE_MS: u64 = 250;

/// The probe wait one `--fresh` read carries: the read's own bound less the
/// frames it has to travel in. `None` when that bound cannot hold a frame.
fn fresh_wait_ms(timeout_ms: u64) -> Option<u64> {
    let wait = timeout_ms.saturating_sub(FRESH_RESERVE_MS);
    (wait > 0).then_some(wait)
}

/// `sessions` lists sessions, mapped onto `query_sessions`.
pub fn sessions(flags: &GlobalFlags, args: SessionsArgs) -> i32 {
    let fresh_wait_ms = match (args.fresh, args.task.is_some()) {
        (false, _) => None,
        // A read that asks nobody is the mirror under a name that says fresh.
        (true, false) => {
            return runtime::usage_error(
                "onlyne: --fresh needs --task; a fresh read asks one task's client".to_string(),
            );
        }
        (true, true) => match fresh_wait_ms(flags.timeout_ms) {
            Some(wait) => Some(wait),
            None => {
                return runtime::usage_error(format!(
                    "onlyne: --fresh needs a --timeout above {FRESH_RESERVE_MS}ms; the probe \
                     wait lives inside that bound"
                ));
            }
        },
    };
    let filter = QuerySessionsArgs {
        task_id: args.task,
        role: args.role,
        lifecycle: args.lifecycle,
        limit: args.limit.unwrap_or_default(),
        fresh_wait_ms,
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

/// `ghosts` lists the ghost sweep's audit rows; it needs the admin surface.
pub fn ghosts(flags: &GlobalFlags, args: GhostsArgs) -> i32 {
    admin(
        flags,
        "ghosts",
        AdminOp::QueryGhostSweeps(args.limit.unwrap_or_default() as usize),
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
    let target = match admin_surface(flags, "wait-ready") {
        Ok(target) => target,
        Err(code) => return code,
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

/// The wire value for `--backend-ref`: a spelling that parses as JSON travels as
/// the parsed value, any other text travels as one JSON string, and an omitted
/// flag travels as null.
fn backend_ref_value(raw: Option<String>) -> Value {
    match raw {
        Some(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        None => Value::Null,
    }
}

/// `repair` drives the recovery verbs on the admin surface.
pub fn repair(flags: &GlobalFlags, verb: RepairVerb) -> i32 {
    let op = match verb {
        RepairVerb::Inspect(args) => AdminOp::RepairInspect(repair_target(args)),
        RepairVerb::Adopt(args) => AdminOp::RepairAdopt(RepairAdopt {
            task_id: args.task,
            backend: args.backend,
            backend_ref: backend_ref_value(args.backend_ref),
            reason: args.reason,
        }),
        RepairVerb::Rebind(args) => AdminOp::RepairRebind(RepairRebind {
            task_id: args.task,
            session_id: args.session_id,
            backend: args.backend,
            backend_ref: backend_ref_value(args.backend_ref),
            reason: args.reason,
        }),
        RepairVerb::Retry(args) => AdminOp::RepairRetry(repair_target(args)),
        RepairVerb::Fail(args) => AdminOp::RepairFail(RepairFail {
            task_id: args.task,
            reason: args.reason,
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

#[cfg(test)]
mod tests {
    use super::backend_ref_value;
    use serde_json::{Value, json};

    #[test]
    fn an_object_spelling_travels_as_an_object() {
        let value = backend_ref_value(Some(r#"{"id": "p-7", "pane": 3}"#.to_string()));
        assert_eq!(value["id"], json!("p-7"));
        assert_eq!(value["pane"], json!(3));
    }

    #[test]
    fn a_plain_word_travels_as_one_json_string() {
        assert_eq!(
            backend_ref_value(Some("term_1".to_string())),
            json!("term_1")
        );
    }

    #[test]
    fn an_omitted_flag_travels_as_null() {
        assert_eq!(backend_ref_value(None), Value::Null);
    }
}
