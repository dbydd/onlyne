use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use onlyne_proto::{ClientOp, ErrorCode, Envelope, Frame, Receipt, Report};
use onlyne_store::{ClientStore, IntentRow};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

pub const PERMANENT_ERRORS: &[ErrorCode] = &[
    ErrorCode::AclDenied, ErrorCode::Invalid, ErrorCode::Conflict, ErrorCode::Forbidden,
    ErrorCode::UnknownRole, ErrorCode::NotAdmin, ErrorCode::BadFrame, ErrorCode::FrameTooLarge,
    ErrorCode::ProtocolVersion,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState { Pending, Retrying, Accepted, Exhausted }

impl IntentState { pub fn as_str(self) -> &'static str { match self { Self::Pending=>"pending", Self::Retrying=>"retrying", Self::Accepted=>"accepted", Self::Exhausted=>"exhausted" } } }

#[derive(Debug, Clone, PartialEq)]
pub enum IntentResult { Accepted(Option<Receipt>), Retryable(ErrorCode, String), Dropped(ErrorCode, String), Exhausted }

#[derive(Clone)]
pub struct IntentMachine {
    pub store: ClientStore,
    pub attempts: u32,
    pub backoff_ms: Vec<u64>,
}

impl IntentMachine {
    pub fn new(store: ClientStore, attempts: u32, backoff_ms: Vec<u64>) -> Self {
        Self { store, attempts, backoff_ms }
    }

    pub fn enqueue(&self, envelope: &Envelope) -> Result<bool> {
        let op_id = envelope.op_id.as_deref().context("intent envelope missing op_id")?;
        Ok(self.store.enqueue_intent(op_id, &serde_json::to_value(envelope)?)?)
    }

    pub fn pending(&self) -> Result<Vec<IntentRow>> { Ok(self.store.flush_order()?) }

    pub fn next_delay(&self, attempt: u32) -> Duration {
        let idx = attempt.saturating_sub(1) as usize;
        Duration::from_millis(self.backoff_ms.get(idx).copied().or_else(|| self.backoff_ms.last().copied()).unwrap_or(1_000))
    }

    pub fn attempt(&self, row: &IntentRow, response: Option<&Frame>) -> Result<IntentResult> {
        let op_id = row.op_id.as_str();
        let Some(frame) = response else {
            let next = row.attempt.saturating_add(1) as u32;
            if next >= self.attempts {
                self.store.exhaust_intent(op_id, "intent attempts exhausted")?;
                self.record_exhausted(&row.env_json, row.attempt, "intent attempts exhausted")?;
                return Ok(IntentResult::Exhausted);
            }
            let due = Utc::now() + self.next_delay(next);
            self.store.bump_intent(op_id, due, "connection unavailable")?;
            return Ok(IntentResult::Retryable(ErrorCode::Internal, "connection unavailable".into()));
        };
        let Frame::Res { body, .. } = frame else { return Ok(IntentResult::Retryable(ErrorCode::BadFrame, "expected response".into())); };
        self.apply_response(row, body)
    }

    fn apply_response(&self, row: &IntentRow, body: &onlyne_proto::ResBody) -> Result<IntentResult> {
        if body.ok {
            let receipt = body.data.as_ref().and_then(|v| serde_json::from_value::<Receipt>(v.clone()).ok());
            self.store.accept_intent(&row.op_id, &body.data.clone().unwrap_or(Value::Null))?;
            return Ok(IntentResult::Accepted(receipt));
        }
        let error = body.error.as_ref().context("error response missing payload")?;
        if error.code == ErrorCode::Duplicate {
            let receipt = body.data.as_ref().and_then(|v| serde_json::from_value::<Receipt>(v.clone()).ok());
            self.store.accept_intent(&row.op_id, &body.data.clone().unwrap_or(Value::Null))?;
            return Ok(IntentResult::Accepted(receipt));
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
        let task = env.get("causality").and_then(|v| v.get("task")).and_then(Value::as_str).unwrap_or("");
        let _ = onlyne_session::record_fault(&self.store, task, "intent_exhausted", "intent", reason)?;
        let _ = (attempt, json!(Report::Fault { task_id: Some(task.to_string()), session_id: None, generation: None, seq: None, kind: "intent_exhausted".into(), reason: reason.into(), desired: None, observed: None }));
        Ok(())
    }
}

pub fn op_for_intent(row: &IntentRow) -> Result<ClientOp> {
    let envelope: Envelope = serde_json::from_value(row.env_json.clone())?;
    Ok(ClientOp::Send(Box::new(envelope)))
}

pub fn due(row: &IntentRow) -> Result<DateTime<Utc>> { Ok(DateTime::parse_from_rfc3339(&row.next_attempt_at)?.with_timezone(&Utc)) }

pub fn permanent_error(code: ErrorCode) -> bool { PERMANENT_ERRORS.contains(&code) }
