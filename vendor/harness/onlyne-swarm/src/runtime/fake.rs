use super::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct FakeBackend {
    state: Arc<Mutex<HashMap<String, SessionRef>>>,
    pub fail_available: bool,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn sessions(&self) -> HashMap<String, SessionRef> {
        self.state.lock().unwrap().clone()
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
        self.state
            .lock()
            .unwrap()
            .insert(spec.task_id, session.clone());
        Ok(session)
    }
    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        if self.state.lock().unwrap().contains_key(&session.task_id) {
            Ok(session.clone())
        } else {
            anyhow::bail!("fake session not found: {}", session.task_id)
        }
    }
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        Ok(ResourceProbe {
            alive: self.state.lock().unwrap().contains_key(&session.task_id),
            attached: true,
            detail: None,
        })
    }
    fn close(&self, session: &SessionRef, _reason: CloseReason, _force: bool) -> Result<()> {
        self.state.lock().unwrap().remove(&session.task_id);
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
}
