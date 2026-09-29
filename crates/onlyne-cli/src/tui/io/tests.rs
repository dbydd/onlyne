//! Tests for the board's one IO task.
//!
//! Everything here is the op mapping. `op` is the pure function that turns what
//! the screen asked for into the wire's own admin op, so the operations the plan
//! gives the TUI are pinned without a socket anywhere: the task's I/O half — the
//! five reads, the subscribe, the reconnect — is what the live run in this
//! slice's report exercises, because a fake socket here would pin a second
//! reader of the protocol rather than the ops the screen asks for.

use super::*;
use onlyne_proto::Outcome;

/// The admin op one action carries, or a panic naming the action that did not
/// map.
fn mapped(action: Action) -> AdminOp {
    op(action).expect("every action but refresh carries an op")
}

#[test]
fn a_send_is_one_task_envelope_at_a_fresh_family_root() {
    let op = mapped(Action::Send {
        from: "operator".to_string(),
        to: "planner".to_string(),
        body: "plan the work".to_string(),
    });
    let AdminOp::Send(sent) = op else {
        panic!("a send is an admin send, not {op:?}");
    };
    assert_eq!(sent.from, "operator");
    let envelope = sent.envelope;
    assert_eq!(envelope.kind, MsgKind::Task);
    assert_eq!(envelope.from, Principal::role("operator"));
    assert_eq!(envelope.to, Principal::role("planner"));
    assert_eq!(envelope.body.text.as_deref(), Some("plan the work"));
    assert!(!envelope.admin);
    // The idempotency key is what makes the operator's second `enter` a retry
    // rather than a second task.
    assert!(envelope.op_id.is_some(), "a task carries an op id");
    let causality = envelope.causality.as_ref().expect("a causality chain");
    assert_eq!(causality.hop, 0, "a send starts a family");
    assert_eq!(causality.parent_task, None, "nothing caused the root");
    assert_eq!(causality.family.as_deref(), Some(causality.task.as_str()));
}

#[test]
fn a_focus_names_the_target_the_operator_chose() {
    let op = mapped(Action::Focus {
        from: "operator".to_string(),
        to: "builder".to_string(),
        task_id: "t2".to_string(),
    });
    let AdminOp::Control(control) = op else {
        panic!("a focus is an admin control, not {op:?}");
    };
    assert_eq!(control.from, "operator");
    assert_eq!(control.to.as_deref(), Some("builder"));
    assert_eq!(
        control.op,
        ControlOp::Focus {
            task_id: "t2".to_string()
        }
    );
}

#[test]
fn every_repair_verb_carries_its_own_op_and_the_reason_it_needs() {
    let repairs = [
        (
            Repair::Ack {
                fault_id: 4,
                reason: "looked at it".to_string(),
            },
            "ack",
        ),
        (
            Repair::Retry {
                task_id: "t2".to_string(),
                reason: "once more".to_string(),
            },
            "retry",
        ),
        (
            Repair::Close {
                task_id: "t2".to_string(),
                reason: "done with it".to_string(),
            },
            "close",
        ),
        (
            Repair::Fail {
                task_id: "t2".to_string(),
                reason: "cannot be done".to_string(),
            },
            "fail",
        ),
        (
            Repair::Inspect {
                task_id: "t2".to_string(),
            },
            "inspect",
        ),
    ];
    for (repair, word) in repairs {
        let op = mapped(Action::Repair(repair));
        // One arm per verb: the op the wire sees is the verb the operator
        // pressed, and none of the five collapses into another.
        let seen = match op {
            AdminOp::RepairAck(ack) => {
                assert_eq!(ack.fault_id, 4);
                assert_eq!(ack.reason, "looked at it");
                "ack"
            }
            AdminOp::RepairRetry(target) => {
                assert_eq!(target.task_id, "t2");
                assert_eq!(target.reason.as_deref(), Some("once more"));
                "retry"
            }
            AdminOp::RepairClose(target) => {
                assert_eq!(target.task_id, "t2");
                assert_eq!(target.reason.as_deref(), Some("done with it"));
                "close"
            }
            AdminOp::RepairFail(fail) => {
                assert_eq!(fail.task_id, "t2");
                assert_eq!(fail.reason, "cannot be done");
                "fail"
            }
            AdminOp::RepairInspect(target) => {
                assert_eq!(target.task_id, "t2");
                assert_eq!(
                    target.reason, None,
                    "inspect changes nothing, so it carries no reason"
                );
                "inspect"
            }
            other => panic!("{word} mapped to {other:?}"),
        };
        assert_eq!(seen, word);
    }
}

#[test]
fn a_report_carries_the_verdict_and_one_head_line() {
    let op = mapped(Action::Report {
        from: "planner".to_string(),
        task_id: "t1".to_string(),
        outcome: Outcome::Done,
        head: "plan the work".to_string(),
    });
    let AdminOp::Report(report) = op else {
        panic!("a report is an admin report, not {op:?}");
    };
    assert_eq!(report.from, "planner");
    match report.report.as_ref() {
        Report::Complete {
            task_id,
            outcome,
            head,
            details,
            files,
            ..
        } => {
            assert_eq!(task_id, "t1");
            assert_eq!(*outcome, Outcome::Done);
            assert_eq!(head.as_deref(), Some("plan the work"));
            assert_eq!(*details, None);
            assert!(files.is_empty());
        }
        other => panic!("a report is a completion, not {other:?}"),
    }
}

#[test]
fn refresh_reads_the_snapshot_rather_than_an_op() {
    // `^R` re-reads the five reads, which is a snapshot and not an admin op: a
    // mapping that returned one would send the refresh down the op path.
    assert!(op(Action::Refresh).is_err());
}
