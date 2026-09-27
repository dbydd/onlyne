//! The CLI-command layer: every `orca` invocation this backend makes, the argv
//! it builds, and the decoding of what comes back.

use super::session::OrcaBackend;
use crate::backend::*;

impl OrcaBackend {
    pub(super) fn json(&self, args: Vec<String>) -> Result<Value> {
        run_json(
            self.runner.as_ref(),
            &self.command,
            &args,
            None,
            &BTreeMap::new(),
        )
    }

    /// `terminal show` for one handle.
    pub(super) fn show(&self, handle: &str) -> Result<Value> {
        self.json(vec![
            "terminal".into(),
            "show".into(),
            "--terminal".into(),
            handle.into(),
            "--json".into(),
        ])
    }

    /// Terminal rows of one worktree, or of every worktree when no selector
    /// is known.
    fn list(&self, selector: Option<&str>) -> Result<Vec<Value>> {
        let mut args = vec!["terminal".into(), "list".into()];
        if let Some(selector) = selector {
            args.push("--worktree".into());
            args.push(selector.into());
        }
        args.push("--json".into());
        let value = self.json(args)?;
        Ok(value
            .pointer("/terminals")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// `terminal list` rows for a remint. A selector this daemon can no longer
    /// resolve must not hide the pane, so the host-wide listing is the
    /// fallback.
    pub(super) fn rows(&self, selector: Option<&str>) -> Result<Vec<Value>> {
        match self.list(selector) {
            Ok(rows) => Ok(rows),
            Err(error) if selector.is_some() && is_selector_not_found(&error) => self.list(None),
            Err(error) => Err(error),
        }
    }

    /// `terminal close` for one handle.
    pub(super) fn close_handle(&self, handle: &str) -> Result<()> {
        self.json(vec![
            "terminal".into(),
            "close".into(),
            "--terminal".into(),
            handle.into(),
            "--json".into(),
        ])
        .map(|_| ())
    }
}

/// Terminal states that end a pane. Some Orca builds omit `status` entirely,
/// which is why [`OrcaBackend::probe`] also reads `exitCause`.
pub(super) const DEAD_STATUS: [&str; 3] = ["exited", "closed", "dead"];

/// Orca's codes for a handle that no longer names the current PTY
/// incarnation. The pane keeps its `pane_key`, so a stale handle is
/// recoverable through [`OrcaBackend::remint`].
const STALE_HANDLE_CODES: [&str; 2] = ["terminal_handle_stale", "terminal_gone"];

/// Whether the CLI refused because the stored handle went stale.
pub(super) fn is_stale(error: &anyhow::Error) -> bool {
    failure_code(error).is_some_and(|code| STALE_HANDLE_CODES.contains(&code))
}

/// Whether the CLI named a resource that no longer exists, which makes a
/// close a no-op instead of an error.
pub(super) fn is_gone(error: &anyhow::Error) -> bool {
    is_stale(error) || failure_code(error) == Some("terminal_not_found")
}

/// Whether the CLI cannot resolve a worktree selector: the answer for a
/// selector Orca dropped since the tab was created.
fn is_selector_not_found(error: &anyhow::Error) -> bool {
    failure_code(error) == Some("selector_not_found")
}

#[cfg(any(test, unix))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(any(test, windows))]
fn cmd_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Build the command executed inside the Orca terminal.
///
/// Tab ownership and working directory are independent: `--worktree` decides
/// which tab list the tab joins, while this command's `cd` decides what the
/// agent sees. So the tab lives flat among the supervisor's tabs (the
/// supervisor's worktree) while the process runs in the role workspace, which
/// Orca is never told about. Environment entries travel through `env` so the
/// terminal process receives the same spawn contract as zellij and other
/// backends.
///
/// The line ends with `exit` because the tab's own shell is what runs it: Orca
/// creates a shell and hands it this command, so the shell — not the session
/// command — owns the tab. Without the tail the shell drops to a prompt when the
/// command returns, and the tab outlives its session reading `running` forever
/// (measured on Orca 1.4.198: a bare `sleep 3` leaves a prompt and the tab
/// behind, while the same line ending in `exit` reclaims it). zellij takes no
/// equivalent tail: its launch line is argv after `--`, with no wrapping shell.
///
/// The separator is `;`, not `&&`, so the tail runs on the failure path too. A
/// crashed or misconfigured session command is exactly the case that must not
/// leave a tab sitting at a prompt, and `&&` would skip the `exit` there and
/// keep it. The command's own output is still in the pane's scrollback and its
/// exit status rides through `exit`'s default.
#[cfg(any(test, unix))]
pub(super) fn spawn_command_posix(spec: &SpawnSpec) -> Result<String> {
    if spec.command.is_empty() {
        anyhow::bail!("orca spawn requires a command");
    }
    let mut command = format!("cd {} &&", shell_quote(&spec.cwd.to_string_lossy()));
    if !spec.env.is_empty() {
        command.push_str(" env");
        for (key, value) in &spec.env {
            command.push(' ');
            command.push_str(&shell_quote(&format!("{key}={value}")));
        }
    }
    for arg in &spec.command {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    command.push_str("; exit");
    Ok(command)
}

/// cmd.exe spelling of [`spawn_command_posix`]: `cd /d`, `set "K=V"`, `& exit`.
#[cfg(any(test, windows))]
pub(super) fn spawn_command_cmd(spec: &SpawnSpec) -> Result<String> {
    if spec.command.is_empty() {
        anyhow::bail!("orca spawn requires a command");
    }
    let mut command = format!("cd /d {} &&", cmd_quote(&spec.cwd.to_string_lossy()));
    for (key, value) in &spec.env {
        command.push_str(" set ");
        command.push_str(&format!("\"{key}={}\"", value.replace('"', "\"\"")));
        command.push_str(" &&");
    }
    for arg in &spec.command {
        command.push(' ');
        command.push_str(&cmd_quote(arg));
    }
    command.push_str(" & exit");
    Ok(command)
}

pub(super) fn spawn_command(spec: &SpawnSpec) -> Result<String> {
    #[cfg(unix)]
    {
        spawn_command_posix(spec)
    }
    #[cfg(windows)]
    {
        spawn_command_cmd(spec)
    }
}
