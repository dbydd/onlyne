//! Where a session's process runs, as that process reported it.
//!
//! A session is a process, and a process runs somewhere the supervisor wants to
//! look at: for a pi session that is the Orca pane its plugin was spawned in,
//! which the plugin inherits as `ORCA_PANE_KEY` (beside `ORCA_TAB_ID`,
//! `ORCA_LEAF_ID` and `ORCA_TERMINAL_HANDLE`) and reports back over the adapter
//! protocol. The reducer never reads this: `project` derives the public
//! lifecycle from the state dimensions alone, and `is_legal` constrains none of
//! it. It travels with the observation because the observation is what the
//! client mirrors and what every reader of a session row sees — the supervisor
//! board (`integrations/orca-plugin`) needs the pane on the *session* axis, and
//! the session axis is the observation.
//!
//! Absent fields are the honest answer and never an error: a plugin outside a
//! pane, or an Orca build that exports only some of the ids, reports what it
//! has. A reader filters on what is there rather than on a placeholder.

use serde::{Deserialize, Serialize};

/// The host kinds a session process can name. One variant today; the type is a
/// wrapper rather than a bare pane so a second substrate adds a sibling field
/// without renaming what is already on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct HostRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orca: Option<OrcaPane>,
}

/// The Orca pane a session process lives in, as the process itself reported it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OrcaPane {
    /// `<tab_id>:<leaf_id>`, the spelling Orca's own tooling addresses a pane by
    /// (`orca terminal list` rows carry the same pair).
    pub pane_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_id: Option<String>,
    /// The terminal handle `orca terminal switch` takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}
