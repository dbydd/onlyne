use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use onlyne_proto::{ClientOp, Envelope, ErrorCode, MsgKind, Receipt, Report, ResBody, new_op_id};
use onlyne_store::{ClientStore, IntentRow};
use serde_json::Value;
use std::time::Duration;

/// Refusals that end an intent.
///
/// `Invalid` stays out: the server answers it for several conditions a retry
/// clears, and exhaustion keeps the row visible as a fault, so a transient
/// refusal costs retries rather than the queued message (plan §6 line 289).
pub const PERMANENT_ERRORS: &[ErrorCode] = &[
    ErrorCode::AclDenied,
    ErrorCode::Conflict,
    ErrorCode::Forbidden,
    ErrorCode::UnknownRole,
    ErrorCode::NotAdmin,
    ErrorCode::BadFrame,
    ErrorCode::FrameTooLarge,
    ErrorCode::ProtocolVersion,
];

/// Passes a row this process cannot act on may be skipped before it retires.
///
/// The role's own ceiling (`attempts`) cannot bound this: it counts the answers
/// the server gave, and a row whose stored payload no longer decodes never
/// reaches the server to be answered. Without a bound of its own such a row sits
/// at its deadline, first in the flush batch, taking a slot on every pass for
/// the life of the queue (round-2 audit C1).
pub const MAX_LOCAL_FAILURES: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    Pending,
    Retrying,
    Accepted,
    Exhausted,
}

impl IntentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Retrying => "retrying",
            Self::Accepted => "accepted",
            Self::Exhausted => "exhausted",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum IntentResult {
    Accepted(Option<Receipt>),
    Retryable(ErrorCode, String),
    /// A refusal in [`PERMANENT_ERRORS`] ended the row: its `intent_dropped`
    /// fault is recorded before the intent is deleted.
    Dropped(ErrorCode, String),
    Exhausted,
    /// The frame reached a connection whose routed `hello` has not landed yet.
    ///
    /// The net layer redials behind the runloop's back, so a connection reads
    /// `Ready` while it still carries no role binding, and the server refuses
    /// every frame that arrives in that window. The refusal is a fact about the
    /// connection, not about the row: charging it a backoff rung takes the row
    /// out of the due window for a whole delay, and the flush the runloop runs
    /// behind the replayed `hello` then finds nothing to send — which is how a
    /// reconnecting client's own state stayed off the server's mirror for a
    /// second (e2e case 15). The row keeps its place in the queue instead.
    NotAuthenticated,
}

/// Give one outbound envelope the `op_id` its intent row is keyed by.
///
/// The proto requires the key for every kind but `Note` (`Envelope::validate`
/// in `onlyne-proto`), so a plugin's note legitimately arrives without one
/// while the queue still keys every row by an id. Minting it here keeps the
/// stamped envelope and the row's key one value: what the row replays is what
/// it stored. A non-note envelope keeps the key it brought, which is what
/// makes a re-delivered task dedup on its original id.
pub fn stamp_op_id(envelope: &mut Envelope) -> String {
    if let Some(op_id) = &envelope.op_id {
        return op_id.clone();
    }
    let op_id = new_op_id();
    envelope.op_id = Some(op_id.clone());
    op_id
}

#[derive(Clone)]
pub struct IntentMachine {
    pub store: ClientStore,
    pub attempts: u32,
    pub backoff_ms: Vec<u64>,
}

impl IntentMachine {
    pub fn new(store: ClientStore, attempts: u32, backoff_ms: Vec<u64>) -> Self {
        Self {
            store,
            attempts,
            backoff_ms,
        }
    }

    /// Queue one envelope under the id its row is keyed by.
    ///
    /// A kind that may arrive without a key (a note) gets a fresh one from
    /// [`stamp_op_id`], and the stamped envelope is what the row stores and
    /// replays, so the row's `op_id` and its `env_json` never disagree.
    pub fn enqueue(&self, envelope: &Envelope) -> Result<bool> {
        let mut stamped = envelope.clone();
        let op_id = stamp_op_id(&mut stamped);
        Ok(self
            .store
            .enqueue_intent(&op_id, &serde_json::to_value(&stamped)?)?)
    }

    pub fn enqueue_value(&self, op_id: &str, envelope: &Value) -> Result<bool> {
        Ok(self.store.enqueue_intent(op_id, envelope)?)
    }

    /// The rows this pass may send: due, oldest deadline first, one batch.
    ///
    /// Not the whole queue — [`ClientStore::flush_order`] bounds what is due and
    /// how many rows one pass takes, and a row still inside its backoff is asked
    /// for again on the pass its deadline arrives.
    pub fn pending(&self) -> Result<Vec<IntentRow>> {
        Ok(self.store.flush_order()?)
    }

    pub fn next_delay(&self, attempt: u32) -> Duration {
        let idx = attempt.saturating_sub(1) as usize;
        Duration::from_millis(
            self.backoff_ms
                .get(idx)
                .copied()
                .or_else(|| self.backoff_ms.last().copied())
                .unwrap_or(1_000),
        )
    }

    /// Count one pass in which this process could not act on a row at all.
    ///
    /// A payload that no longer decodes will not decode on a later pass, and a
    /// row left at its old deadline holds the head of the flush batch forever:
    /// every pass spends a slot on a frame that can never be built, and behind a
    /// batch cap that slot is one the queue's oldest sendable row did not get.
    /// Charging the pass retires the row at [`MAX_LOCAL_FAILURES`] the way a
    /// refused row retires at `attempts` — `exhausted`, with the fault that names
    /// the reason — and the backoff meanwhile takes it out of the due window, so
    /// the head moves on the same pass.
    ///
    /// A transport failure is not this path: the link being down says nothing
    /// about the row, so [`IntentMachine::defer`] keeps its budget intact and the
    /// reconnect flushes it (plan §6 line 289).
    pub fn fail_local(&self, row: &IntentRow, reason: &str) -> Result<IntentResult> {
        let failures = row.attempt.max(0) as u32 + 1;
        if failures >= MAX_LOCAL_FAILURES {
            self.store.exhaust_intent(&row.op_id, reason)?;
            self.record_intent_fault(&row.env_json, "intent_exhausted", reason)?;
            return Ok(IntentResult::Exhausted);
        }
        let due = Utc::now() + self.next_delay(failures);
        self.store.bump_intent(&row.op_id, due, reason)?;
        Ok(IntentResult::Retryable(
            ErrorCode::Internal,
            reason.to_string(),
        ))
    }

    pub fn attempt(&self, row: &IntentRow, response: Option<&ResBody>) -> Result<IntentResult> {
        let op_id = row.op_id.as_str();
        let Some(body) = response else {
            let next = row.attempt.saturating_add(1) as u32;
            if next >= self.attempts {
                self.store
                    .exhaust_intent(op_id, "intent attempts exhausted")?;
                self.record_intent_fault(
                    &row.env_json,
                    "intent_exhausted",
                    "intent attempts exhausted",
                )?;
                return Ok(IntentResult::Exhausted);
            }
            let due = Utc::now() + self.next_delay(next);
            self.store
                .bump_intent(op_id, due, "connection unavailable")?;
            return Ok(IntentResult::Retryable(
                ErrorCode::Internal,
                "connection unavailable".into(),
            ));
        };
        self.apply_response(row, body)
    }

    /// Hold an intent whose send never reached the server.
    ///
    /// The link being down says nothing about the message, so the queue keeps
    /// the row and the attempt counter stays where it was (plan §6 line 289).
    pub fn defer(&self, row: &IntentRow, reason: &str) -> Result<IntentResult> {
        let due = Utc::now() + self.next_delay(row.attempt.max(0) as u32 + 1);
        self.store.defer_intent(&row.op_id, due, reason)?;
        Ok(IntentResult::Retryable(
            ErrorCode::Internal,
            reason.to_string(),
        ))
    }

    fn apply_response(
        &self,
        row: &IntentRow,
        body: &onlyne_proto::ResBody,
    ) -> Result<IntentResult> {
        if body.ok {
            let receipt = body
                .data
                .as_ref()
                .and_then(|v| serde_json::from_value::<Receipt>(v.clone()).ok());
            self.store
                .accept_intent(&row.op_id, &body.data.clone().unwrap_or(Value::Null))?;
            return Ok(IntentResult::Accepted(receipt));
        }
        let error = body
            .error
            .as_ref()
            .context("error response missing payload")?;
        if error.code == ErrorCode::Duplicate {
            let receipt = body
                .data
                .as_ref()
                .and_then(|v| serde_json::from_value::<Receipt>(v.clone()).ok());
            self.store
                .accept_intent(&row.op_id, &body.data.clone().unwrap_or(Value::Null))?;
            return Ok(IntentResult::Accepted(receipt));
        }
        // A frame answered before the server session finished its `hello` names a
        // window of the connection, so the row waits for the handshake instead of
        // leaving the queue (plan §7 line 310's refusal). It waits with its
        // deadline where it is rather than on a backoff rung: the rows behind it
        // in the same batch are refused the same way, and the reconnect's own
        // flush has to be able to send them (plan §7 line 310).
        if error.message == onlyne_proto::HELLO_REQUIRED_MESSAGE {
            return Ok(IntentResult::NotAuthenticated);
        }
        if PERMANENT_ERRORS.contains(&error.code) {
            // The row leaves the queue for good, so this fault is the only record
            // of the refusal that outlives the process (plan §7).
            self.record_intent_fault(
                &row.env_json,
                "intent_dropped",
                &format!("{}: {}", error.code, error.message),
            )?;
            self.delete_intent(&row.op_id)?;
            return Ok(IntentResult::Dropped(error.code, error.message.clone()));
        }
        let next = row.attempt.saturating_add(1) as u32;
        if next >= self.attempts {
            self.store.exhaust_intent(&row.op_id, &error.message)?;
            self.record_intent_fault(&row.env_json, "intent_exhausted", &error.message)?;
            Ok(IntentResult::Exhausted)
        } else {
            let due = Utc::now() + self.next_delay(next);
            self.store.bump_intent(&row.op_id, due, &error.message)?;
            Ok(IntentResult::Retryable(error.code, error.message.clone()))
        }
    }

    fn delete_intent(&self, op_id: &str) -> Result<()> {
        self.store.delete_intent(op_id)?;
        Ok(())
    }

    /// Record a terminal intent as a fault: a local `faults` row plus the
    /// `report{kind:"fault"}` event the runloop forwards once the link is up.
    ///
    /// Both terminal kinds share this shape — `intent_exhausted` and
    /// `intent_dropped` differ only in the kind they name — so one helper keeps
    /// the task it is keyed by and the reason it carries identical for each.
    fn record_intent_fault(&self, env: &Value, kind: &str, reason: &str) -> Result<()> {
        let task = env
            .get("causality")
            .and_then(|v| v.get("task"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let _ = crate::reconcile::record_fault(&self.store, task, kind, "intent", reason)?;
        let report = Report::Fault {
            task_id: Some(task.to_string()),
            session_id: None,
            generation: None,
            seq: None,
            kind: kind.into(),
            reason: reason.into(),
            desired: None,
            observed: None,
        };
        let _ = self.store.append_event(
            "report_fault",
            &serde_json::to_value(&report).unwrap_or(Value::Null),
        );
        Ok(())
    }
}

pub fn op_for_intent(row: &IntentRow) -> Result<ClientOp> {
    if let Ok(op) = serde_json::from_value::<ClientOp>(row.env_json.clone()) {
        return Ok(op);
    }
    let envelope: Envelope = serde_json::from_value(row.env_json.clone())?;
    Ok(ClientOp::Send(Box::new(envelope)))
}

pub fn due(row: &IntentRow) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(&row.next_attempt_at)?.with_timezone(&Utc))
}

/// The session whose completion intent one queue row carries, when the row is
/// one.
///
/// Two shapes and no others, and the restriction is a soundness rule rather
/// than tidiness. A receipt closes the drain the completion is riding on, and
/// the reducer only admits one from a drain that is open — so feeding a receipt
/// for an accepted op that merely *names* a task would close the completion's
/// drain on the strength of something else entirely. An ack settles a delivery,
/// a ready or heartbeat report publishes a projection, and a note wakes an
/// agent: none of them is the completion, and none of them may end it. The two
/// that are, are the `Completion` envelope the settlement sends and the
/// `complete` report the residual reconcile sends when it holds no envelope.
///
/// The session is keyed by the task the intent answers, which is what a
/// client-held session's own row is keyed by. A row whose payload names no task
/// — a note, or an ack — answers `None`, and the caller leaves the reducer
/// alone rather than guessing a session.
pub fn completion_task_id(row: &IntentRow) -> Option<String> {
    if let Ok(envelope) = serde_json::from_value::<Envelope>(row.env_json.clone()) {
        if envelope.kind != MsgKind::Completion {
            return None;
        }
        return envelope.task_id().map(str::to_string);
    }
    match serde_json::from_value::<ClientOp>(row.env_json.clone()) {
        Ok(ClientOp::Report(Report::Complete { task_id, .. })) => Some(task_id),
        _ => None,
    }
}

pub fn permanent_error(code: ErrorCode) -> bool {
    PERMANENT_ERRORS.contains(&code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_proto::{Body, Causality, MsgKind, Principal, new_envelope};
    use onlyne_store::SessionLedger;
    use tempfile::tempdir;

    /// The task both the envelope and its fault row are keyed by.
    const TASK: &str = "3f1d6f4e-1a2b-4c3d-8e9f-0a1b2c3d4e5f";

    /// One task envelope whose causality names the task its fault row will carry.
    fn delivery(task: &str) -> Envelope {
        new_envelope(
            MsgKind::Task,
            Principal::role("planner"),
            Principal::role("qa"),
            Body::text("work"),
            Some(Causality::root(task)),
        )
        .expect("task envelope")
    }

    #[test]
    fn a_permanent_refusal_records_the_dropped_intent() {
        let dir = tempdir().expect("tempdir");
        let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
        let machine = IntentMachine::new(store.clone(), 3, vec![10]);
        assert!(machine.enqueue(&delivery(TASK)).expect("enqueue"));
        let row = machine
            .pending()
            .expect("the due queue")
            .into_iter()
            .next()
            .expect("the queued row");

        let body = ResBody::err(
            ErrorCode::AclDenied,
            "role planner may not address qa",
            None,
        );
        let result = machine
            .attempt(&row, Some(&body))
            .expect("the refusal is handled");

        assert_eq!(
            result,
            IntentResult::Dropped(
                ErrorCode::AclDenied,
                "role planner may not address qa".into()
            )
        );
        let fault = store
            .list_faults(TASK)
            .expect("stored faults")
            .into_iter()
            .find(|fault| fault.kind == "intent_dropped")
            .expect("the drop left a fault row");
        assert_eq!(fault.task_id, TASK);
        assert_eq!(fault.intent, "intent");
        assert_eq!(fault.reason, "acl_denied: role planner may not address qa");
        assert!(
            store
                .due_intents(Utc::now() + chrono::Duration::hours(1), 8)
                .expect("the queue")
                .is_empty(),
            "the refused intent is gone from the queue"
        );
    }
}
