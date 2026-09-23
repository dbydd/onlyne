use super::config::{
    ClientInit, NOT_READY_PAUSE_MS, PULL_HOLD_MS, PULL_LIMIT, PULL_PAUSE_MS, RunState,
    SHUTDOWN_CLOSE_BUDGET,
};
use super::link::{link_loop, transient};
use super::sessions::{accept_delivery, outcome_loop};
use crate::session::adapter_socket::AdapterSocket;
use crate::session::dispatch::{self, ClientLink, DispatchState};
use anyhow::{Result, anyhow};
use onlyne_layout::RoleWorkspace;
use onlyne_proto::{AckArgs, ClientOp, ControlOp, Delivery, PullArgs, PullReply};
use onlyne_store::ClientStore;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::sleep;

/// Run the role runtime until a permanent handshake failure or a dead local
/// surface stops it.
///
/// The acceptor task owns the workspace socket bind, and its failure ends this
/// run the same way a permanent link failure does: a client that keeps a TLS
/// link while its socket is unbound reads as connected from the server while
/// every verb the workspace issues fails, so the exit status and the message on
/// stderr are the operator's only notice.
pub async fn run(init: ClientInit) -> Result<()> {
    let workspace = RoleWorkspace::resolve(&init.workspace);
    let store = ClientStore::open(workspace.client_db_path())?;
    let state = RunState::new(&init, store)?;
    let mut acceptor = tokio::spawn(acceptor(init.clone(), state.clone()));
    let closing = tokio::spawn(close_on_signal(state.dispatch.clone()));
    let mut outcomes = tokio::spawn(outcome_loop(state.clone()));
    let outcome = tokio::select! {
        link = link_loop(&init, &state) => link,
        served = &mut acceptor => match served {
            Ok(Ok(())) => Err(anyhow!("the adapter surface stopped serving")),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(anyhow!("adapter acceptor task ended: {error}")),
        },
        pumped = &mut outcomes => match pumped {
            Ok(Ok(())) => Err(anyhow!("the session outcome pump stopped")),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(anyhow!("session outcome pump task ended: {error}")),
        },
    };
    acceptor.abort();
    closing.abort();
    outcomes.abort();
    outcome
}

/// Close live sessions when the operator stops the client.
///
/// `SIGTERM` ends the foreground client, and the default disposition would
/// kill the process with every tab it opened still running: the resources
/// would outlive the only thing that can address them. Each session closes with
/// [`onlyne_session::CloseReason::Shutdown`] first, so the backend record and
/// the plugin-facing tab map end truthfully.
#[cfg(unix)]
pub(super) async fn close_on_signal(dispatch: DispatchState) {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(error = %error, "SIGTERM handler was not installed");
            return;
        }
    };
    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(error = %error, "SIGINT handler was not installed");
            return;
        }
    };
    tokio::select! {
        _ = terminate.recv() => tracing::info!("SIGTERM: closing live sessions"),
        _ = interrupt.recv() => tracing::info!("SIGINT: closing live sessions"),
    }
    dispatch::close_all(
        &dispatch,
        onlyne_session::CloseReason::Shutdown,
        SHUTDOWN_CLOSE_BUDGET,
    );
    std::process::exit(0);
}

/// Close live sessions when the operator stops the client.
///
/// Windows has no SIGTERM; operators use `onlyne shutdown` for a graceful
/// daemon stop. Ctrl-C is the console interrupt, and it runs the same
/// close_all budget the unix SIGINT path uses.
#[cfg(windows)]
pub(super) async fn close_on_signal(dispatch: DispatchState) {
    // `ctrl_c()` installs synchronously and hands back the watch stream; the
    // await belongs on `recv`, which yields once per console interrupt.
    let mut interrupt = match tokio::signal::windows::ctrl_c() {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(error = %error, "Ctrl-C handler was not installed");
            return;
        }
    };
    interrupt.recv().await;
    tracing::info!("Ctrl-C: closing live sessions");
    dispatch::close_all(
        &dispatch,
        onlyne_session::CloseReason::Shutdown,
        SHUTDOWN_CLOSE_BUDGET,
    );
    std::process::exit(0);
}

/// Bind the adapter socket and serve it for the life of the process.
///
/// The bind is the first act, and its error leaves this task: a socket that
/// never opened is a workspace whose plugins cannot mount, and only the exit
/// status says so. Once the listener exists the surface stays up — a failed
/// `accept` is logged and retried by [`AdapterSocket::accept_loop`] — so this
/// task ends the run exactly when the local surface could not start.
pub(super) async fn acceptor(init: ClientInit, state: RunState) -> Result<()> {
    let workspace = RoleWorkspace::resolve(&init.workspace);
    let cluster = state
        .store
        .config("cluster")
        .ok()
        .flatten()
        .unwrap_or_default();
    let socket = AdapterSocket {
        workspace: workspace.root().to_path_buf(),
        role: init.role.clone(),
        cluster,
        server: init.server.clone(),
        dispatch: state.dispatch.clone(),
    };
    let (listener, _endpoint) = socket.bind().await?;
    socket.accept_loop(listener).await
}

/// The pull-ack task: drain what the server queued, then settle what the local
/// side finished.
pub(super) async fn pull_ack_loop(
    init: ClientInit,
    link: ClientLink,
    state: RunState,
) -> Result<()> {
    loop {
        if !state.accept_new.load(Ordering::SeqCst) {
            sleep(Duration::from_millis(PULL_PAUSE_MS)).await;
            continue;
        }
        // A role at `max_sessions` stops asking for work it has nowhere to run
        // (plan §5), and the same pause must not stop it hearing the command that
        // frees a slot: a full role is exactly the one whose operator wants to
        // `recycle` or `focus`. Control rows still travel on the ordinary pull
        // when the role has capacity.
        let control_only = !state.dispatch.has_capacity();
        let reply = match link
            .request(ClientOp::Pull(PullArgs {
                role: Some(init.role.clone()),
                limit: PULL_LIMIT,
                hold_ms: Some(PULL_HOLD_MS),
                control_only: control_only.then_some(true),
            }))
            .await
        {
            Ok(reply) => reply,
            Err(error) if transient(&error) => {
                sleep(Duration::from_millis(NOT_READY_PAUSE_MS)).await;
                continue;
            }
            Err(error) => return Err(anyhow!(error)),
        };
        if !reply.ok {
            tracing::warn!(error = ?reply.error, "pull refused");
            sleep(Duration::from_millis(PULL_PAUSE_MS)).await;
            continue;
        }
        let Some(data) = reply.data else { continue };
        let pulled: PullReply = serde_json::from_value(data)?;
        for delivery in pulled.deliveries {
            accept_delivery(&state, &delivery).await;
        }
        state.set_cursor(pulled.seq);
        sleep(Duration::from_millis(PULL_PAUSE_MS)).await;
    }
}

/// Apply one delivered control command and settle its row.
///
/// The row settles whether or not this role still holds the task it names. A
/// command whose session already ended has nothing left to act on, and leaving
/// the row in flight would report an operator's `control` as undelivered.
pub(super) async fn settle_control(state: &RunState, delivery: &Delivery) {
    let ack = |accepted: bool, reason: Option<String>| AckArgs {
        msg_id: delivery.msg_id.clone(),
        op_id: None,
        accepted,
        reason,
    };
    let Some(op) = delivery.envelope.control.clone() else {
        tracing::warn!(msg_id = %delivery.msg_id, "control delivery carried no command");
        state.dispatch.push_settled(ack(
            false,
            Some("control delivery carried no control op".to_string()),
        ));
        return;
    };
    match dispatch::on_control(&state.dispatch, &op).await {
        Ok(held) => {
            tracing::info!(
                op = op.name(),
                task = %op.task_id(),
                held,
                "control command applied"
            );
            state.dispatch.push_settled(ack(true, None));
            if held
                && matches!(op, ControlOp::Recycle { .. } | ControlOp::Cancel { .. })
            {
                // A published exit releases the task's in-flight rows. The
                // command's own row is one of them until its ack is enqueued.
                if let Err(error) =
                    dispatch::sync_session(&state.dispatch, op.task_id()).await
                {
                    tracing::warn!(
                        error = %error,
                        task = %op.task_id(),
                        "a closed control session was not published"
                    );
                }
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, op = op.name(), "control command refused");
            state
                .dispatch
                .push_settled(ack(false, Some(error.to_string())));
        }
    }
}

#[cfg(test)]
mod tests;
