//! The CLI-command layer: every `herdr` invocation this backend makes, the
//! argv it builds, and the decoding of what comes back.

use super::session::HerdrBackend;
use crate::backend::*;

/// herdr `agent start --kind` values, taken from `herdr agent start --help`.
const KIND_TABLE: &[&str] = &[
    "pi",
    "claude",
    "codex",
    "gemini",
    "cursor",
    "devin",
    "agy",
    "cline",
    "omp",
    "mastracode",
    "opencode",
    "copilot",
    "kimi",
    "kiro",
    "droid",
    "amp",
    "grok",
    "hermes",
    "kilo",
    "qodercli",
    "qwen",
    "maki",
    "muse",
];

/// Wait for `agent start` interactive readiness. Measured on herdr 0.9.0.
pub(super) const AGENT_START_TIMEOUT_MS: &str = "25000";

impl HerdrBackend {
    fn cli_env(&self) -> BTreeMap<String, String> {
        self.env
            .iter()
            .filter(|(key, _)| key.starts_with("HERDR_"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    pub(super) fn json(&self, args: Vec<String>) -> Result<Value> {
        run_json(
            self.runner.as_ref(),
            &self.command,
            &args,
            None,
            &self.cli_env(),
        )
    }

    pub(super) fn run_line(&self, args: Vec<String>) -> Result<CommandOutput> {
        run_checked(
            self.runner.as_ref(),
            &self.command,
            &args,
            None,
            &self.cli_env(),
        )
    }
}

#[cfg(any(test, unix))]
pub(super) fn posix_shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(any(test, windows))]
pub(super) fn cmd_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn shell_quote(value: &str) -> String {
    #[cfg(unix)]
    {
        posix_shell_quote(value)
    }
    #[cfg(windows)]
    {
        cmd_quote(value)
    }
}

pub(super) fn pane_run_line(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The cwd in the spelling herdr needs: absolute.
///
/// herdr resolves a relative `--cwd` against its own working directory, which
/// placed session panes in `$HOME` for a client started with a relative
/// `--workspace`. `std::path::absolute` is lexical (Rust 1.79+) and gives a
/// relative path the client's own current directory, so a path that is already
/// absolute keeps its spelling. A refusal falls back to the literal, matching
/// `pipe_name_for` in `onlyne-layout`.
pub(super) fn absolute_cwd(cwd: &Path) -> String {
    std::path::absolute(cwd)
        .unwrap_or_else(|_| cwd.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub(super) fn kind_of(command: &[String]) -> Option<String> {
    let first = command.first()?;
    let name = std::path::Path::new(first)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(first)
        .to_ascii_lowercase();
    KIND_TABLE
        .iter()
        .find(|kind| **kind == name)
        .map(|kind| (*kind).to_string())
}
