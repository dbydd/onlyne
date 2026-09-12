//! Extraction of the ledger fields a reply or handoff needs to link causality.
//!
//! `hop` is the counter this module reads; a row's `attempt` is the server's
//! delivery counter and `causality.attempt` on an envelope is the sender's view
//! through the transport, which reply and handoff never read back.

use onlyne_proto::{AdminOp, ClientOp, ErrorCode, LedgerQuery};
use serde_json::Value;
use tokio::net::UnixStream;

use crate::socket::{SocketTarget, Surface};
use crate::wire::{self, ExchangeError};

/// The response shapes a `query_ledger` answer may take, in the order they are
/// expected, from the most complete down to a bare row.
const ROW_LIST_KEYS: [&str; 4] = ["rows", "results", "ledger", "items"];

const ROW_FIELD_KEYS: [&str; 5] = ["msg_id", "task", "state", "out_head", "body"];

/// Rows returned by one `query_ledger` answer.
pub fn rows_of(data: &Value) -> Vec<Value> {
    match data {
        Value::Array(rows) => rows.clone(),
        Value::Object(map) => {
            for key in ROW_LIST_KEYS {
                if let Some(Value::Array(rows)) = map.get(key) {
                    return rows.clone();
                }
            }
            if ROW_FIELD_KEYS.iter().any(|key| map.contains_key(*key)) {
                return vec![data.clone()];
            }
            vec![]
        }
        _ => vec![],
    }
}

/// The row that sits deepest in its task family, by causality hop.
pub fn deepest(rows: &[Value]) -> Option<&Value> {
    rows.iter().max_by_key(|row| row_hop(row).unwrap_or(0))
}
/// A stored principal, held either as a JSON object or as a JSON string.
pub fn row_principal(row: &Value, key: &str) -> Option<onlyne_proto::Principal> {
    let raw = row.get(key)?;
    let object = match raw {
        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
        other => Some(other.clone()),
    };
    object.and_then(|value| serde_json::from_value(value).ok())
}

pub fn row_text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

/// Hop count of a row, read from the stored envelope when the column is absent.
pub fn row_hop(row: &Value) -> Option<u32> {
    if let Some(hop) = row
        .get("hop")
        .and_then(Value::as_u64)
        .and_then(|hop| u32::try_from(hop).ok())
    {
        return Some(hop);
    }
    for body in [
        row.get("body"),
        row.get("body_json")
            .and_then(Value::as_str)
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .as_ref(),
    ]
    .iter()
    .flatten()
    {
        if let Some(hop) = body
            .get("causality")
            .and_then(|c| c.get("hop"))
            .and_then(Value::as_u64)
            .and_then(|hop| u32::try_from(hop).ok())
        {
            return Some(hop);
        }
    }
    None
}

/// One `query_ledger` exchange against the chosen surface.
pub async fn query(
    stream: &mut UnixStream,
    timeout_ms: u64,
    target: &SocketTarget,
    request_id: String,
    args: LedgerQuery,
) -> Result<Vec<Value>, ExchangeError> {
    let request = match target.surface {
        Surface::Admin => wire::Outbound::admin(request_id, AdminOp::Ledger(args)),
        Surface::Client => wire::Outbound::client(request_id, ClientOp::QueryLedger(args)),
    };
    let body = wire::request_res(stream, &request, timeout_ms).await?;
    let Some(data) = body.data else {
        return Err(body_error(&body));
    };
    Ok(rows_of(&data))
}

/// Map a `query_ledger` rejection onto a wire error.
fn body_error(body: &onlyne_proto::ResBody) -> ExchangeError {
    let (code, message) = body
        .error
        .clone()
        .map(|error| (error.code, error.message))
        .unwrap_or_else(|| {
            (
                ErrorCode::Internal,
                "query_ledger did not answer with data".to_string(),
            )
        });
    ExchangeError::Wire(code, message)
}

/// The byte-exact message for a ledger lookup that came back empty.
pub fn missing_row_message(kind: &str, id: &str) -> String {
    format!("onlyne: no ledger row for {kind} {id}")
}
