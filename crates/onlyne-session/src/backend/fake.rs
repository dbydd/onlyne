use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct FakeBackend {
    state: Arc<Mutex<HashMap<String, SessionRef>>>,
    failed_probes: Arc<Mutex<HashSet<String>>>,
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reducer_like_spawn_probe_close() {
        let backend = FakeBackend::new();
        let spec = SpawnSpec {
            cwd: ".".into(),
            task_id: "task".into(),
            command: vec!["pi".into()],
            env: BTreeMap::new(),
            focus: None,
            rename: None,
        };
        let session = backend.spawn(spec).unwrap();
        assert!(backend.probe(&session).unwrap().alive);
        backend
            .close(&session, CloseReason::Completed, false)
            .unwrap();
        assert!(!backend.probe(&session).unwrap().alive);
    }

    #[test]
    fn forced_probe_failure_reports_dead_without_closing() {
        let backend = FakeBackend::new();
        let session = backend
            .spawn(SpawnSpec {
                cwd: ".".into(),
                task_id: "ghost-task".into(),
                command: vec!["agent".into()],
                env: BTreeMap::new(),
                focus: None,
                rename: None,
            })
            .unwrap();
        assert!(backend.probe(&session).unwrap().alive);
        backend.fail_probe("ghost-task");
        let probe = backend.probe(&session).unwrap();
        assert!(!probe.alive);
        assert!(!probe.attached);
        assert!(probe.detail.is_some());
        backend.clear_probe_failure("ghost-task");
        assert!(backend.probe(&session).unwrap().alive);
    }
}
