//! One turn: the report the agent leaves, the way its ending is read, and the
//! thread that runs it.
//!
//! [`run_turn`] is the only reader of a session's ending, so a turn reports
//! exactly once. The prompt's completion directive hands the agent the path the
//! ending reads, so the two directions cannot drift, and a report may only lower
//! a turn's standing — never raise it.

use crate::backend::*;
use crate::content::ContentWriter;
use onlyne_acp::{ContentBlock, PromptOutcome};
use onlyne_layout::RoleWorkspace;
use onlyne_proto::payload::{Handoff, PayloadV2};
use std::path::Path;

use super::journal::{Journal, append, completion_head, drain};
use super::state::SessionEntry;

/// Refusals named in one fault record; the rest are counted, not listed.
const REFUSAL_LIST_LIMIT: usize = 3;

/// Map a stop reason onto what the ledger records: the outcome, the fault note
/// when there is one, and the reason word the turn record carries.
///
/// What the agent claims about its own ending is a separate input, and a
/// `hop-blocked:` report line is not a state this function reads: the verdict
/// word is the agent naming something outside the task it is waiting on, and
/// only the caller that folds the report in can lower a standing with it. A
/// write that fails because a path is a file where a directory belongs — a
/// blocked journal, a blocked report directory — is blocked in a third sense
/// again, and it costs a warning, never a verdict.
fn settle_for(stop_reason: &str, answer: Option<&str>) -> (TaskState, Option<String>, String) {
    let named = if stop_reason.is_empty() {
        "(absent)".to_string()
    } else {
        stop_reason.to_string()
    };
    match stop_reason {
        PromptOutcome::END_TURN => (TaskState::Done, None, named),
        PromptOutcome::CANCELLED => (
            TaskState::Cancelled,
            Some("the client cancelled this session".to_string()),
            named,
        ),
        PromptOutcome::REFUSAL | PromptOutcome::MAX_TOKENS | PromptOutcome::MAX_TURN_REQUESTS => (
            TaskState::Failed,
            Some(format!("agent stopped the turn: {named}")),
            named,
        ),
        // An absent or unknown `stopReason` is not a success this client can read,
        // and neither is a turn that left no answer behind.
        _ => (
            TaskState::Failed,
            Some(match answer {
                Some(_) => format!("agent ended the turn with stopReason {named:?}"),
                None => format!("agent ended the turn with stopReason {named:?} and no answer"),
            }),
            named,
        ),
    }
}

/// Where one turn leaves its payload-v2 result report, derived from the
/// session's workspace and the task id. Both directions call this — the
/// directive that hands the agent its path and the ending that reads it — so
/// they cannot drift, and the path is absolute because a shared agent process
/// may run with some other session's directory as its cwd.
pub(super) fn payload_dir(workdir: &Path) -> PathBuf {
    RoleWorkspace::resolve(workdir).out_dir()
}

pub(super) fn payload_path(workdir: &Path, task_id: &str) -> PathBuf {
    RoleWorkspace::resolve(workdir).report_path(task_id)
}

/// The block appended to every ACP prompt: where to report this task's
/// outcome, in what form, and the rule that settlement is ours. The agent is
/// not an onlyne client and is told so; the file is all it has to leave. The
/// grammar is printed from [`GRAMMAR_V2`](onlyne_proto::payload::GRAMMAR_V2),
/// the same text the CLI's `report check --help` shows, so an agent and an
/// operator reading the two never get two answers.
pub(super) fn completion_directive(workdir: &Path, task_id: &str) -> String {
    format!(
        "\n\nResult report (write before you stop): {}\n{}\n\
         Create it under a temporary name in the same directory and rename it \
         into place, so no reader ever sees a half-written report.\n\
         Keep the detail in project files; the verdict line may name them.\n\
         Where an `onlyne` command is on your PATH, `onlyne report check --path \
         <the path above>` names the line it cannot parse: use it before you \
         write the report and after, and fix what it refuses.\n\
         Do not run any `onlyne` command to settle the task: this client reads \
         the file and settles the task itself.",
        payload_path(workdir, task_id).display(),
        onlyne_proto::payload::GRAMMAR_V2,
    )
}

/// One turn's result report, as the ending found it. The grammar is the shared
/// one in [`onlyne_proto::payload`], and this holds only what that parser
/// cannot see: a turn whose report never arrived.
enum Payload {
    /// No file, or one this client could not open: the turn settles on its
    /// stop reason alone, exactly as it did before reports existed.
    Absent,
    /// What the parser made of a file that was there.
    Parsed(PayloadV2),
}

impl Payload {
    /// The word the turn's `payload` journal record carries under
    /// `payload_kind`; the record type `kind` is taken by the journal.
    fn payload_kind(&self) -> &'static str {
        match self {
            Payload::Absent => "absent",
            Payload::Parsed(PayloadV2::Done { .. }) => "done",
            Payload::Parsed(PayloadV2::Failed { .. }) => "failed",
            Payload::Parsed(PayloadV2::Blocked { .. }) => "blocked",
            Payload::Parsed(PayloadV2::Invalid { .. }) => "invalid",
        }
    }

    /// The verdict line's own text: the head of a done task, the reason of a
    /// failed or blocked one. `None` when there is no verdict to read.
    fn verdict(&self) -> Option<&str> {
        match self {
            Payload::Absent => None,
            Payload::Parsed(report) => report.verdict(),
        }
    }

    /// The handoff lines the report named, none of them routed yet.
    fn handoffs(&self) -> &[Handoff] {
        match self {
            Payload::Absent => &[],
            Payload::Parsed(report) => report.handoffs(),
        }
    }

    /// The reason a report was refused, `Some` for exactly the reports that
    /// stay on disk.
    fn error(&self) -> Option<&str> {
        match self {
            Payload::Parsed(PayloadV2::Invalid { error }) => Some(error.as_str()),
            _ => None,
        }
    }
}

/// Read this turn's report, write down what it asks to hand on, and take the
/// file away. Deleting after reading is the isolation a requeued task id needs:
/// the next turn starts with no report until an agent living through it writes
/// one. A file that is not a report is kept — it is the evidence for the
/// refusal this client recorded, and its author can still fix it in place.
fn read_payload(workdir: &Path, task_id: &str, journal: &Journal) -> (Payload, Vec<Handoff>) {
    let path = payload_path(workdir, task_id);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return (Payload::Absent, Vec::new()),
    };
    let payload = match String::from_utf8(bytes) {
        Err(_) => Payload::Parsed(PayloadV2::Invalid {
            error: "payload is not valid utf-8".to_string(),
        }),
        Ok(text) => Payload::Parsed(onlyne_proto::payload::parse(&text)),
    };
    let handoffs = emit_handoffs(journal, task_id, &payload);
    if payload.error().is_some() {
        return (payload, Vec::new());
    }
    if let Err(error) = std::fs::remove_file(&path) {
        tracing::warn!(error = %error, path = %path.display(), "acp: report file stayed behind");
    }
    (payload, handoffs)
}

/// Record this turn's handoff lines, and answer the ones worth routing.
///
/// The journal takes them while the report file still exists: the file is the
/// only place the agent put them down, and it is gone a moment later. A
/// `hop-blocked:` verdict hands nothing on — work that did not finish has
/// nothing to pass along — so its lines are recorded as skipped and stay in
/// this process. Routing belongs to the client, the one process that holds a
/// server link, and it carries the returned lines to that link.
fn emit_handoffs(journal: &Journal, task_id: &str, payload: &Payload) -> Vec<Handoff> {
    let head = payload.verdict().unwrap_or_default();
    let blocked = payload.payload_kind() == "blocked";
    let mut queued = Vec::new();
    for handoff in payload.handoffs() {
        journal.record(
            "handoff",
            vec![
                ("task_id", Value::from(task_id)),
                ("to_role", Value::from(handoff.to_role.clone())),
                ("text", Value::from(handoff.text_or(head).to_string())),
                (
                    "status",
                    Value::from(if blocked { "skipped_blocked" } else { "queued" }),
                ),
            ],
        );
        if !blocked {
            queued.push(handoff.clone());
        }
    }
    queued
}

/// Run one turn and report how it ended. The thread this runs on is the only
/// reader of this session's ending, so the report happens exactly once.
pub(super) fn run_turn(
    entry: Arc<SessionEntry>,
    sink: OutcomeSink,
    content: ContentWriter,
    task_id: String,
    prompt: String,
    warning: Option<String>,
    policy: &'static str,
) {
    let journal = Journal::new(&entry.workdir, &task_id, &entry.id, content);
    journal.record(
        "dispatch",
        vec![
            ("task_id", Value::from(task_id.clone())),
            ("prose", Value::from(prompt.clone())),
        ],
    );
    if let Some(detail) = &warning {
        journal.record(
            "warning",
            vec![
                ("task_id", Value::from(task_id.clone())),
                ("detail", Value::from(detail.clone())),
            ],
        );
    }
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
    let head = completion_head(&drained.message);
    // payload-v2: a report the agent left stands in for what the closing
    // message said, and may only lower the turn's standing. `hop-done`
    // replaces the head while the stop reason still decides the outcome, so a
    // report can never promote a turn the agent was cut short on. The handoff
    // lines are written into the journal here, before the file goes.
    let (payload, handoffs) = read_payload(&entry.workdir, &task_id, &journal);
    let payload_kind = payload.payload_kind();
    let payload_error = payload.error().map(str::to_string);
    let head_kind = match &payload {
        Payload::Absent => None,
        Payload::Parsed(report) => report.head_kind(),
    }
    .map(str::to_string);
    let head = match &payload {
        Payload::Parsed(PayloadV2::Done { head, .. }) => Some(head.clone()),
        _ => head,
    };
    let (settled, note, stop_reason) = match &turn {
        Ok(outcome) => settle_for(&outcome.stop_reason, head.as_deref()),
        Err(error) => (
            TaskState::Failed,
            Some(death_note(error, drained.exited.as_deref())),
            "(error)".to_string(),
        ),
    };
    // A report that is present but not a report cancels the completion: the
    // client will not guess a verdict out of a file it asked for in one shape,
    // and the reason travels in the fault note rather than a head. A turn whose
    // process died or was cut short already carries a harder fact than any
    // report, so the verdict changes and the detail is added to that note.
    let (settled, head, note) = match payload {
        Payload::Parsed(PayloadV2::Failed { reason, .. }) => {
            // The sender reads the agent's own line; where the turn was cut
            // short or its process died, that harder fact stays in the note.
            let note = match note {
                Some(found) => format!("{found}; the agent reported: {reason}"),
                None => reason.clone(),
            };
            (TaskState::Failed, Some(reason), Some(note))
        }
        Payload::Parsed(PayloadV2::Blocked { reason, .. }) => {
            // A task waiting on something outside itself did not finish, so it
            // cannot settle as done; `head_kind` says this was a block rather
            // than a break, and the handoff lines stayed in the journal.
            let detail = format!("the agent reported it is blocked: {reason}");
            let note = match note {
                Some(found) => format!("{found}; {detail}"),
                None => detail,
            };
            (TaskState::Failed, Some(reason), Some(note))
        }
        Payload::Parsed(PayloadV2::Invalid { error }) => {
            let detail = format!("acp payload invalid: {error}");
            let note = match note {
                Some(found) => format!("{found}; {detail}"),
                None => detail,
            };
            (TaskState::Cancelled, None, Some(note))
        }
        _ => (settled, head, note),
    };
    let mut payload_fields = vec![
        ("task_id", Value::from(task_id.clone())),
        (
            "path",
            Value::from(payload_path(&entry.workdir, &task_id).display().to_string()),
        ),
        ("payload_kind", Value::from(payload_kind)),
        ("head", head.clone().map(Value::from).unwrap_or(Value::Null)),
        ("handoffs", Value::from(handoffs.len())),
    ];
    // A refused report stays on disk, so the record that says so names the line
    // it could not read; an accepted one needs no such field.
    if let Some(refused) = &payload_error {
        payload_fields.push(("error", Value::from(refused.as_str())));
    }
    journal.record("payload", payload_fields);
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
        head_kind,
        note,
        refusals,
        handoffs,
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
