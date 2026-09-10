use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;

use crate::sched::{self, Sched};

/// Deliver a swarm wire through the workspace daemon's Unix socket.
/// The daemon persists an idempotent receipt under `op_id`.
pub fn send_loopback_rpc(
    sched: &Arc<Sched>,
    task_id: &str,
    ws: &Path,
    wire: &str,
) -> anyhow::Result<serde_json::Value> {
    let sock = crate::root::onlyne_sock(ws);
    let op_id = format!("swarm:{task_id}:{}", stable_wire_id(wire));
    let request = serde_json::json!({"id": op_id, "op": "loopback", "op_id": op_id, "text": wire, "raw_text": true});
    let mut stream = UnixStream::connect(&sock)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    writeln!(stream, "{request}")?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    let value: serde_json::Value = serde_json::from_str(&response)?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        anyhow::bail!(
            "loopback RPC rejected: {}",
            value.get("error").cloned().unwrap_or(value)
        );
    }
    let _ = sched;
    Ok(value.get("data").cloned().unwrap_or(value))
}

fn stable_wire_id(wire: &str) -> String {
    let mut hash = 14695981039346656037u64;
    for byte in wire.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:016x}")
}

/// Serve swarm.sock: line-delimited JSON requests, one-line responses.
/// `subscribe` upgrades the connection to an event stream.
///
/// Shutdown contract: Ctrl-C sets `sched.shutdown` (pump/reaper threads
/// observe it and exit), stops managed daemons, then returns. The
/// listener is dropped here so no new connections arrive during teardown.
pub async fn serve(root: &Path) -> anyhow::Result<()> {
    let sock = crate::root::swarm_sock(root);
    if sock.exists() {
        // Take over a stale socket; refuse if a live scheduler answers.
        match UnixStream::connect(&sock) {
            Ok(_) => anyhow::bail!("swarm scheduler already running at {}", sock.display()),
            Err(_) => {
                let _ = std::fs::remove_file(&sock);
            }
        }
    }
    let sched = Sched::new(root.to_path_buf(), crate::db::Db::open(root)?);
    // Ctrl-C / SIGTERM path: set the shutdown flag so pump watch threads
    // and the reaper loop exit, then break the accept loop by closing the
    // listener. A signal-hook watcher thread (not tokio::signal: the
    // multi-thread runtime's signal driver can stall here when blocking
    // client threads hold the shared lock — observed: SIGTERM worked but
    // SIGINT never fired the tokio handler) flips an AtomicBool that the
    // async serve loop polls.
    let _ = sched.clone(); // Sched.shutdown is set from the accept loop bridge below.
    let shutdown_fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_fired_watcher = shutdown_fired.clone();
    // SAFETY: handler only performs an atomic store (async-signal-safe).
    unsafe {
        let _ = signal_hook::low_level::register(signal_hook::consts::SIGINT, move || {
            shutdown_fired_watcher.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let shutdown_fired_term = shutdown_fired.clone();
        let _ = signal_hook::low_level::register(signal_hook::consts::SIGTERM, move || {
            shutdown_fired_term.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    }
    let shutdown_flag = shutdown_fired.clone();
    // Reconcile the session ledger against the live backend before touching the
    // task ledger: this is the boot-time full pass that adopts provably-live
    // generations and fails quarantined/lost work into the fault queue. It runs
    // first so `reap_previous_run` (task-only, no session rows) sees a settled
    // session view and does not double-handle an already-adopted generation.
    let boot = crate::reconcile::startup_reconcile(&sched);
    tracing::info!(?boot, "boot reconcile");
    // Reap orphan pending tasks from a previous run: terminals are gone,
    // so they count as early exits (failed ledger rows, no replay).
    reap_previous_run(&sched);
    let listener = UnixListener::bind(&sock)?;
    // Spawn the daemon event-subscription pump (priority consumer on each run/s).
    {
        let s = sched.clone();
        std::thread::spawn(move || crate::events::pump(s));
    }
    // Spawn idle-pool + ready-timeout reaper.
    {
        let s = sched.clone();
        std::thread::spawn(move || reap_loop(s));
    }
    // tokio::select on a blocking accept() cannot observe the shutdown
    // flag (accept has no timeout). Poll it with a 100ms timeout so
    // Ctrl-C breaks the loop promptly after the flag is set.
    listener.set_nonblocking(true)?;
    let serve_result = loop {
        // Bridge the signal-hook flag into the Sched flag the pump/reaper
        // threads observe.
        if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
            sched
                .shutdown
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if sched.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            break Ok(());
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let s = sched.clone();
                std::thread::spawn(move || {
                    let _ = handle_conn(s, stream);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => break Err::<(), _>(e.into()),
        }
    };
    // Graceful exit leaves no stale socket: without this `status` in the
    // stopped window reports ECONNREFUSED instead of "no scheduler".
    // Next start's stale-takeover still exists as a crash backstop.
    let _ = std::fs::remove_file(crate::root::swarm_sock(root));
    serve_result
}

fn reap_previous_run(sched: &Arc<Sched>) {
    let Ok(rows) = sched.db.list(None, 1000) else {
        return;
    };
    for r in rows {
        if r.state != crate::db::TaskState::Running && r.state != crate::db::TaskState::Pending {
            continue;
        }
        // R5: adopt-before-kill. The scheduler is a foreground process and
        // `stop_all` only reaches daemons it spawned; worker pi sessions
        // routinely survive a restart (their daemon lives under the session).
        // Only declare the task dead when its terminal is provably gone —
        // otherwise re-adopt the handle so a later out is still recorded.
        // A dropped out on a terminal row is now also emitted (R5.3).
        if !r.terminal.is_empty()
            && !r.terminal.starts_with("stub-")
            && sched::session_alive(sched, &r.task_id, &r.terminal)
        {
            sched
                .terminals
                .lock()
                .unwrap()
                .insert(r.task_id.clone(), r.terminal.clone());
            if r.terminal.starts_with("stub-") {
                sched.sessions.lock().unwrap().insert(
                    r.task_id.clone(),
                    crate::runtime::SessionRef {
                        task_id: r.task_id.clone(),
                        backend: "stub".into(),
                        backend_ref: serde_json::json!({"handle": r.terminal.clone()}),
                        generation: 1,
                    },
                );
            }
            sched
                .running_since
                .lock()
                .unwrap()
                .insert(r.task_id.clone(), std::time::Instant::now());
            // R4: adoption lands straight in busy (CR2: probe-alive is
            // work evidence; later idle/busy reports self-correct).
            sched.db.set_hop(&r.task_id, "busy").ok();
            sched.hop_since.lock().unwrap().insert(
                r.task_id.clone(),
                ("busy".into(), std::time::Instant::now()),
            );
            sched.emit(
                "hop_state",
                serde_json::json!({"task_id": r.task_id, "from_state": "", "to_state": "busy"}),
            );
            sched
                .db
                .set_ledger(
                    &r.task_id,
                    "adopted",
                    "",
                    "scheduler restarted; live session re-adopted",
                )
                .ok();
            crate::db::append_ledger_line(
                &sched.root,
                &crate::db::LedgerEvent {
                    task_id: r.task_id.clone(),
                    transfer_send_to: r.transfer_send_to.clone(),
                    from_ws: r.from_ws.clone(),
                    to_ws: r.to_ws.clone(),
                    state: "adopted".into(),
                    out_head: String::new(),
                    reason: "scheduler restarted; live session re-adopted".into(),
                },
            );
            sched.emit(
                "task_adopted",
                serde_json::json!({"task_id": r.task_id, "to": r.to_ws, "terminal_handle": r.terminal}),
            );
            sched.note_alert(format!(
                "adopted: {} {}",
                &r.task_id[..8.min(r.task_id.len())],
                r.to_ws
            ));
            continue;
        }
        let _ = sched::on_early_exit(sched, &r.task_id, "scheduler restarted");
    }
}

/// One dead-terminal sweep iteration, extracted for unit testing.
/// Returns (failed_task_ids, retried_task_ids).
///
/// Retry policy is root-configurable: `.onlyne/swarm.workspace.jsonc` may set
/// `"retry": {"max_attempts": 0}` for rings the operator wants retried
/// without a cap (max_attempts 0 = unbounded, each failure replays the task
/// unchanged with attempt+1). Absent config keeps MAX_ATTEMPTS = 3.
fn sweep_dead_terminals(sched: &Arc<Sched>) -> (Vec<String>, Vec<String>) {
    const MAX_ATTEMPTS: u32 = 3;
    let max_attempts: Option<u32> = sched.root_retry_max_attempts();
    let mut failed = vec![];
    let mut retried = vec![];
    // Dead-terminal sweep: a Running task whose orca tab is gone (pi
    // exited without writing out) would otherwise sit in running
    // forever. Detect via `terminal show`, record failed, and requeue
    // with attempt+1 so the hop is retried in a fresh terminal.
    // Attempt cap lives here, not in dispatch. A 15s delivery grace avoids
    // racing a clean stub/session exit against the daemon's out event relay.
    const DELIVERY_GRACE: std::time::Duration = std::time::Duration::from_secs(15);
    let since = sched.running_since.lock().unwrap().clone();
    let running: Vec<(String, String, u32)> = sched
        .db
        .list(Some("running"), 200)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| {
            !r.terminal.is_empty()
                && !r.terminal.starts_with("stub-")
                && since
                    .get(&r.task_id)
                    .map(|t| t.elapsed() >= DELIVERY_GRACE)
                    .unwrap_or(false)
        })
        .map(|r| (r.task_id.clone(), r.terminal.clone(), r.attempt))
        .collect();
    for (task_id, handle, attempt) in running {
        if sched::session_alive(sched, &task_id, &handle) {
            continue;
        }
        // Terminal dead, no out: failed + ledger, then retry or drop.
        let _ = sched::on_early_exit(sched, &task_id, "terminal exited before out");
        failed.push(task_id.clone());
        if max_attempts
            .map(|m| attempt < m)
            .unwrap_or(attempt < MAX_ATTEMPTS)
        {
            if let Ok(Some(task)) = sched.db.get(&task_id) {
                let retry_id = uuid::Uuid::new_v4().to_string();
                if sched
                    .db
                    .insert_task(
                        &retry_id,
                        &task.from_ws,
                        &task.to_ws,
                        &task.transfer_send_to,
                        attempt + 1,
                        &task.payload,
                    )
                    .unwrap_or(false)
                {
                    sched.emit(
                        "task_retried",
                        serde_json::json!({
                            "task_id": retry_id,
                            "from_failed": task_id,
                            "attempt": attempt + 1,
                        }),
                    );
                    // Best effort: orca may be unreachable in tests.
                    let _ = sched::dispatch_public(&sched, &retry_id, &task.to_ws);
                    retried.push(retry_id);
                }
            }
        } else {
            tracing::warn!(task = %task_id, attempt, max_attempts = ?max_attempts, "terminal dead, attempt cap reached; not retrying");
        }
    }
    (failed, retried)
}

fn reap_loop(sched: Arc<Sched>) {
    loop {
        // Shutdown gate first: without this the 10s sleep keeps the
        // process alive forever after Ctrl-C (the reported hang).
        if sched.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
        if sched.shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        // Idle pool TTL (R4: owned by the hop state machine; see §4).
        // Pure-parking panes (no associated hop) expire silently; panes
        // backing an idle hop expire as a `swarm_idle timeout` failure.
        {
            let mut idle = sched.idle.lock().unwrap();
            for handles in idle.values_mut() {
                handles.retain(|t| t.since.elapsed() < std::time::Duration::from_secs(60));
            }
        }
        // Dead-terminal sweep lives in sweep_dead_terminals (unit-tested).
        let (_failed, _retried) = sweep_dead_terminals(&sched);
        // Recover ready events written while the event subscription missed
        // them. Run before timeout checks so a healthy pane is not judged
        // stalled merely because its daemon event was lost.
        replay_waiting_ready(&sched);
        // Persistent busy rows are not trusted solely because their tab was
        // alive at boot. Orca/session processes can vanish while the scheduler
        // stayed up; reconcile their exact handles before timeout reporting.
        reconcile_busy_terminals(&sched);
        // Session-ledger reconcile: probe every open session row, mirror the
        // task ledger's terminal state, and drive the reducer's isolate
        // (m) / terminate (n) ladder. Independent of the task-row sweep above.
        crate::reconcile::periodic_reconcile(&sched);
        // R4 hop timeouts: ready-write guard (fixed 30s), dispatched
        // handshake guard (configurable, default 120s), idle stall guard
        // (default 60s), busy overlong reporter (per-role, default off).
        hop_timeouts(&sched);
    }
}

/// R4: replay handshake-waiting task handles from every workspace daemon.
/// Subscription delivery is best effort; this is the self-healing path for a
/// daemon event that was written during pump reconnect or pump startup.
fn replay_waiting_ready(sched: &Arc<Sched>) {
    let mut seen_tasks = std::collections::HashSet::new();
    let waiting: Vec<(String, String, String)> = sched
        .terminals
        .lock()
        .unwrap()
        .iter()
        .filter(|pair| {
            let task_id = pair.0.as_str();
            sched.awaiting_ready.lock().unwrap().contains(task_id)
                && seen_tasks.insert(task_id.to_owned())
        })
        .map(|(task_id, handle)| {
            let to = match sched.db.get(task_id) {
                Ok(Some(row)) => row.to_ws,
                _ => String::new(),
            };
            (task_id.clone(), handle.clone(), to)
        })
        .collect();
    for (_task_id, handle, to) in waiting {
        if to.is_empty() {
            continue;
        }
        // One socket timeout must not serialize every waiting pane behind the
        // reaper tick. The lookup and on_ready transition remain idempotent.
        let s = sched.clone();
        std::thread::spawn(move || {
            let ws = crate::root::resolve_instance(&s.root, &to);
            crate::events::replay_ready_history(
                &s,
                &to,
                &crate::root::onlyne_sock(&ws),
                Some(&handle),
            );
        });
    }
}

/// R4: reconcile live hop state against its exact Orca terminal.
/// A busy row without a terminal is a failed handoff, not a long-running
/// worker. The shared close path keeps CR1 (terminal close + failure) and
/// clears the persisted hop_state, so a restart cannot leave an orphan.
fn reconcile_busy_terminals(sched: &Arc<Sched>) {
    let busy: Vec<(String, String)> = sched
        .db
        .list(Some("running"), 500)
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.hop_state == "busy" && !row.terminal.is_empty())
        .map(|row| (row.task_id, row.terminal))
        .collect();
    for (task_id, handle) in busy {
        if sched::session_alive(sched, &task_id, &handle) {
            if handle.starts_with("stub-") {
                sched.sessions.lock().unwrap().insert(
                    task_id.clone(),
                    crate::runtime::SessionRef {
                        task_id: task_id.clone(),
                        backend: "stub".into(),
                        backend_ref: serde_json::json!({"handle": handle.clone()}),
                        generation: 1,
                    },
                );
            }
            // Adoption can recover a live row from the DB before its
            // in-memory terminal map exists. Use the persisted handle for
            // this exact pane.
            let _ = sched
                .terminals
                .lock()
                .unwrap()
                .insert(task_id.clone(), handle.clone());
            continue;
        }
        sched.note_alert(format!(
            "busy terminal gone: {}",
            &task_id[..8.min(task_id.len())]
        ));
        let _ = sched::on_early_exit(sched, &task_id, "busy terminal gone");
    }
}

/// R4 hop timeout driver, extracted for unit testing. Runs every reaper
/// tick (10s):
/// - `ready` older than 30s (fixed): delivery write never completed ->
///   fail via on_early_exit (CR1: same call also recycles the terminal).
/// - `dispatched` older than dispatched_secs (default 120s) with no
///   delivery (`running_since` absent): handshake stall -> fail. The
///   marquee unbounded-retry policy is preserved verbatim.
/// - `idle` older than idle_secs (default 60s): hop stall -> fail.
/// - `busy` older than the role's busy_secs cap: emit `hop_overlong`
///   (CR6 repeat: first at the limit, then every max(10min, limit/2)).
///   Never kills: the power to kill belongs to human `cancel`.
fn hop_timeouts(sched: &Arc<Sched>) {
    use std::time::Duration;
    let dispatched_limit = Duration::from_secs(sched.timeout_secs("dispatched_secs"));
    let idle_limit = Duration::from_secs(sched.timeout_secs("idle_secs"));
    let now = std::time::Instant::now();
    // Snapshot hop clocks without holding locks across DB IO.
    let clocks: Vec<(String, String, std::time::Instant)> = sched
        .hop_since
        .lock()
        .unwrap()
        .iter()
        .map(|(k, (s, t))| (k.clone(), s.clone(), *t))
        .collect();
    // Which busy tasks already got their first overlong report.
    let mut overlong_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (task_id, hop, since) in clocks {
        let elapsed = now.saturating_duration_since(since);
        match hop.as_str() {
            "ready" if elapsed >= Duration::from_secs(30) => {
                sched.emit(
                    "task_stalled_ready_timeout",
                    serde_json::json!({"task_id": task_id}),
                );
                let _ = sched::on_early_exit(sched, &task_id, "delivery timeout");
            }
            "dispatched" if elapsed >= dispatched_limit => {
                // Same running_since guard as the legacy handshake path:
                // a delivered payload is never a handshake stall.
                if sched.running_since.lock().unwrap().contains_key(&task_id) {
                    continue;
                }
                sched.emit(
                    "task_stalled_ready_timeout",
                    serde_json::json!({"task_id": task_id}),
                );
                fail_or_replay_handshake(sched, &task_id);
            }
            "idle" if elapsed >= idle_limit => {
                if sched.idle_pending_exit.lock().unwrap().contains(&task_id) {
                    // Plugin-local reminder is the sole actor for pending_exit
                    // idles. Scheduler records/display only (CR3).
                    continue;
                }
                let _ = sched::on_early_exit(sched, &task_id, "swarm_idle timeout");
            }
            "busy" => {
                let role = sched
                    .db
                    .get(&task_id)
                    .ok()
                    .flatten()
                    .map(|r| r.to_ws)
                    .unwrap_or_default();
                let Some(limit) = sched.busy_limit_secs(&role) else {
                    continue;
                };
                let limit_d = Duration::from_secs(limit);
                if elapsed < limit_d {
                    continue;
                }
                // CR6 repeat: first at limit, then every max(10min, limit/2).
                let repeat = std::cmp::max(Duration::from_secs(600), limit_d / 2);
                let over = elapsed.as_secs().saturating_sub(limit);
                let prev = sched.overlong_last.lock().unwrap().get(&task_id).copied();
                let due = match prev {
                    None => true,
                    Some(last) => now.saturating_duration_since(last) >= repeat,
                };
                if due {
                    sched
                        .overlong_last
                        .lock()
                        .unwrap()
                        .insert(task_id.clone(), now);
                    sched.emit(
                        "hop_overlong",
                        serde_json::json!({
                            "task_id": task_id, "limit_secs": limit,
                            "elapsed_secs": elapsed.as_secs(), "over_secs": over,
                        }),
                    );
                }
                overlong_seen.insert(task_id);
            }
            _ => {}
        }
    }
    // Forget overlong state for tasks that left busy.
    sched
        .overlong_last
        .lock()
        .unwrap()
        .retain(|k, _| overlong_seen.contains(k));
}

/// Legacy handshake-stall outcome (marquee unbounded-retry preserved).
fn fail_or_replay_handshake(sched: &Arc<Sched>, task_id: &str) {
    if sched.root_retry_max_attempts() == Some(0) {
        if let Ok(Some(task)) = sched.db.get(task_id) {
            let retry_id = uuid::Uuid::new_v4().to_string();
            let _ = sched::on_early_exit(sched, task_id, "swarm_ready timeout");
            if sched
                .db
                .insert_task(
                    &retry_id,
                    &task.from_ws,
                    &task.to_ws,
                    &task.transfer_send_to,
                    task.attempt + 1,
                    &task.payload,
                )
                .unwrap_or(false)
            {
                sched.emit(
                    "task_retried",
                    serde_json::json!({
                        "task_id": retry_id,
                        "from_failed": task_id,
                        "attempt": task.attempt + 1,
                    }),
                );
                let _ = sched::dispatch_public(sched, &retry_id, &task.to_ws);
            }
        }
        return;
    }
    let _ = sched::on_early_exit(sched, task_id, "swarm_ready timeout");
}

fn handle_conn(sched: Arc<Sched>, stream: UnixStream) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_resp(
                    &mut writer,
                    &None,
                    false,
                    None,
                    Some(format!("bad json: {e}")),
                )?;
                continue;
            }
        };
        let id = req.get("id").cloned();
        let op = req.get("op").and_then(|o| o.as_str()).unwrap_or("");
        match op {
            "subscribe" => {
                write_resp(
                    &mut writer,
                    &id,
                    true,
                    Some(serde_json::json!({"subscribed": true})),
                    None,
                )?;
                let mut rx = sched.bus.sender().subscribe();
                while let Ok(ev) = rx.blocking_recv() {
                    let msg = serde_json::json!({"event": true, "type": ev.typ, "data": ev.data});
                    if writeln!(writer, "{}", msg).is_err() {
                        return Ok(());
                    }
                }
                return Ok(());
            }
            "status" => {
                let counts = sched.db.counts().unwrap_or_default();
                let tree = crate::template::load_tree(&sched.root).unwrap_or_default();
                let health = crate::sync::inspect(&sched.root).unwrap_or_default();
                // R3: per-workspace swarm-ready rollup so the TUI and the
                // operator see unready targets without opening each one.
                let not_ready: Vec<_> = tree
                    .iter()
                    .filter_map(|e| {
                        let path = if e.path.is_empty() { "." } else { &e.path };
                        let ws = crate::root::resolve_instance(&sched.root, &e.path);
                        let gaps = crate::sync::swarm_ready_gaps(&ws);
                        (!gaps.is_empty())
                            .then(|| serde_json::json!({"workspace": path, "gaps": gaps}))
                    })
                    .collect();
                // R4: live hop clocks for the TUI dwell display. Memory-only
                // (cleared on restart; adoption timestamps the clock at the
                // restart moment, and the row's ledger_state says `adopted`).
                let overlong: std::collections::HashSet<String> = sched
                    .overlong_last
                    .lock()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect();
                let hops: serde_json::Value = sched
                    .hop_since
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, (state, since))| {
                        (
                            id.clone(),
                            serde_json::json!({
                                "state": state,
                                "secs": since.elapsed().as_secs(),
                                "overlong": overlong.contains(id),
                            }),
                        )
                    })
                    .collect::<serde_json::Map<_, _>>()
                    .into();
                let data = serde_json::json!({
                    "root": sched.root.to_string_lossy(),
                    "workspaces": tree.len(),
                    "tasks_by_state": counts,
                    "ledger_tail": sched.db.ledger(20).unwrap_or_default(),
                    "orphans": health.orphans,
                    "dangling": health.dangling,
                    "legacy_views": health.legacy_views,
                    "not_swarm_ready": not_ready,
                    "hops": hops,
                    "alerts": sched.recent_alerts(),
                });
                write_resp(&mut writer, &id, true, Some(data), None)?;
            }
            // One op for the whole operator surface: `repair::repair_request`
            // owns argument extraction, so the socket, the CLI, and the tests
            // all exercise the same contract. A refused action comes back as
            // ok:false with the reducer's or the probe's own reason.
            "repair" => match crate::repair::repair_request(&sched, &req) {
                Ok(data) => write_resp(&mut writer, &id, true, Some(data), None)?,
                Err(e) => write_resp(&mut writer, &id, false, None, Some(e.to_string()))?,
            },
            "graph" => match sched.db.graph() {
                Ok(graph) => {
                    write_resp(&mut writer, &id, true, Some(serde_json::json!(graph)), None)?
                }
                Err(e) => write_resp(&mut writer, &id, false, None, Some(e.to_string()))?,
            },
            "list_workspaces" => {
                let tree = crate::template::load_tree(&sched.root).unwrap_or_default();
                let items: Vec<_> = tree
                    .iter()
                    .map(|e| {
                        let path = if e.path.is_empty() { "." } else { &e.path };
                        let ws = crate::root::resolve_instance(&sched.root, &e.path);
                        let daemon = if crate::daemon::is_alive(&crate::root::onlyne_sock(&ws)) {
                            "ready"
                        } else {
                            "offline"
                        };
                        // R3: swarm-ready gates per workspace. A daemon can be
                        // up while the session inside can never handshake
                        // (missing [swarm], autoStart off, no plugin).
                        let gaps = crate::sync::swarm_ready_gaps(&ws);
                        let swarm_ready = gaps.is_empty();
                        serde_json::json!({
                            "path": path,
                            "name": e.name,
                            "model": {"provider": e.model.provider, "model": e.model.model, "effort": e.model.effort},
                            "back_edges": e.back_edges,
                            "daemon": daemon,
                            "swarm_ready": swarm_ready,
                            "swarm_ready_gaps": gaps,
                        })
                    })
                    .collect();
                write_resp(&mut writer, &id, true, Some(serde_json::json!(items)), None)?;
            }
            "list_tasks" => {
                let filter = task_filter(&req);
                match sched.db.list_page(&filter) {
                    Ok(page) => {
                        write_resp(&mut writer, &id, true, Some(serde_json::json!(page)), None)?
                    }
                    Err(e) => write_resp(&mut writer, &id, false, None, Some(e.to_string()))?,
                }
            }
            "task_detail" => {
                let task_id = req.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
                match sched.db.detail(task_id) {
                    Ok(Some(detail)) => {
                        // R4: the detail response carries the live hop clock
                        // so the TUI dwell line has one source of truth. The
                        // adopted marker explains a clock that starts at a
                        // scheduler restart, not at the session's real start.
                        let mut value = serde_json::json!(detail);
                        let hop = sched.hop_since.lock().unwrap().get(task_id).cloned();
                        let adopted = sched
                            .db
                            .get(task_id)
                            .ok()
                            .flatten()
                            .map(|r| r.ledger_state == "adopted")
                            .unwrap_or(false);
                        if let (Some((state, since)), Some(obj)) = (hop, value.as_object_mut()) {
                            obj.insert(
                                "hop".into(),
                                serde_json::json!({
                                    "state": state,
                                    "secs": since.elapsed().as_secs(),
                                    "adopted": adopted,
                                }),
                            );
                        }
                        write_resp(&mut writer, &id, true, Some(value), None)?
                    }
                    Ok(None) => write_resp(
                        &mut writer,
                        &id,
                        false,
                        None,
                        Some(format!("unknown task: {task_id}")),
                    )?,
                    Err(e) => write_resp(&mut writer, &id, false, None, Some(e.to_string()))?,
                }
            }
            "submit" => {
                let to = req.get("to").and_then(|t| t.as_str()).unwrap_or("");
                let payload = req
                    .get("payload_markdown")
                    .and_then(|p| p.as_str())
                    .unwrap_or("");
                // Supervisor submits from "." unless told otherwise.
                let from = req.get("from").and_then(|f| f.as_str()).unwrap_or(".");
                // R3: refuse fast when the target can never handshake.
                // A silent 120s hang costs more than a loud rejection.
                // Stub agents (headless e2e) have no .pi at all by design.
                let stub = std::env::var("SWARM_STUB_AGENT").as_deref() == Ok("1");
                if !stub {
                    let target_ws = crate::root::resolve_instance(&sched.root, to);
                    let gaps = crate::sync::swarm_ready_gaps(&target_ws);
                    if !gaps.is_empty() {
                        write_resp(
                            &mut writer,
                            &id,
                            false,
                            None,
                            Some(format!(
                                "workspace '{to}' is not swarm-ready: {}",
                                gaps.join("; ")
                            )),
                        )?;
                        continue;
                    }
                }
                match sched::submit(&sched, from, to, payload) {
                    Ok(task_id) => write_resp(
                        &mut writer,
                        &id,
                        true,
                        Some(serde_json::json!({"task_id": task_id})),
                        None,
                    )?,
                    Err(e) => write_resp(&mut writer, &id, false, None, Some(e.to_string()))?,
                }
            }
            "cancel" => {
                let task_id = req.get("task_id").and_then(|t| t.as_str()).unwrap_or("");
                let reason = req
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("cancelled");
                let force = req.get("force").and_then(|f| f.as_bool()).unwrap_or(false);
                match sched::cancel_force(&sched, task_id, reason, force) {
                    Ok(ids) => write_resp(
                        &mut writer,
                        &id,
                        true,
                        Some(serde_json::json!({"cancelled": ids})),
                        None,
                    )?,
                    Err(e) => write_resp(&mut writer, &id, false, None, Some(e.to_string()))?,
                }
            }
            "toggle_swarm" => {
                let ws = req.get("workspace").and_then(|w| w.as_str()).unwrap_or("");
                let enabled = req
                    .get("enabled")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(false);
                match toggle_swarm(&sched, ws, enabled) {
                    Ok(()) => write_resp(
                        &mut writer,
                        &id,
                        true,
                        Some(serde_json::json!({"workspace": ws, "enabled": enabled})),
                        None,
                    )?,
                    Err(e) => write_resp(&mut writer, &id, false, None, Some(e.to_string()))?,
                }
            }
            _ => write_resp(
                &mut writer,
                &id,
                false,
                None,
                Some(format!("unknown op: {op}")),
            )?,
        }
    }
}

fn task_filter(req: &serde_json::Value) -> crate::db::TaskFilter {
    crate::db::TaskFilter {
        state: req.get("state").and_then(|v| v.as_str()).map(str::to_owned),
        to_ws: req.get("to_ws").and_then(|v| v.as_str()).map(str::to_owned),
        from_ws: req
            .get("from_ws")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        text: req.get("text").and_then(|v| v.as_str()).map(str::to_owned),
        since: req.get("since").and_then(|v| v.as_i64()),
        retry_only: req
            .get("retry_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        limit: req.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize,
        offset: req.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
    }
}

/// Drain semantics: disabling stops new session creation; running tasks finish.
/// Re-enabling resumes. State persists in the workspace config.toml [swarm] table.
fn toggle_swarm(sched: &Arc<Sched>, workspace: &str, enabled: bool) -> anyhow::Result<()> {
    let ws = crate::root::resolve_instance(&sched.root, workspace);
    let cfg_path = ws.join(".onlyne/config.toml");
    let mut text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    if text.contains("[swarm]") {
        // Flip the enabled line inside [swarm].
        let mut out = String::new();
        let mut in_swarm = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_swarm = t == "[swarm]";
                out.push_str(line);
                out.push('\n');
                continue;
            }
            if in_swarm && t.starts_with("enabled") {
                out.push_str(&format!("enabled = {enabled}\n"));
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        text = out;
    } else {
        text.push_str(&format!("\n[swarm]\nenabled = {enabled}\n"));
    }
    std::fs::write(&cfg_path, text)?;
    Ok(())
}

fn write_resp(
    w: &mut UnixStream,
    id: &Option<serde_json::Value>,
    ok: bool,
    data: Option<serde_json::Value>,
    error: Option<String>,
) -> anyhow::Result<()> {
    let msg = serde_json::json!({
        "id": id,
        "ok": ok,
        "data": data,
        "error": error.map(|m| serde_json::json!({"message": m})),
    });
    writeln!(w, "{msg}")?;
    Ok(())
}

/// A workspace daemon on a real Unix socket, for tests in this crate.
///
/// It answers exactly the two operations the scheduler sends to
/// `root::onlyne_sock`: `loopback` (accept a wire and, for a recycle control
/// frame, remember the `swarm_recycled` ack it now owes) and
/// `fetch_channel_history` (hand those lines back, which is what
/// `sched::wait_recycled_ack` polls). Every request is recorded so a test can
/// assert the exact bytes that crossed the socket.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct DaemonState {
    pub(crate) requests: Vec<serde_json::Value>,
    pub(crate) history: Vec<serde_json::Value>,
}

#[cfg(test)]
impl DaemonState {
    /// The `text` of every `loopback` request that carried a swarm wire.
    pub(crate) fn wires(&self) -> Vec<String> {
        self.requests
            .iter()
            .filter(|r| r["op"] == "loopback")
            .filter_map(|r| r["text"].as_str().map(str::to_string))
            .collect()
    }
}

/// Bind `root/.onlyne/run/s` for `role` and serve it until the process ends.
/// The returned state is shared with the accepting threads.
#[cfg(test)]
pub(crate) fn test_daemon(
    root: &std::path::Path,
    role: &str,
) -> Arc<std::sync::Mutex<DaemonState>> {
    use std::io::{BufRead, BufReader, Write};
    let ws = crate::root::resolve_instance(root, role);
    std::fs::create_dir_all(ws.join(".onlyne/run")).unwrap();
    let sock = crate::root::onlyne_sock(&ws);
    let _ = std::fs::remove_file(&sock);
    let listener = match UnixListener::bind(&sock) {
        Ok(listener) => listener,
        Err(err) => panic!("bind the fake daemon at {}: {err}", sock.display()),
    };
    let state: Arc<std::sync::Mutex<DaemonState>> =
        Arc::new(std::sync::Mutex::new(DaemonState::default()));
    let sink = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let sink = sink.clone();
            std::thread::spawn(move || {
                let mut writer = match stream.try_clone() {
                    Ok(stream) => stream,
                    Err(_) => return,
                };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
                        continue;
                    };
                    let id = request
                        .get("id")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let op = request
                        .get("op")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let text = request
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut guard = sink.lock().unwrap();
                    guard.requests.push(request.clone());
                    let response = match op.as_str() {
                        "loopback" => {
                            if text.contains("op: recycle") {
                                if let Some(task) = text
                                    .lines()
                                    .find_map(|line| line.strip_prefix("task_id: "))
                                    .map(|line| line.trim().to_string())
                                {
                                    guard.history.push(serde_json::json!({
                                        "direction": "inbound",
                                        "text": format!("swarm_recycled {{\"task_id\":\"{task}\"}}"),
                                    }));
                                }
                            }
                            serde_json::json!({"id": id, "ok": true, "data": {"accepted": true}})
                        }
                        "fetch_channel_history" => serde_json::json!({
                            "id": id,
                            "ok": true,
                            "data": guard.history.clone(),
                        }),
                        other => serde_json::json!({
                            "id": id,
                            "ok": false,
                            "error": {"message": format!("test daemon: unknown op {other:?}")},
                        }),
                    };
                    if writeln!(writer, "{response}").is_err() {
                        return;
                    }
                }
            });
        }
    });
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, TaskState};

    /// The repair op over `swarm.sock`: same contract as the CLI, refusals
    /// included, and a bad request must never take the connection down — one
    /// operator session can inspect, fail, and ask again.
    #[test]
    fn the_repair_op_answers_over_the_socket_and_keeps_talking_after_a_refusal() {
        use std::io::Write;
        let sched = test_sched();
        sched
            .db
            .insert_task("rp-1", ".", "worker", "", 1, "payload")
            .unwrap();
        let seed = crate::reconcile::to_versioned(
            &crate::lifecycle::Observation::initial(1, 3),
            "{}",
            "{}",
        )
        .unwrap();
        assert!(sched.db.upsert_session("rp-1", &seed).unwrap());
        // A resource that never attached has nothing to close, so put the pane
        // up first: `close` should then land the close for real.
        crate::reconcile::feed_resource_attached(&sched, "rp-1").unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        // The handler runs on its own thread and reports through a channel, so
        // a wedged reply fails the test in seconds instead of hanging the whole
        // suite: `UnixStream` clones share one file description, and a server
        // that never sees EOF would otherwise block in `read_line` forever.
        let (answered, waited): (
            std::sync::mpsc::Sender<std::io::Result<()>>,
            std::sync::mpsc::Receiver<std::io::Result<()>>,
        ) = std::sync::mpsc::channel();
        let serving = sched.clone();
        std::thread::spawn(move || {
            let _ = answered.send(
                handle_conn(serving, server)
                    .map(|_| ())
                    .map_err(|err| std::io::Error::other(err.to_string())),
            );
        });
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        let mut ask = |request: serde_json::Value| -> serde_json::Value {
            writeln!(writer, "{request}").unwrap();
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        };
        let seen = ask(serde_json::json!({
            "id": "cli", "op": "repair", "action": "inspect", "task_id": "rp-1"
        }));
        assert_eq!(seen["id"], "cli", "{seen}");
        assert_eq!(seen["ok"], true, "{seen}");
        assert_eq!(seen["data"]["action"], "inspect");
        assert_eq!(seen["data"]["task_id"], "rp-1");
        assert_eq!(seen["data"]["tuple"]["public_lifecycle"], "created");
        assert_eq!(seen["data"]["tuple"]["generation"], 1);
        // An unknown action is refused by name, on the same connection.
        let refused = ask(serde_json::json!({
            "id": "cli2", "op": "repair", "action": "resurrect", "task_id": "rp-1"
        }));
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(refused["id"], "cli2");
        let message = refused["error"]["message"].as_str().unwrap_or("");
        assert!(message.contains("unknown repair action"), "{message}");
        assert!(message.contains("rebind"), "{message}");
        // A missing fault id is its own refusal, distinct from "already acked".
        let refused =
            ask(serde_json::json!({"id": 3, "op": "repair", "action": "ack", "fault_id": 4242}));
        assert_eq!(refused["ok"], false, "{refused}");
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("no fault 4242"),
            "{refused}"
        );
        // And the session is still usable afterwards.
        let again = ask(serde_json::json!({
            "id": "cli3", "op": "repair", "action": "close", "task_id": "rp-1"
        }));
        assert_eq!(again["ok"], true, "{again}");
        assert_eq!(again["data"]["resource_state"], "closed");
        assert_eq!(again["data"]["agent_state"], "gone");
        assert_eq!(again["data"]["public_lifecycle"], "exited");
        assert_eq!(
            again["data"]["task_state"].as_str(),
            Some("failed"),
            "work that was still owed is failed by the close, as the ack path does"
        );
        assert_eq!(
            sched.db.operator_revision("rp-1").unwrap().unwrap(),
            again["data"]["operator_revision"],
            "the stamp the reply reports is the stamp the row holds"
        );
        // Close the socket for real: `drop(writer)` alone keeps the fd alive
        // through the reader's clone, so the handler would never see EOF.
        reader
            .get_mut()
            .shutdown(std::net::Shutdown::Both)
            .expect("shut down the client half");
        drop(writer);
        drop(reader);
        match waited.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(err)) => panic!("the repair connection failed: {err}"),
            Err(err) => panic!("the connection handler never returned: {err}"),
        }
    }

    fn test_sched() -> Arc<Sched> {
        let dir = tempfile::tempdir().unwrap();
        let root: &'static std::path::Path = Box::leak(dir.path().join("root").into_boxed_path());
        std::fs::create_dir_all(root).unwrap();
        let _ = Box::leak(Box::new(dir));
        let db = Db::open(root).unwrap();
        Sched::new(root.to_path_buf(), db)
    }

    #[test]
    fn hop_timeouts_fail_ready_stall_and_respect_pending_idle() {
        // CR1 + CR3: a `ready` hop past the fixed 30s delivery window dies
        // and recycles its terminal in the same call; a pending_exit idle is
        // recorded but never failed by TTL (the plugin owns its exit path).
        let s = test_sched();
        s.db.insert_task("rdy", ".", "a", "", 1, "p").unwrap();
        s.db.set_state("rdy", TaskState::Running).unwrap();
        s.db.set_terminal("rdy", "stub-term").unwrap();
        s.hop_since.lock().unwrap().insert(
            "rdy".into(),
            (
                "ready".into(),
                std::time::Instant::now() - std::time::Duration::from_secs(31),
            ),
        );
        // pending_exit idle, also older than the idle limit, must survive.
        s.db.insert_task("idl", ".", "a", "", 1, "p").unwrap();
        s.db.set_state("idl", TaskState::Running).unwrap();
        s.hop_since.lock().unwrap().insert(
            "idl".into(),
            (
                "idle".into(),
                std::time::Instant::now() - std::time::Duration::from_secs(120),
            ),
        );
        s.idle_pending_exit.lock().unwrap().insert("idl".into());
        hop_timeouts(&s);
        assert_eq!(
            s.db.get("rdy").unwrap().unwrap().state,
            TaskState::Failed,
            "ready stall fails"
        );
        assert!(
            !s.terminals.lock().unwrap().contains_key("rdy"),
            "CR1: terminal recycled"
        );
        assert_eq!(
            s.db.get("idl").unwrap().unwrap().state,
            TaskState::Running,
            "CR3: pending idle untouched"
        );
    }

    #[test]
    fn task_filter_maps_all_history_predicates() {
        let filter = task_filter(&serde_json::json!({
            "state": "active",
            "to_ws": "scout",
            "from_ws": "model",
            "text": "needle",
            "since": 123,
            "retry_only": true,
            "limit": 42,
            "offset": 84,
        }));
        assert_eq!(filter.state.as_deref(), Some("active"));
        assert_eq!(filter.to_ws.as_deref(), Some("scout"));
        assert_eq!(filter.from_ws.as_deref(), Some("model"));
        assert_eq!(filter.text.as_deref(), Some("needle"));
        assert_eq!(filter.since, Some(123));
        assert!(filter.retry_only);
        assert_eq!(filter.limit, 42);
        assert_eq!(filter.offset, 84);
    }

    #[test]
    fn sweep_ignores_stub_handles() {
        std::env::set_var("ORCA_CLI_COMMAND", "/bin/false");
        let s = test_sched();
        s.db.insert_task("stub1", ".", "a", "", 1, "p").unwrap();
        s.db.set_state("stub1", TaskState::Running).unwrap();
        s.db.set_terminal("stub1", "stub-term").unwrap();
        let (failed, retried) = sweep_dead_terminals(&s);
        assert!(failed.is_empty());
        assert!(retried.is_empty());
        std::env::remove_var("ORCA_CLI_COMMAND");
    }

    #[test]
    fn fresh_delivery_skips_dead_terminal_in_grace() {
        std::env::set_var("ORCA_CLI_COMMAND", "/bin/false");
        let s = test_sched();
        s.db.insert_task("fresh1", ".", "a", "", 1, "p").unwrap();
        s.db.set_state("fresh1", TaskState::Running).unwrap();
        s.db.set_terminal("fresh1", "term-fresh").unwrap();
        // Inside the grace window: never reaped, even though the orca probe
        // would say dead (the out-event race the sweep must not win).
        s.running_since
            .lock()
            .unwrap()
            .insert("fresh1".into(), std::time::Instant::now());
        let (failed, retried) = sweep_dead_terminals(&s);
        assert!(failed.is_empty());
        assert!(retried.is_empty());
        // Older than grace still stays untouched when the Orca probe is
        // unavailable: only an explicit `status=exited` may fail a task.
        // The explicit-status parser has dedicated unit coverage in
        // orca_term::tests::terminal_liveness_requires_explicit_exited_status.
        s.running_since.lock().unwrap().insert(
            "fresh1".into(),
            std::time::Instant::now() - std::time::Duration::from_secs(30),
        );
        let (failed, retried) = sweep_dead_terminals(&s);
        assert!(failed.is_empty());
        assert!(retried.is_empty());
        std::env::remove_var("ORCA_CLI_COMMAND");
    }

    #[test]
    fn reconcile_busy_only_reaps_explicitly_dead_terminal() {
        // A DB row left `busy` from a prior scheduler lifetime can be adopted
        // while live. Orca probe failure is unknown and stays alive; only an
        // explicit exited status may clear the stale hop row.
        std::env::set_var("ORCA_CLI_COMMAND", "/bin/false");
        let s = test_sched();
        s.db.insert_task("busy1", ".", "a", "", 1, "p").unwrap();
        s.db.set_state("busy1", TaskState::Running).unwrap();
        s.db.set_terminal("busy1", "term-unknown").unwrap();
        s.db.set_hop("busy1", "busy").unwrap();
        reconcile_busy_terminals(&s);
        let row = s.db.get("busy1").unwrap().unwrap();
        assert_eq!(row.state, TaskState::Running);
        assert_eq!(row.hop_state, "busy");
        assert!(s.terminals.lock().unwrap().contains_key("busy1"));
        std::env::remove_var("ORCA_CLI_COMMAND");
    }

    #[test]
    fn restart_reconcile_adopts_live_terminal() {
        // R5: a running row whose orca tab is still alive must be adopted,
        // not failed. The liveness contract treats probe failure as alive
        // (unknown), so /bin/false as the orca binary exercises adoption.
        std::env::set_var("ORCA_CLI_COMMAND", "/bin/false");
        let s = test_sched();
        s.db.insert_task("adopt1", ".", "a", "", 1, "p").unwrap();
        s.db.set_state("adopt1", TaskState::Running).unwrap();
        s.db.set_terminal("adopt1", "term-live").unwrap();
        super::reap_previous_run(&s);
        let row = s.db.get("adopt1").unwrap().unwrap();
        assert_eq!(row.state, TaskState::Running);
        assert!(s.terminals.lock().unwrap().contains_key("adopt1"));
        let tail = s.db.ledger(10).unwrap();
        assert!(tail
            .iter()
            .any(|e| e.task_id == "adopt1" && e.state == "adopted"));
        // A row with no terminal at all still fails fast.
        s.db.insert_task("dead1", ".", "a", "", 1, "p").unwrap();
        s.db.set_state("dead1", TaskState::Running).unwrap();
        super::reap_previous_run(&s);
        let row = s.db.get("dead1").unwrap().unwrap();
        assert_eq!(row.state, TaskState::Failed);
        std::env::remove_var("ORCA_CLI_COMMAND");
    }
}
