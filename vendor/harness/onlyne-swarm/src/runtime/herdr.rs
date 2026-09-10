use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Herdr has no stable, documented machine-readable spawn/probe contract in
/// this repository. The adapter exposes availability and explicit failures.
pub struct HerdrBackend {
    runner: Arc<dyn Runner>,
    command: String,
}
impl HerdrBackend {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            command: std::env::var("HERDR_COMMAND").unwrap_or_else(|_| "herdr".into()),
        }
    }
}
impl SessionBackend for HerdrBackend {
    fn name(&self) -> &'static str {
        "herdr"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            spawn: false,
            attach: false,
            probe: false,
            close: false,
            focus: false,
            rename: false,
        }
    }
    fn available(&self) -> Result<bool> {
        Ok(self
            .runner
            .run(
                &self.command,
                &["status".into(), "--json".into()],
                None,
                &BTreeMap::new(),
            )
            .map(|o| o.status == 0)
            .unwrap_or(false))
    }
    fn spawn(&self, _spec: SpawnSpec) -> Result<SessionRef> {
        Err(unsupported(
            self.name(),
            "spawn",
            "Herdr agent/pane JSON shape is unspecified",
        ))
    }
    fn attach(&self, _session: &SessionRef) -> Result<SessionRef> {
        Err(unsupported(
            self.name(),
            "attach",
            "Herdr agent/pane reference shape is unspecified",
        ))
    }
    fn probe(&self, _session: &SessionRef) -> Result<ResourceProbe> {
        Err(unsupported(
            self.name(),
            "probe",
            "Herdr liveness JSON shape is unspecified",
        ))
    }
    fn close(&self, _session: &SessionRef, _reason: CloseReason, _force: bool) -> Result<()> {
        Err(unsupported(
            self.name(),
            "close",
            "Herdr pane close contract is unspecified",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_operations_are_explicit() {
        let backend = HerdrBackend::new(Arc::new(ProcessRunner));
        let error = backend
            .spawn(SpawnSpec {
                cwd: ".".into(),
                task_id: "t".into(),
                command: vec![],
                env: BTreeMap::new(),
                focus: None,
                rename: None,
            })
            .unwrap_err();
        assert!(error.to_string().contains("does not support spawn"));
    }
}
