use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct FakeBackend {
    state: Arc<Mutex<HashMap<String, SessionRef>>>,
    failed_probes: Arc<Mutex<HashSet<String>>>,
    /// Every argv this backend was asked to spawn, in order.
    ///
    /// A client's spawn is where the rendered command leaves its own code, and
    /// what it asked for is not visible in the session it got back: a revival
    /// runs the role's `resume` argv and a birth runs its `command`, so a case
    /// that cares which one was asked reads it here.
    spawns: Arc<Mutex<Vec<Vec<String>>>>,
    pub fail_available: bool,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn guard<'a, T>(
        mutex: &'a Mutex<T>,
        task_id: &str,
        what: &str,
    ) -> Result<std::sync::MutexGuard<'a, T>> {
        mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("fake {what} lock poisoned for task {task_id}"))
    }

    /// The argv of every spawn this backend has been asked for, oldest first.
    pub fn spawned_commands(&self) -> Vec<Vec<String>> {
        Self::guard(&self.spawns, "<spawns>", "spawns")
            .unwrap_or_else(|err| panic!("{err}"))
            .clone()
    }

    pub fn sessions(&self) -> HashMap<String, SessionRef> {
        Self::guard(&self.state, "<sessions>", "state")
            .unwrap_or_else(|err| panic!("{err}"))
            .clone()
    }

    /// Force probes for one task to report a dead backend resource.
    pub fn fail_probe(&self, task_id: &str) {
        Self::guard(&self.failed_probes, task_id, "failed_probes")
            .unwrap_or_else(|err| panic!("{err}"))
            .insert(task_id.to_owned());
    }

    /// Clear a forced probe failure for one task.
    pub fn clear_probe_failure(&self, task_id: &str) {
        Self::guard(&self.failed_probes, task_id, "failed_probes")
            .unwrap_or_else(|err| panic!("{err}"))
            .remove(task_id);
    }
}

impl SessionBackend for FakeBackend {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            spawn: true,
            attach: true,
            probe: true,
            close: true,
            focus: true,
            rename: true,
        }
    }
    fn available(&self) -> Result<bool> {
        Ok(!self.fail_available)
    }
    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        Self::guard(&self.spawns, &spec.task_id, "spawns")?.push(spec.command.clone());
        let session = SessionRef {
            task_id: spec.task_id.clone(),
            backend: self.name().into(),
            backend_ref: serde_json::json!({"id": spec.task_id}),
            generation: 1,
        };
        Self::guard(&self.state, &session.task_id, "state")?.insert(spec.task_id, session.clone());
        Ok(session)
    }
    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        if Self::guard(&self.state, &session.task_id, "state")?.contains_key(&session.task_id) {
            Ok(session.clone())
        } else {
            anyhow::bail!("fake session not found: {}", session.task_id)
        }
    }
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        let failed = Self::guard(&self.failed_probes, &session.task_id, "failed_probes")?
            .contains(&session.task_id);
        Ok(ResourceProbe {
            alive: !failed
                && Self::guard(&self.state, &session.task_id, "state")?
                    .contains_key(&session.task_id),
            attached: !failed,
            detail: failed.then(|| serde_json::json!({"forced": "probe_failure"})),
        })
    }
    fn close(&self, session: &SessionRef, _reason: CloseReason, _force: bool) -> Result<()> {
        Self::guard(&self.state, &session.task_id, "state")?.remove(&session.task_id);
        Ok(())
    }
}
