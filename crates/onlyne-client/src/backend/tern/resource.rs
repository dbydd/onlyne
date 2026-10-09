//! The resource layer: blocks, the processes inside them, and focus, as tern
//! reports them.

use super::cli::{absolute_cwd, block_is_gone, session_is_gone};
use super::policy::{Listing, created_id, split_word};
use super::session::TernBackend;
use crate::backend::*;

impl TernBackend {
    /// Launch the agent as a new session's or tab's only block — the whole
    /// answer to a missing session or tab.
    ///
    /// `tern new session` and `tern new tab` create the container and launch
    /// the command in its first block as one call, so the agent is the first
    /// thing the role tab ever held and no anchor shell is made for it. The
    /// argv travels after `--`, environment prefixed inside the pane's shell
    /// ([`launch_argv`]), exactly as a split carries it.
    ///
    /// The placement's direction is not sent: there is no split to take one.
    /// A `new tab` answer's session is checked against `expected_session` —
    /// the same check [`split_and_start`] makes — because a `--window` that
    /// addressed another window would strand the block beyond this backend's
    /// reach; a `new session` names no session to check against.
    pub(super) fn launch(
        &self,
        prefix: Vec<String>,
        expected_session: Option<&str>,
        spec: &SpawnSpec,
    ) -> Result<Value> {
        let mut args = prefix;
        args.extend([
            "--cwd".into(),
            absolute_cwd(&spec.cwd),
            "--keep-open".into(),
            "--json".into(),
            "--".into(),
        ]);
        args.extend(launch_argv(spec));
        let created = self.json(args)?;
        if let Some(expected) = expected_session {
            if let Some(created_session) = created_id(&created, "session") {
                if created_session != expected {
                    anyhow::bail!(
                        "tern answered in session {created_session}, not {expected}: the \
                             TERN_WINDOW_KEY this client used addresses another window"
                    );
                }
            }
        }
        Ok(created)
    }

    /// Close the one block this spawn created, keeping the original error.
    ///
    /// A post-create failure — a refused rename, an answer from another
    /// window — must not leave a live agent pane nothing addresses. The close
    /// is best-effort and its failures logged, never reported instead of the
    /// failure that caused it; nothing else this call may have touched — a
    /// session or tab holding no other block — is deleted, because onlyne
    /// never deletes a host resource it did not name as its own.
    pub(super) fn close_created(&self, pane_id: &str) {
        let _ = self
            .json(vec!["close".into(), pane_id.into(), "--json".into()])
            .inspect_err(|error| {
                tracing::warn!(pane_id, error = %error, "tern left a spawn's own block open");
            });
    }

    /// Split a new block beside `base_pane` and put the session command in it.
    ///
    /// `tern split BLOCK right|down` creates the block and launches in it as
    /// one call, so there is no window in which a block exists without its
    /// command. `--keep-open` keeps the block addressable after the command
    /// returns: without it a block whose program succeeds closes itself, and a
    /// finished agent would take the only handle that names its pane.
    ///
    /// The command travels after `--` as argv, not as a shell line, so a task
    /// id or a title with a space in it needs no quoting.
    ///
    /// The session's environment is set inside the block rather than passed to
    /// the CLI, because tern's launch options carry no `--env` — see
    /// [`launch_argv`].
    ///
    /// The ratio is not sent: `tern split` takes no ratio, so a placement's
    /// uneven ratio has nowhere on this host to go. Only the direction, which
    /// tern does take, travels.
    ///
    /// The session and tab the block reports are checked against the ones the
    /// caller asked to split inside. A `--window` that addressed another
    /// window answers with that window's ids, and a pane in a session this
    /// client never named would be one no later call could address; catching
    /// it here names the window instead of leaving a stranded block.
    pub(super) fn split_and_start(
        &self,
        base_pane: &str,
        spec: &SpawnSpec,
        placement: PanePlacement,
        session_id: &str,
        tab_id: &str,
    ) -> Result<String> {
        if spec.command.is_empty() {
            anyhow::bail!("tern spawn requires a command");
        }
        let mut args = vec![
            "split".into(),
            base_pane.into(),
            split_word(placement.direction).into(),
            "--cwd".into(),
            absolute_cwd(&spec.cwd),
            "--keep-open".into(),
            "--json".into(),
            "--".into(),
        ];
        args.extend(launch_argv(spec));
        let created = self.json(args)?;
        if let Some(created_session) = created_id(&created, "session") {
            if created_session != session_id {
                anyhow::bail!(
                    "tern split answered in session {created_session}, not {session_id}: the \
                     TERN_WINDOW_KEY this client used addresses another window"
                );
            }
        }
        if let Some(created_tab) = created_id(&created, "tab") {
            if created_tab != tab_id {
                anyhow::bail!(
                    "tern split answered in tab {created_tab}, not {tab_id}: the block it made \
                     is not in the role's tab"
                );
            }
        }
        created_id(&created, "block")
            .ok_or_else(|| anyhow::anyhow!("tern split returned no block id"))
    }

    /// Focus one block, in every window.
    ///
    /// `tern focus BLOCK` is a single call where herdr needed a workspace hop,
    /// a tab hop and a pane hop: tern's help reads `every window shows it`, so
    /// one call is the whole path and the tab hop has no counterpart to skip.
    pub(super) fn focus_block(&self, pane_id: &str) -> Result<()> {
        self.json(vec!["focus".into(), pane_id.into(), "--json".into()])
            .map(|_| ())
    }

    /// The block holding focus, with the tab and session that hold it.
    ///
    /// Tern reports `focused` per block per tab, and a window with several
    /// sessions shows one focused block in each — so "the focused block" means
    /// nothing without the tab and session around it, and the caller compares
    /// all three. `None` when this window's listing holds no focused block at
    /// all, which is what an empty window answers.
    pub(super) fn focus_here(&self, listing: &Listing) -> Option<FocusSite> {
        listing.sessions.iter().find_map(|session| {
            session.tabs.iter().find_map(|tab| {
                tab.blocks
                    .iter()
                    .find(|block| block.focused)
                    .map(|block| FocusSite {
                        pane_id: block.id.clone(),
                        tab_id: tab.id.clone(),
                        session_id: session.id.clone(),
                    })
            })
        })
    }

    /// The running process inside one block, as `tern process` reports it.
    ///
    /// A separate call rather than a field on the listing: the listing says a
    /// block exists and whether its program has exited, and the pid and argv
    /// are what a reader wants about a live pane. A block tern cannot answer
    /// for — gone, or holding no process — yields `None`, and the probe still
    /// reports the block's own row.
    pub(super) fn process_of(&self, pane_id: &str) -> Option<Value> {
        self.json(vec!["process".into(), pane_id.into(), "--json".into()])
            .ok()
    }

    /// Whether this refusal means the block is gone, so closing it again is a
    /// no-op.
    pub(super) fn is_gone(&self, error: &anyhow::Error) -> bool {
        block_is_gone(error) || session_is_gone(error)
    }
}

/// Where focus currently sits.
pub(super) struct FocusSite {
    pub(super) pane_id: String,
    pub(super) tab_id: String,
    pub(super) session_id: String,
}

/// The session command as argv, with the session's environment set inside the
/// pane's own shell.
///
/// Tern launches `-- COMMAND...` in the block's shell, so the tokens are
/// prefixed as `env NAME=VALUE …` rather than as shell assignments: `env` is
/// a real executable, so a value carrying a space or a quote is not re-split
/// by a shell that never sees it. Nothing else from the client travels here —
/// `spawn`'s contract is the session's own environment, and the tools-mount
/// token and the rest of the client's variables belong to no pane this backend
/// opens.
pub(super) fn launch_argv(spec: &SpawnSpec) -> Vec<String> {
    let mut argv = vec!["env".to_string()];
    for (key, value) in &spec.env {
        argv.push(format!("{key}={value}"));
    }
    argv.extend(spec.command.iter().cloned());
    argv
}
