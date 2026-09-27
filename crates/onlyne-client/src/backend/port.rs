use super::*;

pub trait SessionBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    fn available(&self) -> Result<bool>;
    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef>;
    fn attach(&self, session: &SessionRef) -> Result<SessionRef>;
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe>;
    fn close(&self, session: &SessionRef, reason: CloseReason, force: bool) -> Result<()>;
    fn rename(&self, _session: &SessionRef, _title: &str) -> Result<()> {
        Err(unsupported(
            self.name(),
            "rename",
            "backend does not expose rename",
        ))
    }
    fn focus(&self, _session: &SessionRef) -> Result<()> {
        Err(unsupported(
            self.name(),
            "focus",
            "backend does not expose focus",
        ))
    }

    /// Whether this backend carries task payloads itself. A backend that
    /// answers true delivers through [`SessionBackend::deliver`]; one that
    /// answers false keeps the adapter-socket path where a mounted plugin
    /// receives `assign` or `config_get{key:"stdin:<text>"}`.
    fn self_driven(&self) -> bool {
        false
    }

    /// Hand one task payload to a session this backend owns.
    fn deliver(&self, _session: &SessionRef, _task_id: &str, _prose: &str) -> Result<()> {
        Err(unsupported(
            self.name(),
            "deliver",
            "backend delivers through an adapter socket",
        ))
    }

    /// Install the client-owned destination for records journalled by this
    /// backend. Backends without a journal keep the default no-op.
    fn set_content_sink(&self, _sink: Arc<dyn crate::content::ContentSink>) {}

    /// Terminal facts this backend observed without an adapter report.
    /// Absent for backends whose sessions end through the adapter socket.
    fn outcomes(&self) -> Option<OutcomeFeed> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub trait Runner: Send + Sync {
    fn run(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Result<CommandOutput>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Result<CommandOutput> {
        let mut command = std::process::Command::new(program);
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command.envs(env);
        let output = command.output()?;
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}
