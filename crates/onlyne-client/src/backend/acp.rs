//! ACP session backend: run the role's agent as an Agent Client Protocol peer of
//! this client, instead of as a program inside a terminal.
//!
//! The other backends start a session command in a place a human can read —
//! a zellij pane, an Orca tab, an `exec` child whose stdout is a
//! log file — and a mounted plugin inside that place reports the session's state
//! back. An ACP agent has no place and mounts nothing: it is a process that
//! speaks a turn protocol on a pipe, so this backend owns both halves the plugin
//! would otherwise supply. It carries the delivery itself
//! ([`SessionBackend::deliver`]) and it reports the ending itself
//! ([`SessionBackend::outcomes`]). The ending is the agent's own: the head is
//! the closing message it streamed, and its stop reason decides the standing.
//! The agent is a self-driving role with no plugin of ours inside it, so the
//! conversation this client journaled is the whole of what it can say about how
//! it ended.
//!
//! Process discipline, which is what makes it different from a loop that just
//! calls [`onlyne_acp::Agent::prompt`]:
//!
//! * **one agent process per distinct rendered command, shared by every session
//!   of the role.** A role whose `session_command` renders the same argv for each
//!   task runs one agent and opens one ACP session per task on it. A command that
//!   interpolates `{task}` renders to a different key per session and gets a
//!   process of its own: legal, and the reason the key is the command and not the
//!   role. A shared process keeps the environment it was started with, so a second
//!   session rides the *first* session's `ONLYNE_*` identity variables — ACP has
//!   no per-session exec environment, so an agent that needs its own identity per
//!   task should render `{task}` into its command and take a process of its own.
//! * **a turn runs on its own thread.** [`onlyne_acp::Agent::prompt`] parks its
//!   caller until the agent ends the turn, which can be minutes;
//!   [`SessionBackend::deliver`] answers at once and the thread pushes a
//!   [`SessionOutcome`] when the turn ends. Settling from the delivery path would
//!   leave the role unable to serve a second session meanwhile.
//! * **permission asks are answered on a policy, never by a fallback grant.** One
//!   responder thread per process listens for `session/request_permission`:
//!   `reject_once` by default, `allow_once` for a client told to allow, and never
//!   `allow_always` — a blanket grant is an operator's decision, expressed through
//!   the session `mode`, not a client default.
//! * **the conversation is written down.** An ACP session owns no terminal, so
//!   `<workspace>/.onlyne/logs/session-<task>.log` (rendered, for `tail -f`) and
//!   `session-<task>.events.jsonl` (raw updates, plus this client's own
//!   `dispatch` and `turn` records) are the whole human-visible surface. Both
//!   are best effort: a write that fails is a warning, never a failed turn.
//! * **closing does not wait on the dispatch lock.** A close asks the agent to
//!   stop, waits a short bounded moment for the turn, and hands a turn that is
//!   still running to a detached thread rather than parking its caller.

// The module documentation names these; nothing here compiles against them, so
// they come into scope only while rustdoc is reading the links.
#[cfg(doc)]
use super::*;

mod journal;
mod mcp;
mod process;
mod session;
mod state;
mod turn;

pub use state::{AcpBackend, AcpOptions};
