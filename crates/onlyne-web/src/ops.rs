//! The ops the browser may ask for, as admin ops.
//!
//! The vocabulary is the plan's (`docs/v2-PLAN.md` line 364): `send`,
//! `control`, `repair`, `report`, and the spec edits. A send from a board is a
//! `_supervisor` send — the operator is who typed it — and so is every other
//! op that carries a `from`: the reserved role is the operator's standing, and
//! the receipts of those sends are what the operator's board renders.

use onlyne_proto::{
    new_envelope, new_task_id, AdminControl, AdminOp, AdminReport, AdminSend, Body, Causality,
    ControlOp, MsgKind, Outcome, Principal, RepairAck, RepairFail, RepairTarget, Report, SpecApply,
    SpecEdit,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::render::OPERATOR_ROLE;

/// One op the browser asks `POST /api/op` to perform.
///
/// The shape mirrors the admin vocabulary it becomes, so a field can never be
/// read for an op that has no place to put it — the same rule the TUI's
/// `Prompt` keeps (`crates/onlyne-cli/src/tui/state.rs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum WebOp {
    /// Write a task to a board: a `_supervisor` send that starts a family.
    Send { to: String, text: String },
    /// Point a role's session at a task it did not pull.
    Focus { to: String, task_id: String },
    /// File a session's report as the operator.
    Report {
        task_id: String,
        /// One of the `Outcome` words: done, failed, cancelled, blocked.
        outcome: Outcome,
        head: String,
    },
    /// Mark a fault handled.
    RepairAck { fault_id: i64, reason: String },
    /// Re-queue a settled or faulted task once.
    RepairRetry { task_id: String, reason: String },
    /// Settle a task as failed.
    RepairFail { task_id: String, reason: String },
    /// Close a session's resource and settle its task.
    RepairClose { task_id: String, reason: String },
    /// Read a session's reducer state without changing it.
    RepairInspect { task_id: String },
    /// Read the structured spec, its path, and its source hash.
    SpecGet,
    /// Apply typed edits to `spec.toml` and reload the cluster.
    SpecApply {
        base_hash: String,
        edits: Vec<SpecEdit>,
    },
}

/// The admin op one browser op carries. A bad outcome word is a decode error,
/// which serde answers before this runs.
pub fn admin_op(op: WebOp) -> Result<AdminOp, String> {
    Ok(match op {
        WebOp::Send { to, text } => {
            // A send starts a family: a fresh task id at hop 0, exactly as the
            // client mints one for fresh work and the TUI's send key does.
            let envelope = new_envelope(
                MsgKind::Task,
                Principal::role(OPERATOR_ROLE),
                Principal::role(&to),
                Body {
                    text: Some(text),
                    head: None,
                    image: None,
                },
                Some(Causality::root(new_task_id())),
            )
            .map_err(|error| error.to_string())?;
            AdminOp::Send(AdminSend {
                from: OPERATOR_ROLE.to_string(),
                envelope: Box::new(envelope),
            })
        }
        WebOp::Focus { to, task_id } => AdminOp::Control(AdminControl {
            from: OPERATOR_ROLE.to_string(),
            op: ControlOp::Focus { task_id },
            to: Some(to),
        }),
        WebOp::Report {
            task_id,
            outcome,
            head,
        } => AdminOp::Report(AdminReport {
            from: OPERATOR_ROLE.to_string(),
            report: Box::new(Report::Complete {
                task_id,
                outcome,
                head: Some(head),
                details: None,
                files: Vec::new(),
                reply_to: None,
                cluster_ref: None,
            }),
        }),
        WebOp::RepairAck { fault_id, reason } => AdminOp::RepairAck(RepairAck { fault_id, reason }),
        WebOp::RepairRetry { task_id, reason } => AdminOp::RepairRetry(RepairTarget {
            task_id,
            reason: Some(reason),
        }),
        WebOp::RepairFail { task_id, reason } => {
            AdminOp::RepairFail(RepairFail { task_id, reason })
        }
        WebOp::RepairClose { task_id, reason } => AdminOp::RepairClose(RepairTarget {
            task_id,
            reason: Some(reason),
        }),
        WebOp::RepairInspect { task_id } => AdminOp::RepairInspect(RepairTarget {
            task_id,
            reason: None,
        }),
        WebOp::SpecGet => AdminOp::SpecGet(Value::Null),
        WebOp::SpecApply { base_hash, edits } => AdminOp::SpecApply(SpecApply { base_hash, edits }),
    })
}
