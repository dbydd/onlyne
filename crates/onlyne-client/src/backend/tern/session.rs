//! The session layer: the backend's own type and its [`SessionBackend`]
//! implementation.

use super::cli::default_command;
use super::policy::{Site, TernRef, session_label, split_word};
use crate::backend::*;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct TernBackend {
    pub(super) runner: Arc<dyn Runner>,
    pub(super) command: String,
    pub(super) env: BTreeMap<String, String>,
}

impl TernBackend {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self::with_env(runner, process_env())
    }

    pub fn with_env(runner: Arc<dyn Runner>, env: BTreeMap<String, String>) -> Self {
        let command = env
            .get("TERN_COMMAND")
            .cloned()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(default_command);
        Self {
            runner,
            command,
            env,
        }
    }
}

impl SessionBackend for TernBackend {
    fn name(&self) -> &'static str {
        "tern"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            spawn: true,
            attach: true,
            probe: true,
            close: true,
            focus: true,
            // `tern rename BLOCK NAME` renames the block's *tab*, measured: a
            // block id renames the tab that holds it, and a tab id is refused
            // with `no block is called`. Renaming one session's pane would
            // therefore rename every pane in that role's tab, so this backend
            // answers unsupported rather than do it.
            rename: false,
        }
    }

    fn available(&self) -> Result<bool> {
        // The only honest test: run the read every other call here begins with.
        // A window key in the environment proves the client is inside a pane,
        // not that the binary answers — and a `PATH` `tern` would fail this
        // where a spawned pane's `ls` would too.
        Ok(self.json(vec!["ls".into(), "--json".into()]).is_ok())
    }

    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        if spec.command.is_empty() {
            anyhow::bail!("tern spawn requires a command");
        }
        let label = session_label(&spec);
        let (session_id, tab_id, base_pane, direction, pane_id) = match self.role_site(&spec)? {
            // A launch already holds the agent: it is its own base, and the
            // direction keeps the word a first split beside it would take,
            // so the field reads the shape every ref carries. A found tab
            // needs the split, and its block count picks the default
            // direction as it did for herdr — a split bringing the count to
            // a power of two goes right, every other one down. Tern takes no
            // ratio, so the placement contributes its direction and nothing
            // else.
            Site::Launched {
                session_id,
                tab_id,
                ref pane_id,
            } => (
                session_id,
                tab_id,
                pane_id.clone(),
                split_word(PanePlacement::from_pane_count(0).direction),
                pane_id.clone(),
            ),
            Site::Found {
                session_id,
                tab_id,
                base_pane,
                pane_count,
            } => {
                // Tern takes no ratio, so the placement contributes its
                // direction and nothing else. The pane count still decides
                // the default direction, as it did for herdr: a split
                // bringing the count to a power of two goes right, every
                // other one down.
                let placement = spec
                    .placement
                    .unwrap_or_else(|| PanePlacement::from_pane_count(pane_count));
                tracing::info!(
                    pane_count,
                    direction = split_word(placement.direction),
                    "tern block split (tern takes no split ratio)"
                );
                let pane_id =
                    self.split_and_start(&base_pane, &spec, placement, &session_id, &tab_id)?;
                (
                    session_id,
                    tab_id,
                    base_pane,
                    split_word(placement.direction),
                    pane_id,
                )
            }
        };
        Ok(SessionRef {
            task_id: spec.task_id.clone(),
            backend: self.name().into(),
            backend_ref: TernRef {
                session_id,
                tab_id,
                base_pane,
                pane_id,
                session_label: label,
                split_direction: direction.to_string(),
            }
            .to_value(),
            generation: 1,
        })
    }

    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        let probe = self.probe(session)?;
        if !probe.alive {
            anyhow::bail!("tern block is gone");
        }
        Ok(session.clone())
    }

    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        let reference = TernRef::from_session(session)?;
        // A listing that cannot be read is a probe that could not answer, not
        // a verdict: the block's own state is unknown, so it reports the
        // failure rather than claiming the pane died.
        let listing = self.listing()?;
        let Some((block, tab, host)) = listing.block(&reference.pane_id) else {
            // The block is in no tab of any session: gone, or detached by the
            // daemon. Both read the same to this backend, which can address
            // neither.
            return Ok(ResourceProbe {
                alive: false,
                attached: false,
                detail: Some(serde_json::json!({
                    "pane_id": reference.pane_id,
                    "error": "block is in no tab",
                })),
            });
        };
        // Liveness from the block's own row: `exited` is null while the
        // program runs and holds an exit code once it returns, and `live` says
        // whether the daemon still holds the pty. A `--keep-open` block whose
        // command has ended stays listed with `exited` set — the pane is
        // addressable, and the session that owned it is over. So `exited`
        // ends a session and `live` is the fallback for a build that reports
        // one without the other.
        let alive = block.exited.is_none() && block.live;
        let mut detail = serde_json::json!({
            "pane": {
                "id": block.id,
                "title": block.title,
                "cwd": block.cwd,
                "program": block.program,
                "command": block.command,
                "exited": block.exited,
                "keep_open": block.keep_open,
                "focused": block.focused,
                "live": block.live,
            },
            "tab_id": tab.id,
            "session_id": host.id,
            "session_label": host.name,
        });
        if let Some(process) = self.process_of(&reference.pane_id) {
            if let Some(map) = detail.as_object_mut() {
                map.insert("process".into(), process);
            }
        }
        Ok(ResourceProbe {
            alive,
            // A block still listed is one the daemon holds, so a live one is
            // attached to a pty. A block whose program exited under
            // `--keep-open` is addressable but holds nothing: attached follows
            // `live`, which is what separates the two.
            attached: block.live,
            detail: Some(detail),
        })
    }

    fn close(&self, session: &SessionRef, reason: CloseReason, force: bool) -> Result<()> {
        // `tern close` has no force flag and no reason; both close paths are
        // the same command.
        tracing::debug!(task = %session.task_id, ?reason, force, "closing tern block");
        let reference = TernRef::from_session(session)?;
        match self.json(vec![
            "close".into(),
            reference.pane_id.clone(),
            "--json".into(),
        ]) {
            Ok(_) => Ok(()),
            // Measured: a closed block is refused with `no block is called
            // \`N\`` and exit 1, with no JSON body to carry a code. Closing
            // again is a no-op, which is what lets the client end a session
            // twice without the second end failing.
            Err(error) if self.is_gone(&error) => {
                tracing::debug!(
                    task = %session.task_id,
                    pane = %reference.pane_id,
                    "tern block already closed"
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn rename(&self, _session: &SessionRef, _title: &str) -> Result<()> {
        // `tern rename BLOCK NAME` renames the block's tab, so one session's
        // title would land on every pane in the role's tab. Tern offers no
        // block title of its own — the listing's `title` is the pane's
        // working directory or its program — so this is unsupported, and the
        // capability says so.
        Err(unsupported(
            self.name(),
            "rename",
            "tern renames a block's tab, so renaming one session's block would rename the \
             whole role tab",
        ))
    }

    fn focus(&self, session: &SessionRef) -> Result<()> {
        let reference = TernRef::from_session(session)?;
        let listing = self.listing()?;
        if let Some(site) = self.focus_here(&listing) {
            if site.pane_id == reference.pane_id
                && site.tab_id == reference.tab_id
                && site.session_id == reference.session_id
            {
                return Ok(());
            }
        }
        self.focus_block(&reference.pane_id)?;
        // Confirm the block took focus, naming the one that holds it. The
        // listing can drift when an operator closes or moves blocks, and a
        // focus that lands elsewhere sends the operator's keyboard to another
        // session — a wrong block holding focus is reported, not passed off as
        // success.
        let after = self.listing()?;
        match self.focus_here(&after) {
            Some(site) if site.pane_id == reference.pane_id => Ok(()),
            Some(site) => Err(anyhow::anyhow!(
                "tern left block {} unfocused (focused block {})",
                reference.pane_id,
                site.pane_id
            )),
            None => Err(anyhow::anyhow!(
                "tern left block {} unfocused (no block holds focus)",
                reference.pane_id
            )),
        }
    }
}
