use super::config::{ClientInit, FLUSH_PAUSE_MS, READINESS_POLL_MS, RunState, reconnect_backoff};
use super::run::pull_ack_loop;
use super::sessions::{
    refresh_role_slice, scan_control_settles, scan_reclaimed_resources, scan_reconnect_grace,
    scan_stalls,
};
use crate::runtime::intent::op_for_intent;
use crate::session::dispatch::{self, ClientLink};
use anyhow::{Result, anyhow};
use onlyne_net::conn::ConnReadiness;
use onlyne_net::is_permanent;
use onlyne_proto::{ClientOp, EventTier, Frame, Subscribe};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::sleep;

/// Keep the server link up until a permanent failure ends the run.
///
/// Every pass from `Reconnecting` back to `Ready` runs the order the plan fixes
/// for a reconnect inside [`run_link`], and the ladder caps at the last rung so
/// a server that stays down costs one dial per minute.
pub(super) async fn link_loop(init: &ClientInit, state: &RunState) -> Result<()> {
    let mut backoff = reconnect_backoff();
    let accept_new = state.dispatch.accept_new();
    loop {
        match ClientLink::connect(init, state.dispatch.hello_live_tasks()).await {
            Ok(link) => {
                backoff.reset();
                state.dispatch.attach_outbox(Arc::new(link.clone()));
                state.dispatch.set_link_up(true);
                match run_link(init, &link, state).await {
                    Ok(()) => tracing::info!(role = %init.role, "server link ended"),
                    Err(error) => tracing::warn!(error = %error, "server link failed"),
                }
                state.dispatch.detach_outbox();
                state.dispatch.set_link_up(false);
                accept_new.store(false, Ordering::SeqCst);
                if let Some(failure) = link.failure().await {
                    if is_permanent(&failure) {
                        return Err(anyhow!("{failure}"));
                    }
                }
            }
            Err(error) if is_permanent(&error) => return Err(anyhow!("{error}")),
            Err(error) => tracing::warn!(error = %error, "connect failed"),
        }
        let delay = backoff.next();
        tracing::info!(seconds = delay.as_secs(), "reconnecting");
        sleep(delay).await;
    }
}

/// Drive one live link: welcome, flush, resume, then the four tasks.
pub(super) async fn run_link(init: &ClientInit, link: &ClientLink, state: &RunState) -> Result<()> {
    let welcome = link.welcome().clone();
    state
        .store
        .put_prose(&welcome.role, &welcome.prose, &welcome.spec_hash)?;
    state.store.put_config("role", &welcome.role)?;
    state.store.put_config("cluster", &welcome.cluster)?;
    state.adopt(&welcome).await;
    // §5 line 248: the aggregate name is a property of the spec entry the
    // server bound this link to, so it can only come from the welcome. A plain
    // role receives `None` and keeps sending reports without the key.
    state
        .dispatch
        .set_cluster_ref(welcome.aggregate.clone().unwrap_or_default());
    flush_intents(link, state).await;
    subscribe(link, state.cursor()).await?;
    state.accept_new.store(true, Ordering::SeqCst);
    let mut pull = tokio::spawn(pull_ack_loop(init.clone(), link.clone(), state.clone()));
    let mut flusher = tokio::spawn(flush_loop(link.clone(), state.clone()));
    let mut reader = tokio::spawn(read_events(link.clone(), state.clone()));
    let mut watcher = tokio::spawn(watch_readiness(link.clone(), state.clone()));
    tracing::info!(role = %init.role, "server link ready");
    tokio::select! {
        _ = &mut pull => {}
        _ = &mut flusher => {}
        _ = &mut reader => {}
        _ = &mut watcher => {}
    }
    pull.abort();
    flusher.abort();
    reader.abort();
    watcher.abort();
    Ok(())
}

/// Follow the connection state and re-run the post-reconnect order.
pub(super) async fn watch_readiness(link: ClientLink, state: RunState) -> Result<()> {
    let mut ready = true;
    loop {
        sleep(Duration::from_millis(READINESS_POLL_MS)).await;
        scan_reclaimed_resources(&state).await;
        scan_stalls(&state).await;
        scan_reconnect_grace(&state).await;
        // Behind the reconnect sweep, which settles the work a session that died
        // took with it and drops any note on that task: what is left for this
        // step is the word of a command whose session is gone while its task is
        // still open, and no report is coming to answer it.
        scan_control_settles(&state).await;
        match link.readiness() {
            ConnReadiness::Ready => {
                if !ready {
                    ready = true;
                    // The link redials on its own, so its fresh connection needs
                    // the routed `hello` before any queued frame reaches it.
                    link.authenticate(state.dispatch.hello_live_tasks()).await?;
                    state.accept_new.store(true, Ordering::SeqCst);
                    state.dispatch.set_link_up(true);
                    flush_intents(&link, &state).await;
                    subscribe(&link, state.cursor()).await?;
                    tracing::info!("server link restored; intents flushed");
                }
            }
            ConnReadiness::Reconnecting => {
                if ready {
                    ready = false;
                    state.accept_new.store(false, Ordering::SeqCst);
                    state.dispatch.set_link_up(false);
                    tracing::warn!("server link lost; sessions settle and intents keep queuing");
                }
            }
            ConnReadiness::Closed => return Ok(()),
        }
    }
}

/// Whether the transport answered from a fresh link or a live one.
pub(super) fn transient(error: &onlyne_net::NetError) -> bool {
    matches!(
        error,
        onlyne_net::NetError::NotReady
            | onlyne_net::NetError::Disconnected(_)
            | onlyne_net::NetError::RequestTimeout
    )
}

/// The flusher task: push the durable intent queue at the server.
pub(super) async fn flush_loop(link: ClientLink, state: RunState) -> Result<()> {
    loop {
        // The link redials behind the runtime's back, and a frame sent into that
        // fresh connection is refused until its `hello` lands, so the queue waits
        // for a ready link rather than spending a round trip on the refusal.
        if link.readiness() == ConnReadiness::Ready {
            flush_intents(&link, &state).await;
        }
        sleep(Duration::from_millis(FLUSH_PAUSE_MS)).await;
    }
}

/// Send each pending intent once and record the answer.
pub(super) async fn flush_intents(link: &ClientLink, state: &RunState) {
    let rows = match state.intents.lock().pending() {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(error = %error, "intent queue unreadable");
            return;
        }
    };
    for row in rows {
        let op = match op_for_intent(&row) {
            Ok(op) => op,
            Err(error) => {
                tracing::warn!(error = %error, op_id = %row.op_id, "intent payload unreadable");
                continue;
            }
        };
        // §5 line 248: a supervisor's report names its cluster on the durable
        // path too, so the flusher stamps the same rule `send_frame` applies.
        let op = match op {
            ClientOp::Report(report) => ClientOp::Report(crate::session::dispatch::with_cluster(
                &state.dispatch,
                report,
            )),
            other => other,
        };
        match link.request(op).await {
            Ok(body) => {
                let machine = state.intents.lock();
                match machine.attempt(&row, Some(&body)) {
                    Ok(crate::runtime::intent::IntentResult::Dropped(code, reason)) => {
                        // Dropping is terminal for the row, so the reason stays in
                        // the log rather than only in the deleted row (plan §6).
                        tracing::warn!(
                            op_id = %row.op_id,
                            ?code,
                            reason = %reason,
                            "intent dropped by a permanent answer"
                        );
                    }
                    Ok(crate::runtime::intent::IntentResult::Accepted(_)) => {
                        note_intent_receipt(state, &row);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(error = %error, op_id = %row.op_id, "intent answer not recorded");
                    }
                }
            }
            Err(error) => {
                // The link is down rather than the server refusing, so this row
                // waits for the reconnect with its retry budget intact.
                let machine = state.intents.lock();
                if let Err(record) = machine.defer(&row, "connection unavailable") {
                    tracing::warn!(error = %record, op_id = %row.op_id, "intent deferral not recorded");
                }
                tracing::warn!(error = %error, op_id = %row.op_id, "intent send failed");
                return;
            }
        }
    }
}

/// Start, or resume, the observation stream.
pub(super) async fn subscribe(link: &ClientLink, since_seq: u64) -> Result<()> {
    let request = Subscribe {
        since_seq,
        tiers: vec![EventTier::Durable, EventTier::Advisory],
        kinds: Vec::new(),
        roles: Vec::new(),
    };
    let reply = link.request(ClientOp::Subscribe(request)).await?;
    if !reply.ok {
        return Err(anyhow!("subscribe refused: {:?}", reply.error));
    }
    Ok(())
}

/// Feed the reducer the receipt one accepted intent earned for its session.
///
/// The server's answer is the only evidence a completion left this process, and
/// the flusher is the only place that answer is seen, so the routing starts
/// here. A row that is not a completion intent carries no drain to close and
/// stops here: [`completion_task_id`] names the two shapes that are one, and
/// every other op the queue holds answers something else.
fn note_intent_receipt(state: &RunState, row: &onlyne_store::IntentRow) {
    let Some(task_id) = crate::runtime::intent::completion_task_id(row) else {
        return;
    };
    dispatch::note_intent_receipt(&state.dispatch, &task_id);
}

/// The reader task: mirror the server event stream and resync on loss.
pub(super) async fn read_events(link: ClientLink, state: RunState) -> Result<()> {
    let mut events = link.events();
    loop {
        match events.recv().await {
            Ok(frame) => {
                if let Some(count) = onlyne_net::resync_lag_of(&frame) {
                    tracing::warn!(count, "event queue overflowed; resuming from the cursor");
                    subscribe(&link, state.cursor()).await?;
                    continue;
                }
                match frame {
                    Frame::Ev { seq, event } => {
                        let spec_reloaded =
                            matches!(event.as_ref(), onlyne_proto::Event::SpecReloaded(_));
                        let body = serde_json::to_value(&event)?;
                        let kind = body
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("event");
                        state.store.append_event(kind, &body)?;
                        state.set_cursor(seq);
                        if spec_reloaded {
                            refresh_role_slice(&link, &state).await?;
                        }
                    }
                    Frame::Pong { server_seq, .. } => {
                        state.set_cursor(server_seq.max(state.cursor()))
                    }
                    _ => {}
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "event reader lagged; resuming from the cursor");
                subscribe(&link, state.cursor()).await?;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
