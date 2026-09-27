use crate::session::dispatch::DispatchState;
use anyhow::{Context, Result};
use onlyne_adapter::AdapterIo;
use onlyne_config::layout::RoleWorkspace;
use onlyne_proto::{AdapterMsg, HelloArgs, HostOp, MountKind, PROTOCOL_VERSION, PluginOp};
use onlyne_wire::socket::prelude::TokioListener;
use onlyne_wire::socket::{
    LocalListener, RegistrationFile, SocketEndpoint, bind_socket_v2, connect_local,
    registration_path, remove_registration,
};
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
    /// One accessor answers for the whole tree, so the daemon that bound
    /// `<runtime>/<digest>.sock` and a caller that derived it from the
    /// workspace root reach the same socket.
    pub fn path(&self) -> PathBuf {
        RoleWorkspace::resolve(&self.workspace).socket_path()
    }

    /// The registration this client publishes beside its socket.
    ///
    /// The kind is the client's own: a role workspace's daemon serves the
    /// client surface, and no reader infers that from what happens to sit
    /// beside the tree. `role` names the one role this process serves, and
    /// `runtime` names the session backend that hosts it, which is the field an
    /// external runtime's plugin matches on to find the client its session
    /// belongs to.
    pub fn registration(&self) -> RegistrationFile {
        let root = RoleWorkspace::resolve(&self.workspace);
        RegistrationFile::client(root.root())
            .with_role(self.role.clone())
            .with_runtime(self.dispatch.runtime_name())
    }

    /// Bind the workspace socket and report the endpoint that was served.
    ///
    /// [`bind_socket_v2`] owns the whole sequence — resolve the runtime
    /// directory, drop a stale name, and bind `<runtime>/<digest>.sock` — and
    /// its failure message names the path it tried. The registration is
    /// published after the bind succeeds, so a surface that never opened never
    /// names itself.
    ///
    /// The socket is served off the canonical `<run_dir>/s` spelling, so the log
    /// line names that spelling too: it is what an operator reading the tree
    /// will look for and not find.
    #[allow(clippy::unused_async)]
    pub async fn bind(&self) -> Result<(LocalListener, SocketEndpoint)> {
        let layout = RoleWorkspace::resolve(&self.workspace);
        let endpoint = layout.socket_endpoint();
        let listener = bind_socket_v2(layout.root()).with_context(|| {
            format!(
                "bind the workspace socket {}",
                layout.socket_path().display(),
            )
        })?;
        endpoint
            .publish(&self.registration())
            .with_context(|| {
                format!(
                    "publish the client registration {}",
                    endpoint.registration().display(),
                )
            })?;
        // Under v2 the socket always lives in the runtime directory, so serving
        // off the `<run>/s` spelling is the normal case and not a move worth a
        // warning. The canonical spelling is logged because it is what an
        // operator reading the tree will look for and not find.
        tracing::info!(
            canonical = %endpoint.natural().display(),
            served = %endpoint.actual().display(),
            registration = %endpoint.registration().display(),
            role = %self.role,
            "adapter socket serving"
        );
        Ok((listener, endpoint))
    }

    /// Answer every connection on `listener` for the life of the process.
    ///
    /// The listener stays open across a failed `accept`: one bad handshake on one
    /// socket is that client's problem, and every plugin already mounted would
    /// lose its transport if the surface came down for it. The pause keeps a
    /// persistent failure — a descriptor ceiling, a name pulled out from under
    /// the listener — from spinning the loop at full speed.
    ///
    /// The registration published by [`bind`](Self::bind) belongs to this scope:
    /// it names this process as the client for the workspace, and the guard here
    /// takes the file with it when the surface stops serving, however it stops.
    pub async fn accept_loop(&self, listener: LocalListener) -> Result<()> {
        let _registration = RegistrationGuard {
            root: RoleWorkspace::resolve(&self.workspace)
                .root()
                .to_path_buf(),
        };
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

/// The registration a serving surface owns for as long as it serves.
///
/// The file is the one answer to "which client serves this workspace", and a
/// registration outliving its surface is what an external runtime's plugin
/// reads as a live client. Tying it to the scope means every end reaches the
/// removal: a caller that drops the future, an acceptor the run aborts, and a
/// surface that fails.
struct RegistrationGuard {
    root: PathBuf,
}

impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        if let Err(error) = remove_registration(&self.root) {
            tracing::warn!(
                error = %error,
                registration = %registration_path(&self.root).display(),
                "could not remove the client registration"
            );
        } else {
            tracing::info!(root = %self.root.display(), "removed the client registration");
        }
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
