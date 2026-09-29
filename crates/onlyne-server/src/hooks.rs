//! Event hooks: operator policy outside the delivery path
//! (`docs/v2-CONTRACT.md` §"Slice 7", `AGENTS.md` §14).
//!
//! A `[[hook]]` entry names event classes from the closed set and a command to
//! run when one of them is persisted. The server spawns that command with the
//! event JSON (including `seq`) on stdin and `ONLYNE_SOCKET` pointing at the
//! admin socket, so a script can act without a second discovery step.
//!
//! Delivery is **at-least-once**, and the cursor is what makes that true. Each
//! hook records the last `seq` it handled successfully in the durable
//! `hook_cursors` table, a restarted worker resumes from there, and a script
//! deduplicates on `seq`. Two positions, and the difference between them is
//! the whole of the rule:
//!
//! * The **durable cursor** advances only when a script exits `0`, and then
//!   only forward. It is the last event a hook actually handled.
//! * A pass reads the rows after the cursor in `seq` order and **stops at the
//!   first matching failure**, so the events behind a failed one are still
//!   owed. Advancing past a failure would drop it: the row would sit below the
//!   cursor and no restart would ever offer it again.
//!
//! A failure is recorded once per event — a nonzero exit or the bound elapsing
//! — as a `hook_failed` fault, and the original event is left untouched: a hook
//! is policy, and policy failing must not rewrite history. The hook then
//! retries that event after [`RETRY_PAUSE`], because a hook that quietly
//! stopped serving is worse than one that reports and tries again.
//!
//! A hook never stalls the delivery path: [`Server::emit`] appends, broadcasts,
//! and publishes the new head to [`HookHead`] without waiting on any worker.
//! Each hook runs on its own task, so a slow or wedged script delays nothing
//! but its own backlog — not the event that follows it on the stream, and not
//! another hook bound to the same event.

use crate::faults::{FaultDraft, KIND_HOOK_FAILED, record};
use crate::state::Server;
use onlyne_config::{HookEntry, parse_hook_timeout};
use onlyne_wire::socket::socket_path;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

/// How many event rows one read takes. A larger backlog is a loop, not a
/// bigger query.
const HOOK_BATCH: u32 = 256;

/// Pause before a hook retries the event it failed on. The stream is not
/// waiting on this — only the hook's own backlog is — so the value buys a
/// wedge-prone script room without spinning up processes in a tight loop.
pub const RETRY_PAUSE: Duration = Duration::from_secs(1);

/// Fallback bound for a hook whose `timeout` did not parse. The loader refuses
/// such a spec, so this is unreachable in a served cluster and exists only so a
/// hand-built state in a test cannot leave a script unbounded.
const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 10;

/// The newest event `seq` this process appended, published to the hook workers.
///
/// A watch channel rather than a `Notify`: a notification issued while every
/// worker is busy at its script would be dropped, and the event that woke
/// nothing would then never be looked at. A watch carries the value, so a
/// worker that was mid-script sees the newer head the moment it checks, and
/// [`Server::emit`] pays one `send` with no await
/// (`docs/v2-CONTRACT.md` §"Slice 7").
#[derive(Debug)]
pub struct HookHead {
    head: watch::Sender<u64>,
}

impl HookHead {
    pub fn new(head: u64) -> Self {
        Self {
            head: watch::Sender::new(head),
        }
    }

    /// Publish the newest appended `seq`. Non-blocking and infallible: a send
    /// fails only when no worker is listening, which changes nothing.
    pub fn publish(&self, seq: u64) {
        let _ = self.head.send(seq);
    }

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.head.subscribe()
    }
}

/// The stable identity of one hook declaration, and the `hook_cursors` key.
///
/// Built from the bound classes (sorted) and the command argv: reordering
/// `[[hook]]` entries keeps every cursor, while a hook whose classes or command
/// changed is a different policy and starts fresh. `timeout` is excluded on
/// purpose — a bound the operator retuned does not un-handle the events the
/// script already saw.
pub fn hook_key(entry: &HookEntry) -> String {
    let mut classes = entry.on.clone();
    classes.sort();
    format!("{}|{}", classes.join(","), entry.run.join(" "))
}

/// Spawn one worker per declared hook.
///
/// The set is read when the server starts, so a `[[hook]]` edit takes effect on
/// the next start; `reload_spec` names a changed set in the log rather than
/// pretending a running worker changed its policy.
pub fn spawn_workers(state: &Arc<Server>) -> Vec<JoinHandle<()>> {
    let Some(spec) = state.spec_snapshot() else {
        return Vec::new();
    };
    let socket = socket_path(&state.root).ok();
    spec.hook
        .iter()
        .map(|entry| {
            spawn_one(
                state.clone(),
                entry.clone(),
                socket.clone(),
                state.hook_head.subscribe(),
            )
        })
        .collect()
}

fn spawn_one(
    state: Arc<Server>,
    entry: HookEntry,
    socket: Option<PathBuf>,
    mut head: watch::Receiver<u64>,
) -> JoinHandle<()> {
    let key = hook_key(&entry);
    let bound = parse_hook_timeout(&entry.timeout)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_HOOK_TIMEOUT_SECS));
    tokio::spawn(async move {
        // A hook with a durable cursor is resuming and reads from there. One
        // this cluster has never run starts at the head it sees: a policy an
        // operator just declared applies from now on, and the server's retained
        // history is not a backlog the hook owes a run through.
        let mut scan = match state.ledger.hook_cursor(&key) {
            Ok(Some(seq)) => seq,
            Ok(None) => state.event_head().max(0),
            Err(error) => {
                tracing::warn!(error = %error, hook = %key, "hook cursor unreadable; starting at the head");
                state.event_head().max(0)
            }
        };
        // The fell seq whose failure this process already reported. One fault
        // per event per process: a retry that fails again is the same fact, and
        // a stream of identical faults would bury the first one.
        let mut reported: Option<i64> = None;
        loop {
            let mut read_error = false;
            while scan < head_now(&head) {
                match pass(
                    &state,
                    &entry,
                    &key,
                    socket.as_deref(),
                    bound,
                    scan,
                    &mut reported,
                )
                .await
                {
                    Pass::Reached(next) => {
                        if next <= scan {
                            // The read returned no rows past this position, so
                            // the ledger has nothing more to offer.
                            break;
                        }
                        scan = next;
                    }
                    Pass::Failed { at } => {
                        // The event is owed, so the pass stops here: everything
                        // behind it is still owed too, and the cursor stays
                        // where the last success left it. The scan goes back to
                        // the position *before* the failed event — `events_since`
                        // is exclusive — so the retry is offered that same event
                        // instead of stepping over it.
                        scan = at - 1;
                        sleep(RETRY_PAUSE).await;
                    }
                    Pass::Unreadable => {
                        read_error = true;
                        break;
                    }
                }
            }
            if read_error {
                // A ledger the reader cannot open is not a reason to spin.
                sleep(RETRY_PAUSE).await;
            }
            if head.changed().await.is_err() {
                // The server dropped the channel, so nothing will be published
                // again and this hook has nothing left to wait for.
                return;
            }
        }
    })
}

/// The head this worker last saw, as the `i64` the ledger counts in.
fn head_now(head: &watch::Receiver<u64>) -> i64 {
    let seen = *head.borrow();
    seen.min(i64::MAX as u64) as i64
}

/// What one pass over the backlog did.
enum Pass {
    /// The scan reached this `seq`; the caller resumes past it.
    Reached(i64),
    /// A matching event failed. `at` is its `seq`, which the pass stopped on.
    Failed { at: i64 },
    /// The events could not be read at all.
    Unreadable,
}

/// One ordered pass over the events after `from`.
///
/// Rows are read in `seq` order and a row whose class the hook does not name is
/// skipped: a hook never runs for a class it does not name, and the row's
/// `type` column is the class. The pass stops at the first matching failure so
/// the cursor stays a contiguous prefix of handled events.
async fn pass(
    state: &Server,
    entry: &HookEntry,
    key: &str,
    socket: Option<&Path>,
    bound: Duration,
    from: i64,
    reported: &mut Option<i64>,
) -> Pass {
    let rows = match state.ledger.events_since(from, HOOK_BATCH) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(error = %error, hook = %key, "hook events read failed");
            return Pass::Unreadable;
        }
    };
    let mut reached = from;
    for row in &rows {
        reached = reached.max(row.seq);
        if !entry.on.iter().any(|class| class == row.kind.as_str()) {
            continue;
        }
        // The hook is handed the class's own payload as `data`, not the
        // envelope the ledger keeps: `data_json` holds the tagged enum the wire
        // carries (`{"type": <class>, "data": {...}}`) because the history op
        // decodes it back into an `Event`, and a script made to read
        // `.data.data.task_id` would be reading our storage instead of the
        // event. What it gets is `{seq, type, data, created_at}`, where `data`
        // is what the publisher sent — the client's payload for a settlement
        // class, the server's own for the rest.
        let payload = serde_json::json!({
            "seq": row.seq,
            "type": row.kind,
            "data": row.data["data"].clone(),
            "created_at": row.created_at,
        });
        match run_one(entry, socket, bound, &payload).await {
            Ok(()) => {
                if let Err(error) = state.ledger.set_hook_cursor(key, row.seq) {
                    // The event was handled but the position was not recorded,
                    // so a restart offers it again. That is the safe direction:
                    // a script deduplicates on `seq`.
                    tracing::warn!(error = %error, hook = %key, seq = row.seq, "hook cursor not recorded");
                }
            }
            Err(reason) => {
                if *reported != Some(row.seq) {
                    *reported = Some(row.seq);
                    record_failure(state, entry, key, &row.kind, row.seq, &row.data, &reason);
                } else {
                    tracing::debug!(hook = %key, seq = row.seq, reason = %reason, "hook retry failed again");
                }
                return Pass::Failed { at: row.seq };
            }
        }
    }
    Pass::Reached(reached)
}

/// Record a hook that exited nonzero or passed its bound.
///
/// The event row is not touched: the fault is the record, and the durable
/// cursor stays where the last success left it, so the event is offered again.
fn record_failure(
    state: &Server,
    entry: &HookEntry,
    key: &str,
    class: &str,
    seq: i64,
    data: &Value,
    reason: &str,
) {
    let mut draft = FaultDraft::new(
        KIND_HOOK_FAILED,
        format!(
            "hook '{}' on {class} failed for event seq {seq}: {reason}",
            entry.run.join(" "),
        ),
    )
    .with_seq(seq as u64);
    if let Some(task_id) = data.get("task_id").and_then(Value::as_str) {
        draft = draft.with_task(task_id);
    }
    if let Some(role) = data.get("role").and_then(Value::as_str) {
        draft = draft.with_role(role);
    }
    if let Err(error) = record(state, draft) {
        tracing::warn!(error = %error, hook = %key, seq = seq, "hook failure not recorded as a fault");
    }
}

/// Spawn one hook script for one event, with the bound as its ceiling.
///
/// `Ok(())` is a zero exit; `Err` is a nonzero exit, a spawn that failed, or the
/// bound elapsing, and the reason is the sentence the fault carries.
async fn run_one(
    entry: &HookEntry,
    socket: Option<&Path>,
    bound: Duration,
    payload: &Value,
) -> Result<(), String> {
    let (program, args) = entry
        .run
        .split_first()
        .ok_or_else(|| "the hook declares no command".to_string())?;
    let json = serde_json::to_string(payload).map_err(|error| format!("encode event: {error}"))?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        // The script is the worker's child, so a worker that ends at shutdown
        // takes its script with it rather than leaving a process nobody owns.
        .kill_on_drop(true);
    if let Some(socket) = socket {
        // The script can act without a second discovery step: it dials the
        // admin socket the cluster already trusts.
        cmd.env("ONLYNE_SOCKET", socket);
    }
    let mut child = cmd
        .spawn()
        .map_err(|error| format!("spawn {program}: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // The event arrives as one JSON object on stdin. A script that reads
        // nothing is still a script that exits 0; only its stdin is written.
        if let Err(error) = stdin.write_all(json.as_bytes()).await {
            return Err(format!("write event: {error}"));
        }
        let _ = stdin.shutdown().await;
    }
    match timeout(bound, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(match status.code() {
            Some(code) => format!("exit code {code}"),
            None => "killed by a signal".to_string(),
        }),
        Ok(Err(error)) => Err(format!("wait: {error}")),
        Err(_) => {
            // Past the bound: kill the child here rather than at drop, so the
            // worker moves on without waiting out a wedged script's own
            // cleanup.
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(format!("timeout after {bound:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(on: &[&str], run: &[&str], timeout: &str) -> HookEntry {
        HookEntry {
            on: on.iter().map(|class| class.to_string()).collect(),
            run: run.iter().map(|arg| arg.to_string()).collect(),
            timeout: timeout.to_string(),
        }
    }

    /// The key is the declaration's content, so reordering the spec's entries
    /// keeps every cursor and a changed command is a new hook.
    #[test]
    fn the_hook_key_is_the_declaration_not_its_position() {
        let a = entry(&["fault", "delivery_blocked"], &["./a.sh"], "10s");
        let reordered = entry(&["delivery_blocked", "fault"], &["./a.sh"], "10s");
        assert_eq!(hook_key(&a), hook_key(&reordered));
        let other_command = entry(&["fault", "delivery_blocked"], &["./b.sh"], "10s");
        assert_ne!(hook_key(&a), hook_key(&other_command));
        let other_class = entry(&["fault", "handoff"], &["./a.sh"], "10s");
        assert_ne!(hook_key(&a), hook_key(&other_class));
    }

    /// The bound is not part of the identity: retuning how long a script may
    /// run does not un-handle what it already saw.
    #[test]
    fn retuning_the_bound_keeps_the_cursor() {
        let before = entry(&["fault"], &["./a.sh"], "10s");
        let after = entry(&["fault"], &["./a.sh"], "60s");
        assert_eq!(hook_key(&before), hook_key(&after));
    }

    /// The class set the config accepts is the class set the server publishes,
    /// pinned here because both crates are visible in this one. A drift means a
    /// hook that can be declared and can never fire, or an event no operator can
    /// bind to (`docs/v2-CONTRACT.md` §"Slice 7").
    #[test]
    fn the_declarable_classes_are_the_published_ones() {
        let mut declared = onlyne_config::HOOK_EVENT_CLASSES.to_vec();
        declared.sort_unstable();
        let mut published: Vec<&str> = Vec::new();
        published.extend(onlyne_proto::CLIENT_EVENT_CLASSES);
        published.extend([
            "ledger_state",
            "session_state",
            "role_presence",
            "fault",
            "gateway_presence",
            "spec_reloaded",
        ]);
        published.sort_unstable();
        assert_eq!(declared, published);
    }
}
