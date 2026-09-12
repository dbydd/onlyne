use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use onlyne_proto::{ClientOp, Envelope, ErrorCode, Receipt, Report, ResBody, new_op_id};
use onlyne_store::{ClientStore, IntentRow};
use rusqlite::Connection;
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
    Dropped(ErrorCode, String),
    Exhausted,
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

    pub fn attempt(&self, row: &IntentRow, response: Option<&ResBody>) -> Result<IntentResult> {
        let op_id = row.op_id.as_str();
        let Some(body) = response else {
            let next = row.attempt.saturating_add(1) as u32;
            if next >= self.attempts {
                self.store
                    .exhaust_intent(op_id, "intent attempts exhausted")?;
                self.record_exhausted(&row.env_json, row.attempt, "intent attempts exhausted")?;
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
        // leaving the queue (plan §7 line 310's refusal).
        if error.message == onlyne_proto::HELLO_REQUIRED_MESSAGE {
            return self.defer(row, "connection not authenticated");
        }
        if PERMANENT_ERRORS.contains(&error.code) {
            self.delete_intent(&row.op_id)?;
            return Ok(IntentResult::Dropped(error.code, error.message.clone()));
        }
        let next = row.attempt.saturating_add(1) as u32;
        if next >= self.attempts {
            self.store.exhaust_intent(&row.op_id, &error.message)?;
            self.record_exhausted(&row.env_json, row.attempt, &error.message)?;
            Ok(IntentResult::Exhausted)
        } else {
            let due = Utc::now() + self.next_delay(next);
            self.store.bump_intent(&row.op_id, due, &error.message)?;
            Ok(IntentResult::Retryable(error.code, error.message.clone()))
        }
    }

    fn delete_intent(&self, op_id: &str) -> Result<()> {
        let conn = Connection::open(self.store.path())?;
        conn.execute("DELETE FROM intents WHERE op_id = ?", [op_id])?;
        Ok(())
    }

    fn record_exhausted(&self, env: &Value, attempt: i64, reason: &str) -> Result<()> {
        let task = env
            .get("causality")
            .and_then(|v| v.get("task"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let _ =
            onlyne_session::record_fault(&self.store, task, "intent_exhausted", "intent", reason)?;
        let _ = attempt;
        let report = Report::Fault {
            task_id: Some(task.to_string()),
            session_id: None,
            generation: None,
            seq: None,
            kind: "intent_exhausted".into(),
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

pub fn permanent_error(code: ErrorCode) -> bool {
    PERMANENT_ERRORS.contains(&code)
}
