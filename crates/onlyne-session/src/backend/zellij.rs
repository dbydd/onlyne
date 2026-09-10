use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct ZellijBackend {
    runner: Arc<dyn Runner>,
    command: String,
}
impl ZellijBackend {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            command: std::env::var("ZELLIJ_COMMAND").unwrap_or_else(|_| "zellij".into()),
        }
    }
}
impl SessionBackend for ZellijBackend {
    fn name(&self) -> &'static str {
        "zellij"
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
        Ok(self
            .runner
            .run(
                &self.command,
                &["list-sessions".into(), "--short".into()],
                None,
                &BTreeMap::new(),
            )
            .map(|o| o.status == 0)
            .unwrap_or(false))
    }
    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        let session = format!("onlyne-{}", spec.task_id);
        let mut args = vec![
            "--session".into(),
            session.clone(),
            "run".into(),
            "--cwd".into(),
            spec.cwd.to_string_lossy().into_owned(),
            "--no-focus".into(),
            "--".into(),
        ];
        args.extend(spec.command);
        let output = run_checked(self.runner.as_ref(), &self.command, &args, None, &spec.env)?;
        let pane = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if pane.is_empty() {
            return Err(anyhow::anyhow!("zellij run returned no pane id"));
        }
        Ok(SessionRef {
            task_id: spec.task_id,
            backend: self.name().into(),
            backend_ref: serde_json::json!({"session": session, "pane": pane}),
            generation: 1,
        })
    }
    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        let name = session
            .backend_ref
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("zellij session ref missing session"))?;
        let out = self.runner.run(
            &self.command,
            &["list-sessions".into(), "--short".into()],
            None,
            &BTreeMap::new(),
        )?;
        if out.status != 0
            || !String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == name)
        {
            return Err(anyhow::anyhow!("zellij session not found: {name}"));
        }
        Ok(session.clone())
    }
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        let attached = self.attach(session).is_ok();
        Ok(ResourceProbe {
            alive: attached,
            attached,
            detail: None,
        })
    }
    fn close(&self, session: &SessionRef, _reason: CloseReason, _force: bool) -> Result<()> {
        let name = session
            .backend_ref
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("zellij session ref missing session"))?;
        run_checked(
            self.runner.as_ref(),
            &self.command,
            &["kill-session".into(), name.into()],
            None,
            &BTreeMap::new(),
        )
        .map(|_| ())
    }
}
