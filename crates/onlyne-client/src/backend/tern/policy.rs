//! Tern's own policy: what a session, a tab and a block are called, and how
//! each is found or created.

use super::cli::absolute_cwd;
use super::session::TernBackend;
use crate::backend::*;

/// Address of one Tern block, stored under `backend_ref.tern`.
///
/// Every id is Tern's own, kept as the decimal string its JSON spells it in:
/// Tern's ids are wide integers that grow with the daemon's block count, and a
/// block id is not interchangeable with a tab id even though both are numbers —
/// `tern rename <TAB_ID>` is refused with `no block is called` precisely
/// because a tab id names no block. So the three ids stay in three fields with
/// three spellings, and no call ever passes one where another belongs.
///
/// `base_pane` is the block that was split. `split_direction` is the word sent
/// to `tern split`: `right` or `down`. Tern takes no ratio, so the placement's
/// ratio is not recorded — there is nothing on the host to replay it into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TernRef {
    pub(super) session_id: String,
    pub(super) tab_id: String,
    pub(super) pane_id: String,
    pub(super) session_label: String,
    pub(super) base_pane: String,
    pub(super) split_direction: String,
}

impl TernRef {
    pub(super) fn from_session(session: &SessionRef) -> Result<Self> {
        let tern = session
            .backend_ref
            .get("tern")
            .ok_or_else(|| anyhow::anyhow!("tern session ref missing tern object"))?;
        let field = |key: &str| -> Result<String> {
            tern.get(key)
                .and_then(id_of)
                .ok_or_else(|| anyhow::anyhow!("tern session ref missing {key}"))
        };
        let optional = |key: &str| -> String {
            tern.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        Ok(Self {
            session_id: field("session_id")?,
            tab_id: field("tab_id")?,
            pane_id: field("pane_id")?,
            session_label: optional("session_label"),
            base_pane: optional("base_pane"),
            split_direction: optional("split_direction"),
        })
    }

    pub(super) fn to_value(&self) -> Value {
        serde_json::json!({
            "tern": {
                "session_id": self.session_id,
                "tab_id": self.tab_id,
                "pane_id": self.pane_id,
                "session_label": self.session_label,
                "base_pane": self.base_pane,
                "split_direction": self.split_direction,
            }
        })
    }
}

/// One of Tern's ids, in the string spelling a command line carries.
///
/// Tern prints its ids as JSON numbers and its own error messages quote them
/// (`no block is called \`999\``), so both are numbers on the way in and out.
/// A ref written by an older or hand-edited state file may hold either
/// spelling; a number is rendered the way Tern renders it, and a string is
/// kept as written, so a ref survives a round trip byte for byte.
pub(super) fn id_of(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => Some(number.to_string()),
        Value::String(text) => Some(text.clone()),
        _ => None,
    }
}

/// The id a create answer named, under any of the keys tern has spelled it.
///
/// `new session`, `new tab` and `split` all answer
/// `{"session":N,"tab":N,"block":N}`, but a build that answers a nested
/// `{"block":{"id":N}}` would be read the same way: the object form is
/// unwrapped to its own `id`. Anything else is a shape this backend does not
/// know how to address, and the caller reports it by name.
pub(super) fn created_id(value: &Value, key: &str) -> Option<String> {
    let raw = value.get(key)?;
    id_of(raw).or_else(|| raw.get("id").and_then(id_of))
}

/// The one cluster's Tern session: `onlyne:<cluster>`, defaulting to
/// `default` as herdr's did.
pub(super) fn session_label(spec: &SpawnSpec) -> String {
    let cluster = spec
        .env
        .get("ONLYNE_CLUSTER")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or("default");
    format!("onlyne:{cluster}")
}

/// The role's tab name, defaulting to `role` as herdr's did.
pub(super) fn role(spec: &SpawnSpec) -> String {
    spec.env
        .get("ONLYNE_ROLE")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or("role")
        .to_string()
}

/// The direction word `tern split` takes.
pub(super) fn split_word(direction: SplitDirection) -> &'static str {
    match direction {
        SplitDirection::Right => "right",
        SplitDirection::Down => "down",
    }
}

/// Every session in this window, as Tern's tree.
pub(super) struct Listing {
    /// `(session_id, session_name, tabs)`.
    pub(super) sessions: Vec<SessionRow>,
}

pub(super) struct SessionRow {
    pub(super) id: String,
    pub(super) name: Option<String>,
    pub(super) tabs: Vec<TabRow>,
}

pub(super) struct TabRow {
    pub(super) id: String,
    pub(super) name: Option<String>,
    pub(super) blocks: Vec<BlockRow>,
}

pub(super) struct BlockRow {
    pub(super) id: String,
    pub(super) title: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) program: Option<String>,
    pub(super) command: Option<String>,
    /// The block's exit status, once its program has returned.
    ///
    /// `None` both while the program runs and for a build that omits the
    /// field: tern writes `"exited": null` for a live block, so a JSON null
    /// means *not exited* and must not be carried as a value. A number here
    /// is a status; `live` is the fallback for a block whose program ended
    /// without one.
    pub(super) exited: Option<Value>,
    pub(super) keep_open: bool,
    pub(super) focused: bool,
    pub(super) live: bool,
}

/// Decode `ls --json` into the tree this backend addresses.
///
/// Tern's ids are numbers, and a row missing the id that names it cannot be
/// addressed, so such a row is dropped rather than stored with an empty id
/// that would collide with every other empty id. Every other field has a
/// defined reading for its absence: a nameless tab (`name` is null until
/// renamed), a block that has exited (`exited` is null while it runs).
fn text(row: &Value, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(str::to_string)
}

pub(super) fn decode_listing(value: &Value) -> Listing {
    let sessions = value
        .get("sessions")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let id = row.get("id").and_then(id_of)?;
                    let tabs = row
                        .get("tabs")
                        .and_then(Value::as_array)
                        .map(|tabs| tabs.iter().filter_map(decode_tab).collect())
                        .unwrap_or_default();
                    Some(SessionRow {
                        id,
                        name: text(row, "name"),
                        tabs,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // `detached` — blocks the daemon holds with no tab — is read here and
    // dropped: a block in it can be focused and closed by id, but it is never
    // a base to split, and `block` finds a pane only where a tab holds it,
    // which is the reading this backend addresses by.
    Listing { sessions }
}

fn decode_tab(row: &Value) -> Option<TabRow> {
    let id = row.get("id").and_then(id_of)?;
    let blocks = row
        .get("blocks")
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter_map(decode_block).collect())
        .unwrap_or_default();
    Some(TabRow {
        id,
        name: text(row, "name"),
        blocks,
    })
}

fn decode_block(row: &Value) -> Option<BlockRow> {
    Some(BlockRow {
        id: row.get("id").and_then(id_of)?,
        title: text(row, "title"),
        cwd: text(row, "cwd"),
        program: text(row, "program"),
        command: text(row, "command"),
        exited: row.get("exited").filter(|value| !value.is_null()).cloned(),
        keep_open: row
            .get("keep_open")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        focused: row.get("focused").and_then(Value::as_bool).unwrap_or(false),
        live: row.get("live").and_then(Value::as_bool).unwrap_or(false),
    })
}

impl TernBackend {
    /// The cluster's Tern session, found by name or created.
    pub(super) fn find_or_create_session(&self, spec: &SpawnSpec) -> Result<String> {
        let label = session_label(spec);
        let listing = self.listing()?;
        if let Some(session) = listing
            .sessions
            .iter()
            .find(|session| session.name.as_deref() == Some(label.as_str()))
        {
            return Ok(session.id.clone());
        }
        let created = self.json(vec![
            "new".into(),
            "session".into(),
            label.clone(),
            "--cwd".into(),
            absolute_cwd(&spec.cwd),
            "--keep-open".into(),
            "--json".into(),
        ])?;
        let session_id = created_id(&created, "session")
            .ok_or_else(|| anyhow::anyhow!("tern new session returned no session id"))?;
        // The lookup keys on the name alone, so a session holding this
        // cluster's blocks under another name reads as absent here and gains a
        // named sibling. The warning names both, and the rename that makes the
        // next spawn find the operator's session.
        tracing::warn!(
            label = label.as_str(),
            session_id = session_id.as_str(),
            "tern created a session for {label}; rename it with \
             `tern rename {session_id} {label}` before spawning again"
        );
        Ok(session_id)
    }

    /// The role's tab inside `session_id`, found by name or created, with the
    /// block to split beside and the tab's block count.
    ///
    /// A found tab whose blocks all failed to decode reports no block to
    /// split, which the caller turns into an error naming the tab — a role tab
    /// that cannot be split is not something to paper over with a new tab.
    pub(super) fn find_or_create_tab(&self, session_id: &str, spec: &SpawnSpec) -> Result<TabSlot> {
        let role = role(spec);
        let listing = self.listing()?;
        if let Some(session) = listing
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        {
            if let Some(tab) = session
                .tabs
                .iter()
                .find(|tab| tab.name.as_deref() == Some(role.as_str()))
            {
                let base = base_block(tab)?;
                return Ok(TabSlot {
                    tab_id: tab.id.clone(),
                    base_pane: base,
                    pane_count: tab.blocks.len(),
                });
            }
        }
        let created = self.json(vec![
            "new".into(),
            "tab".into(),
            session_id.to_string(),
            "--cwd".into(),
            absolute_cwd(&spec.cwd),
            "--keep-open".into(),
            "--json".into(),
        ])?;
        let tab_id = created_id(&created, "tab")
            .ok_or_else(|| anyhow::anyhow!("tern new tab returned no tab id"))?;
        // A new tab holds exactly one block, and `new tab` names it. A build
        // that answered without one would leave nothing to split, so the call
        // is verified against the listing rather than trusted.
        let base = match created_id(&created, "block") {
            Some(block) => block,
            None => self
                .listing()?
                .tab(&tab_id)?
                .blocks
                .first()
                .map(|block| block.id.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!("tern new tab {tab_id} made a tab with no block to split")
                })?,
        };
        // Tern's `new tab` has no name argument. `rename BLOCK NAME` is the
        // host's tab-rename operation, so apply it immediately to the tab's
        // first block; later session blocks remain in the role-named tab.
        self.rename_tab(&base, &role)?;
        Ok(TabSlot {
            tab_id,
            base_pane: base,
            pane_count: 1,
        })
    }

    pub(super) fn listing(&self) -> Result<Listing> {
        Ok(decode_listing(
            &self.json(vec!["ls".into(), "--json".into()])?,
        ))
    }

    /// Rename the tab containing `block`. Tern's public rename operation is
    /// deliberately not exposed as per-session rename: it renames the whole
    /// tab, which is safe here only while creating a new role tab.
    fn rename_tab(&self, block_id: &str, role: &str) -> Result<()> {
        self.json(vec![
            "rename".into(),
            block_id.into(),
            role.into(),
            "--json".into(),
        ])
        .map(|_| ())
    }
}

/// What `find_or_create_tab` resolves: the tab, the block to split, and how
/// many blocks it holds.
pub(super) struct TabSlot {
    pub(super) tab_id: String,
    pub(super) base_pane: String,
    pub(super) pane_count: usize,
}

/// The block a new pane is split beside: the tab's focused one, else its
/// first.
///
/// Tern focuses a block per tab, and a tab that was never clicked holds one
/// focused block among many. Splitting beside the focused block is the same
/// choice herdr made, and it is what keeps a role's panes from accumulating
/// along its left edge.
fn base_block(tab: &TabRow) -> Result<String> {
    tab.blocks
        .iter()
        .find(|block| block.focused)
        .or_else(|| tab.blocks.first())
        .map(|block| block.id.clone())
        .ok_or_else(|| anyhow::anyhow!("tern tab {} has no block to split", tab.id))
}

impl Listing {
    pub(super) fn tab(&self, tab_id: &str) -> Result<&TabRow> {
        self.sessions
            .iter()
            .flat_map(|session| session.tabs.iter())
            .find(|tab| tab.id == tab_id)
            .ok_or_else(|| anyhow::anyhow!("tern tab {tab_id} is in no session"))
    }

    /// One block's row, anywhere in the tree, with the tab and session that
    /// hold it.
    pub(super) fn block(&self, pane_id: &str) -> Option<(&BlockRow, &TabRow, &SessionRow)> {
        self.sessions.iter().find_map(|session| {
            session.tabs.iter().find_map(|tab| {
                tab.blocks
                    .iter()
                    .find(|block| block.id == pane_id)
                    .map(|block| (block, tab, session))
            })
        })
    }
}
