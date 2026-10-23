//! The CLI-command layer: every `tern` invocation this backend makes, the
//! argv it builds, and the decoding of what comes back.

use super::session::TernBackend;
use crate::backend::*;

/// The tern binary this backend drives when the environment names none.
///
/// Absolute, never a bare `tern` looked up on `PATH`: an unrelated `tern` on
/// the path would answer `ls --json` with a document this module cannot read,
/// and the failure would name neither the wrong binary nor the right one. The
/// default lives in [`crate::backend::select::TERN_BINARY`] so the doctor's
/// host-binary check and this argv cannot drift apart.
pub(super) fn default_command() -> String {
    crate::backend::select::TERN_BINARY.to_string()
}

/// The window key the daemon puts in the environment of every pane, and the
/// one `--window KEY` value that scopes every call this backend makes.
const WINDOW_KEY: &str = "TERN_WINDOW_KEY";

impl TernBackend {
    /// The environment tern is invoked with: its own `TERN_*` variables, and
    /// nothing else.
    ///
    /// A pane's environment carries the whole client's — the tools-mount token
    /// included, which is a capability a model in the pane must not be able to
    /// read. `spawn` passes the session contract to the command itself, after
    /// the `--` separator, so none of it has to travel through this map.
    pub(super) fn cli_env(&self) -> BTreeMap<String, String> {
        self.env
            .iter()
            .filter(|(key, _)| key.starts_with("TERN_"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// The `--window KEY` argument, when the process knows which window it is
    /// in.
    ///
    /// Tern scopes every command to `--window KEY`'s sessions, else to the
    /// window of the pane the command runs in, else to the first window. An
    /// empty `--window ""` is accepted and ignored — measured, and the reason
    /// the flag is left off rather than sent empty: a client outside any Tern
    /// pane has no window of its own and must drive the first one, which is
    /// what omitting the flag does.
    pub(super) fn window_args(&self) -> Vec<String> {
        match self.env.get(WINDOW_KEY) {
            Some(key) if !key.trim().is_empty() => {
                vec!["--window".into(), key.trim().to_string()]
            }
            _ => Vec::new(),
        }
    }

    /// One `tern` call, JSON on stdout, with the window argument spliced in.
    ///
    /// Spliced before the `--` separator rather than appended, because `split`
    /// carries the session command after it: an appended `--window KEY` would
    /// land inside that command's own argv, where the pane's program would
    /// read it as its argument. A call with no separator — every other call
    /// here — gets the argument at the end, which is where tern reads it.
    pub(super) fn json(&self, mut args: Vec<String>) -> Result<Value> {
        let window = self.window_args();
        if let Some(separator) = args.iter().position(|arg| arg == "--") {
            // An empty window splices nothing, leaving the argv as built.
            args.splice(separator..separator, window);
        } else if !window.is_empty() {
            args.extend(window);
        }
        run_json(
            self.runner.as_ref(),
            &self.command,
            &args,
            None,
            &self.cli_env(),
        )
    }
}

/// The cwd in the spelling tern needs: absolute.
///
/// Tern resolves a relative `--cwd` against the daemon's working directory,
/// which is nothing the client chose. `std::path::absolute` is lexical (Rust
/// 1.79+) and gives a relative path the client's own current directory, so an
/// already-absolute path keeps its spelling. A refusal falls back to the
/// literal, matching `absolute` in the orca backend.
pub(super) fn absolute_cwd(cwd: &Path) -> String {
    std::path::absolute(cwd)
        .unwrap_or_else(|_| cwd.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Tern's own words for a resource that no longer exists.
///
/// Tern writes its refusals as plain stderr text (`tern close: no block is
/// called \`999\``) and exits 1, with no JSON body — so
/// [`failure_code`] reads `None` for every one of them, and the code the rest
/// of this crate branches on has no spelling to match. The message is the only
/// machine-readable part of a refusal, so these are matched against it.
///
/// Both templates are fixed strings from the ternary's binary: the block form
/// reads `no block is called \`<BLOCK>\`` and the session form
/// `no session is called \`<SESSION>\``, each prefixed by the command name
/// (`tern close: `, `tern focus: `). A refusal about something else — a
/// session that is already there, a flag tern does not know — reads differently
/// and stays an error.
const GONE_BLOCK: &str = "no block is called";
const GONE_SESSION: &str = "no session is called";

/// Whether this refusal says the block is gone, so a close is a no-op.
pub(super) fn block_is_gone(error: &anyhow::Error) -> bool {
    mentions(error, GONE_BLOCK)
}

/// Whether this refusal says the session is gone, which also means the block
/// it held is.
pub(super) fn session_is_gone(error: &anyhow::Error) -> bool {
    mentions(error, GONE_SESSION)
}

fn mentions(error: &anyhow::Error, needle: &str) -> bool {
    error.to_string().contains(needle)
}
