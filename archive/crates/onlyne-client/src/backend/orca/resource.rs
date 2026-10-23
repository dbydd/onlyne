//! The resource layer: pane handles and refs, re-resolved against the rows
//! `terminal show` and `terminal list` answer with.

use super::cli::is_stale;
use super::session::OrcaBackend;
use crate::backend::*;

/// Read one non-empty string field from a session's `backend_ref`.
pub(super) fn ref_str(session: &SessionRef, key: &str) -> Option<String> {
    session
        .backend_ref
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// The stable per-tab keys Orca repeats in `terminal create` responses and
/// `terminal list` rows. `pane_key` is `tabId:leafId`: it survives a runtime
/// restart, while the `term_<uuid>` handle is minted per PTY incarnation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TabKeys {
    pub(super) handle: Option<String>,
    pub(super) pane_key: Option<String>,
    tab_id: Option<String>,
    leaf_id: Option<String>,
    worktree_id: Option<String>,
    pty_id: Option<String>,
}

impl TabKeys {
    /// Read one CLI row. `create`/`show` nest the row under `terminal`,
    /// `list` under `terminals`, so every accepted position is tried; a key
    /// the build does not send stays `None`.
    pub(super) fn read(row: &Value) -> Self {
        let text = |key: &str| {
            ["", "/terminal", "/result/terminal"]
                .iter()
                .find_map(|prefix| row.pointer(&format!("{prefix}/{key}")))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let tab_id = text("tabId");
        let leaf_id = text("leafId");
        let pane_key = text("paneKey").or_else(|| match (&tab_id, &leaf_id) {
            (Some(tab), Some(leaf)) => Some(format!("{tab}:{leaf}")),
            _ => None,
        });
        Self {
            handle: text("handle"),
            pane_key,
            tab_id,
            leaf_id,
            worktree_id: text("worktreeId"),
            pty_id: text("ptyId"),
        }
    }

    /// Read what a [`SessionRef`] already persisted.
    pub(super) fn from_ref(session: &SessionRef) -> Self {
        Self {
            handle: ref_str(session, "handle"),
            pane_key: ref_str(session, "pane_key"),
            tab_id: ref_str(session, "tab_id"),
            leaf_id: ref_str(session, "leaf_id"),
            worktree_id: ref_str(session, "worktree_id"),
            pty_id: ref_str(session, "pty_id"),
        }
    }

    /// The keys worth persisting. Absent keys are omitted rather than null,
    /// so a ref written before one of them existed still reads.
    pub(super) fn to_ref(&self) -> Value {
        let mut map = serde_json::Map::new();
        for (key, value) in [
            ("handle", &self.handle),
            ("pane_key", &self.pane_key),
            ("tab_id", &self.tab_id),
            ("leaf_id", &self.leaf_id),
            ("worktree_id", &self.worktree_id),
            ("pty_id", &self.pty_id),
        ] {
            if let Some(value) = value {
                map.insert(key.to_string(), Value::String(value.clone()));
            }
        }
        Value::Object(map)
    }
}

impl OrcaBackend {
    pub(super) fn ref_handle(session: &SessionRef) -> Result<String> {
        ref_str(session, "handle")
            .ok_or_else(|| anyhow::anyhow!("orca session ref missing string handle"))
    }

    /// Re-resolve a session's handle from its stable `pane_key`.
    ///
    /// Orca's CLI has no paneKey→handle command, so the stable key round-trips
    /// through `terminal list`: rows carry the pane key Orca persisted and the
    /// handle of the pane's current PTY. Every other field of the ref is kept,
    /// including the selector, and the observed keys are merged in.
    fn remint(&self, session: &SessionRef) -> Result<SessionRef> {
        let pane_key = ref_str(session, "pane_key").ok_or_else(|| {
            anyhow::anyhow!("orca session ref has no pane_key; the handle cannot be re-resolved")
        })?;
        let selector = ref_str(session, "selector");
        let row = self
            .rows(selector.as_deref())?
            .into_iter()
            .find(|row| TabKeys::read(row).pane_key.as_deref() == Some(pane_key.as_str()))
            .ok_or_else(|| {
                CommandFailure::new(
                    format!("orca terminal list (pane {pane_key})"),
                    1,
                    Some("terminal_not_found".into()),
                    Some(format!("pane {pane_key} is not listed in any worktree")),
                )
            })?;
        let keys = TabKeys::read(&row);
        if keys.handle.is_none() {
            anyhow::bail!("orca terminal list row for pane {pane_key} has no handle");
        }
        let mut backend_ref = session.backend_ref.clone();
        if let (Some(map), Value::Object(observed)) = (backend_ref.as_object_mut(), keys.to_ref()) {
            map.extend(observed);
        }
        Ok(SessionRef {
            backend_ref,
            ..session.clone()
        })
    }

    /// Where the tab a `terminal create` just made is, when its response named
    /// no handle.
    ///
    /// The coordinates the response did carry are an exact hook: `terminal list`
    /// repeats `paneKey`, or the `tabId:leafId` pair it is built from, beside the
    /// handle of the pane's current PTY. A response with no coordinates leaves
    /// the create-time title, which Orca's login shell replaces within seconds,
    /// so the newest row carrying that exact title wins. Position alone never
    /// picks a tab: closing one this backend cannot identify is worse than
    /// leaving it for the operator.
    fn find_created(&self, keys: &TabKeys, title: &str) -> Option<String> {
        let rows = self.rows(self.selector_for().as_deref()).ok()?;
        if let Some(wanted) = keys.pane_key.as_deref() {
            let named = rows
                .iter()
                .find(|row| TabKeys::read(row).pane_key.as_deref() == Some(wanted));
            if let Some(row) = named {
                return TabKeys::read(row).handle;
            }
        }
        rows.iter()
            .filter(|row| row.get("title").and_then(Value::as_str) == Some(title))
            .max_by_key(|row| row.get("lastOutputAt").and_then(Value::as_i64).unwrap_or(0))
            .and_then(|row| TabKeys::read(row).handle)
    }

    /// The error for a `terminal create` whose response named no handle, with
    /// the tab it made rolled back first.
    ///
    /// The tab exists by the time this runs, so it is the one way a failed spawn
    /// can leak a resource: the handle is recovered from the coordinates the
    /// response did carry and the tab is closed. A tab that cannot be located is
    /// named by those coordinates in the error instead, so an operator can
    /// finish the job, and no other tab is touched.
    pub(super) fn roll_back_create(
        &self,
        keys: &TabKeys,
        title: &str,
        value: &Value,
    ) -> anyhow::Error {
        let coordinates = format!(
            "title {title:?}, tab_id {:?}, leaf_id {:?}, pane_key {:?}",
            keys.tab_id, keys.leaf_id, keys.pane_key
        );
        match self.find_created(keys, title) {
            Some(handle) => match self.close_handle(&handle) {
                Ok(()) => anyhow::anyhow!(
                    "orca terminal create returned no handle; the tab it made ({coordinates}, \
                     handle {handle}) was closed: {value}"
                ),
                Err(error) => anyhow::anyhow!(
                    "orca terminal create returned no handle and the tab it made ({coordinates}, \
                     handle {handle}) was not closed: {error}; close it by hand. create \
                     answered: {value}"
                ),
            },
            None => anyhow::anyhow!(
                "orca terminal create returned no handle and the tab it made ({coordinates}) \
                 could not be located to close; close it by hand. create answered: {value}"
            ),
        }
    }

    /// The freshest `terminal show` payload for a session, re-resolving the
    /// stored handle once when Orca answers that it went stale.
    ///
    /// A stale handle is expected after an Orca runtime restart: the pane keeps
    /// its `pane_key` while the PTY incarnation mints a new `term_<uuid>`. The
    /// returned [`SessionRef`] carries the handle the payload belongs to.
    pub(super) fn current(&self, session: &SessionRef) -> Result<(SessionRef, Value)> {
        if let Some(handle) = ref_str(session, "handle") {
            match self.show(&handle) {
                Ok(value) => return Ok((session.clone(), value)),
                Err(error) if !is_stale(&error) => return Err(error),
                Err(_) => {}
            }
        }
        let refreshed = self.remint(session)?;
        let handle = ref_str(&refreshed, "handle")
            .ok_or_else(|| anyhow::anyhow!("orca remint produced no handle"))?;
        let value = self.show(&handle)?;
        self.note(&refreshed, "spawned");
        Ok((refreshed, value))
    }
}
