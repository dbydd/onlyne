//! External placement: the runtime is already resident, so the client starts
//! nothing of its own.
//!
//! `plugin × external` is the row of the plan's table where the runtime stays
//! up on its own and its plugin dials the client (`docs/v2-PLAN.md` line 289):
//! one DSH serving several roles, each role's client single-purpose. So a
//! session this placement opens is a name the client stages, not a process it
//! owns: the socket the client serves is published, the plugin mounts on it,
//! and from there the session rides the ordinary adapter path — the `assign`
//! frame, the reports, the settle — which is why this backend writes no spawn.
//!
//! Two consequences are deliberate rather than missing:
//!
//! * [`SessionBackend::spawn`] starts nothing, whatever argv the role's
//!   `[client.runtime] command` carries. The placement is the machine's answer
//!   to "who starts the runtime", and here the answer is "the operator did".
//! * [`SessionBackend::close`] stops nothing. The process belongs to the
//!   operator, and only this client's record of the session ends here.
//!
//! The death window of such a session is the adapter transport's, exactly as it
//! is for a `plugin × herdr` session: a plugin that never mounts is retired by
//! `[client] reconnect_grace_secs`, not by a `try_wait` this backend cannot run.

use super::*;

#[derive(Clone, Default)]
pub struct ExternalBackend;

impl ExternalBackend {
    pub fn new() -> Self {
        Self
    }
}

impl SessionBackend for ExternalBackend {
    fn name(&self) -> &'static str {
        "external"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            spawn: true,
            attach: true,
            probe: true,
            close: true,
            focus: false,
            rename: false,
        }
    }
    fn available(&self) -> Result<bool> {
        Ok(true)
    }
    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        tracing::info!(
            task = %spec.task_id,
            command = %spec.command.join(" "),
            "external placement: the client starts no process; the resident runtime dials in"
        );
        Ok(SessionRef {
            task_id: spec.task_id.clone(),
            backend: self.name().into(),
            backend_ref: serde_json::json!({
                "id": spec.task_id,
                "placement": "external",
            }),
            generation: 1,
        })
    }
    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        // The resource is the connection the runtime opened, and this client
        // holds that itself: there is nothing outside the process to probe.
        Ok(session.clone())
    }
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        Ok(ResourceProbe {
            alive: true,
            attached: true,
            detail: Some(serde_json::json!({
                "placement": "external",
                "task": session.task_id,
            })),
        })
    }
    fn close(&self, session: &SessionRef, reason: CloseReason, _force: bool) -> Result<()> {
        tracing::info!(
            task = %session.task_id,
            ?reason,
            "external session closed; no process of this client's own to stop"
        );
        Ok(())
    }
}
