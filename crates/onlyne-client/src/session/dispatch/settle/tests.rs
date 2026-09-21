use super::*;
use onlyne_proto::new_task_id;

/// Every settled task answers its sender, including the turn that left no
/// result line. The protocol requires a body, so the empty answer travels as
/// an empty text field, and the receipt survives validation. A dropped
/// receipt strands the origin: it waits on a task the role has already
/// retired, which is how a ring stops mid-circle.
#[test]
fn a_settled_task_without_a_result_line_still_files_its_receipt() {
    let task = new_task_id();
    let quiet = completion_envelope("planner", Some(Principal::role("reviewer")), &task, None)
        .expect("an answer with nothing to say is still an answer");
    assert_eq!(quiet.kind, MsgKind::Completion);
    assert_eq!(quiet.causality.as_ref().unwrap().task, task);
    assert_eq!(quiet.body.text.as_deref(), Some(""));
    assert_eq!(
        quiet.to,
        Principal::role("reviewer"),
        "the receipt is addressed to the sender"
    );

    // A blank head and an absent one are the same answer to the sender.
    let blank = completion_envelope(
        "planner",
        Some(Principal::role("reviewer")),
        &task,
        Some(""),
    )
    .expect("a blank result line files too");
    assert_eq!(blank.body.text, quiet.body.text);

    let said = completion_envelope(
        "planner",
        Some(Principal::role("reviewer")),
        &task,
        Some("done"),
    )
    .expect("a result line travels verbatim");
    assert_eq!(said.body.text.as_deref(), Some("done"));

    // The one case that stays silent is the one with no sender to answer.
    assert!(
        completion_envelope("planner", None, &task, Some("done")).is_none(),
        "an unaddressed task files no receipt"
    );
}
