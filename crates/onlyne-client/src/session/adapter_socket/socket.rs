use crate::session::dispatch::DispatchState;
use anyhow::{Context, Result};
use onlyne_adapter::AdapterIo;
use onlyne_layout::local_socket::prelude::TokioListener;
use onlyne_layout::{LocalListener, RoleWorkspace, SocketEndpoint, bind_socket, connect_local};
use onlyne_proto::{AdapterMsg, HelloArgs, HostOp, MountKind, PROTOCOL_VERSION, PluginOp};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone)]
pub struct AdapterSocket {
    pub workspace: PathBuf,
    pub role: String,
    pub cluster: String,
    pub server: String,
    pub dispatch: DispatchState,
}

impl AdapterSocket {
    /// The path this role's clients connect to, per
    /// [`RoleWorkspace::socket_path`].
    ///
    /// One accessor answers for the whole tree, so a daemon that bound the short
    /// path and a caller that derived it from the workspace root reach the same
    /// socket through the marker in `<run>/socket`.
    pub fn path(&self) -> PathBuf {
        RoleWorkspace::resolve(&self.workspace).socket_path()
    }

    /// Bind the workspace socket and report the endpoint that was served.
    ///
    /// [`bind_socket`] owns the whole sequence — resolve, create the run
    /// directory, drop a stale name, bind, publish the marker — and its failure
    /// message carries both spellings it tried with each length.
    ///
    /// A short endpoint means the socket moved off the canonical path, which an
    /// operator reading a stale `s` would otherwise chase forever, so the log
    /// line names the canonical path, its byte length, the served path, and the
    /// marker that carries the answer.
    #[allow(clippy::unused_async)]
    pub async fn bind(&self) -> Result<(LocalListener, SocketEndpoint)> {
        let layout = RoleWorkspace::resolve(&self.workspace);
        let (listener, endpoint) =
            bind_socket(layout.root(), &layout.run_dir()).with_context(|| {
                format!(
                    "bind the workspace socket {}",
                    layout.socket_path_natural().display(),
                )
            })?;
        if endpoint.short() {
            let natural = endpoint.natural();
            tracing::warn!(
                canonical = %natural.display(),
                canonical_bytes = natural.as_os_str().len(),
                served = %endpoint.actual().display(),
                marker = %endpoint.marker().display(),
                "adapter socket moved to the short path"
            );
        } else {
            tracing::info!(socket = %endpoint.actual().display(), "adapter socket serving");
        }
        Ok((listener, endpoint))
    }

    /// Answer every connection on `listener` for the life of the process.
    ///
    /// The listener stays open across a failed `accept`: one bad handshake on one
    /// socket is that client's problem, and every plugin already mounted would
    /// lose its transport if the surface came down for it. The pause keeps a
    /// persistent failure — a descriptor ceiling, a name pulled out from under
    /// the listener — from spinning the loop at full speed.
    pub async fn accept_loop(&self, listener: LocalListener) -> Result<()> {
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    let this = self.clone();
                    tokio::spawn(async move {
                        if let Err(err) = this.connection(stream).await {
                            tracing::debug!(error = %err, "adapter connection closed");
                        }
                    });
                }
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        kind = ?error.kind(),
                        "adapter socket accept failed; retrying"
                    );
                    tokio::time::sleep(ACCEPT_RETRY_PAUSE).await;
                }
            }
        }
    }

    /// Bind and serve in one call, for a caller that owns no endpoint interest.
    pub async fn serve(self) -> Result<()> {
        let (listener, _) = self.bind().await?;
        self.accept_loop(listener).await
    }
}

/// Wait bound for the link probe's `hello` round trip.
///
/// The handshake runs over a local socket, and the bound matches the adapter
/// protocol's own hello budget.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Pause before the accept loop asks the listener for a connection again after
/// an `accept` failure.
///
/// The value is short enough that a transient failure costs one plugin one
/// keystroke and long enough that a persistent one stays off the CPU.
pub const ACCEPT_RETRY_PAUSE: Duration = Duration::from_millis(100);

/// The link state of the client serving `socket`, and `None` when no client
/// answers the probe.
///
/// The probe is an `admin` `hello` on the adapter surface (§7): it names no
/// mount, binds nothing, and the host's `HelloAck` carries the link state. A
/// socket nobody answers is a client that is not serving — `status` reads that
/// as not running — while a client that answers with a down link is running
/// and disconnected.
pub async fn server_link_state(socket: &Path) -> Option<bool> {
    let stream = connect_local(socket).await.ok()?;
    let io = AdapterIo::new(stream, PROBE_TIMEOUT, PROBE_TIMEOUT);
    let hello = HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: "onlyne-client".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        kind: MountKind::Admin,
        capabilities: Vec::new(),
        mount: None,
    };
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Hello(hello)))
        .await
        .ok()?;
    if !body.ok {
        return Some(false);
    }
    let ack = body
        .data
        .and_then(|value| serde_json::from_value::<HostOp>(value).ok());
    Some(matches!(
        ack,
        Some(HostOp::Welcome(ack)) if ack.server.connected
    ))
}

/// Remove a socket file a previous run left behind.
///
/// The leaf is absent in the common case, and `NotFound` is that answer. A name
/// that holds a live listener answers `connect` and keeps the bind of a client
/// restarting behind it, so the readiness path clears it first, and the log line
/// is the record that a surface was cleared.
pub async fn stale_socket_removed(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {
            tracing::info!(socket = %path.display(), "removed stale adapter socket");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove stale socket {}", path.display())),
    }
}
