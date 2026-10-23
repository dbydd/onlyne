//! One turn: the way its ending is read, and the thread that runs it.
//!
//! [`run_turn`] is the only reader of a session's ending, so a turn reports
//! exactly once. The ending is the agent's own: the closing message it streamed
//! becomes the head, and its stop reason decides the standing.

use crate::backend::*;
use crate::content::ContentWriter;
use onlyne_acp::{ContentBlock, PromptOutcome};

use super::journal::{Journal, append, completion_head, drain};
use super::state::SessionEntry;

/// Refusals named in one fault record; the rest are counted, not listed.
const REFUSAL_LIST_LIMIT: usize = 3;

/// Map a stop reason onto what the ledger records: the standing, the fault note
/// when there is one, and the reason word the turn record carries.
///
/// `None` is the ordinary ending. An agent that stopped asking for work reported
/// a fact about the pipe, not a verdict on the task: whether the session
/// completed its task is a fact of its own tools connection, and this backend
/// cannot see that door. So it hands the ending on and the client's turn-end
/// rule reads it (`docs/v2-CONTRACT.md` §3c). A cancelled, refused, or absent
/// stop reason is a standing this client *did* witness for itself, and it
/// settles the delivery where it lands; `answer` is the closing head the turn
/// left, which an unknown reason needs to tell a finished-but-unreadable turn
/// from one that said nothing at all. A journal write that fails — a blocked log
/// file — costs a warning, never a verdict.
fn settle_for(
    stop_reason: &str,
    answer: Option<&str>,
) -> (Option<TaskState>, Option<String>, String) {
    let named = if stop_reason.is_empty() {
        "(absent)".to_string()
    } else {
        stop_reason.to_string()
    };
    match stop_reason {
        PromptOutcome::END_TURN => (None, None, named),
        PromptOutcome::CANCELLED => (
            Some(TaskState::Cancelled),
            Some("the client cancelled this session".to_string()),
            named,
        ),
        PromptOutcome::REFUSAL | PromptOutcome::MAX_TOKENS | PromptOutcome::MAX_TURN_REQUESTS => (
            Some(TaskState::Failed),
            Some(format!("agent stopped the turn: {named}")),
            named,
        ),
        // An absent or unknown `stopReason` is not a success this client can read,
        // and neither is a turn that left no answer behind.
        _ => (
            Some(TaskState::Failed),
            Some(match answer {
                Some(_) => format!("agent ended the turn with stopReason {named:?}"),
                None => format!("agent ended the turn with stopReason {named:?} and no answer"),
            }),
            named,
        ),
    }
}

/// Run one turn and report how it ended. The thread this runs on is the only
/// reader of this session's ending, so the report happens exactly once.
pub(super) fn run_turn(
    entry: Arc<SessionEntry>,
    sink: OutcomeSink,
    content: ContentWriter,
    prompt: String,
    record: &'static str,
    policy: &'static str,
) {
    // The task this turn serves is the one the session was opened for, read from
    // the binding rather than handed in: `deliver` refuses any other id before it
    // claims the turn, so the journal and the outcome that leaves here cannot be
    // two different tasks.
    let task_id = entry.current_task();
    let journal = Journal::new(&entry.workdir, &task_id, &entry.id, content);
    journal.record(
        record,
        vec![
            ("task_id", Value::from(task_id.clone())),
            ("prompt", Value::from(prompt.clone())),
        ],
    );
    let events = entry.agent.subscribe();
    let turn = entry
        .agent
        .prompt(&entry.id, vec![ContentBlock::text(&prompt)]);
    // The drain is exact only because every routed update is already queued when
    // the parked prompt wakes, and the agent's single reader thread is what puts
    // it there: see [`drain`].
    let drained = drain(&events, &entry.id);
    let refusals = take_refusals(&entry, policy);
    for record in drained.lines {
        journal.raw(record);
    }
    if !drained.log.is_empty() {
        append(&journal.log, &drained.log);
    }
    // The agent's closing words are this turn's answer and its stop reason is
    // the standing: nothing read beside the stream can raise or lower either.
    let head = completion_head(&drained.message);
    let (settled, note, stop_reason) = match &turn {
        Ok(outcome) => settle_for(&outcome.stop_reason, head.as_deref()),
        Err(error) => (
            Some(TaskState::Failed),
            Some(death_note(error, drained.exited.as_deref())),
            "(error)".to_string(),
        ),
    };
    journal.record(
        "turn",
        vec![
            ("task_id", Value::from(task_id.clone())),
            ("stop_reason", Value::from(stop_reason)),
            ("head", head.clone().map(Value::from).unwrap_or(Value::Null)),
        ],
    );
    // The turn is released before the report is handed over: a session that
    // reports while still marked busy would refuse the next request against it.
    entry.turn.finish();
    sink.push(SessionOutcome {
        task_id,
        outcome: settled,
        head,
        note,
        refusals,
    });
}

/// Summarise this turn's refusals for the fault record, and reset the accumulator
/// for the next turn of the same session.
fn take_refusals(entry: &SessionEntry, policy: &str) -> Option<String> {
    let mut refused = std::mem::take(&mut *entry.refusals.lock());
    if refused.is_empty() {
        return None;
    }
    let count = refused.len();
    refused.truncate(REFUSAL_LIST_LIMIT);
    Some(format!(
        "{count} permission ask(s) refused (policy={policy}): {}",
        refused.join("; ")
    ))
}

/// The note for a turn that never got its answer. The parked request's own message
/// already names the exit status and the tail of stderr when the process died; the
/// exit event adds what that message could not.
fn death_note(error: &anyhow::Error, exited: Option<&str>) -> String {
    let detail = error.to_string();
    match exited {
        Some(exited) if !detail.contains(exited) => format!("{detail}; {exited}"),
        _ => detail,
    }
}
