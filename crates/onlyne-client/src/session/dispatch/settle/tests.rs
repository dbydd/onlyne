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
    let quiet = completion_envelope(
        "planner",
        Some(Principal::role("reviewer")),
        &task,
        None,
        None,
    )
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
        None,
    )
    .expect("a blank result line files too");
    assert_eq!(blank.body.text, quiet.body.text);

    let said = completion_envelope(
        "planner",
        Some(Principal::role("reviewer")),
        &task,
        Some("done"),
        None,
    )
    .expect("a result line travels verbatim");
    assert_eq!(said.body.text.as_deref(), Some("done"));

    // The one case that stays silent is the one with no sender to answer.
    assert!(
        completion_envelope("planner", None, &task, Some("done"), None).is_none(),
        "an unaddressed task files no receipt"
    );
}

/// A settled task inside a family answers its sender with the family's own
/// figures, so one run reads as one arc in `onlyne ledger`: the task rows and
/// the completion rows print the same family and the same depth.
#[test]
fn a_completion_carries_the_family_figures_of_the_task_it_answers() {
    let task = new_task_id();
    let root = new_task_id();
    let causality = Causality {
        task: task.clone(),
        parent_task: Some(root.clone()),
        reply_to: None,
        hop: 2,
        attempt: 0,
        family: Some(root.clone()),
        hop_budget: Some(7),
        origin: Some("_supervisor".into()),
        deadline: None,
        labels: Some(BTreeMap::from([("run".to_string(), "brief".to_string())])),
    };
    let receipt = completion_envelope(
        "planner",
        Some(Principal::role("reviewer")),
        &task,
        Some("done"),
        Some(&causality),
    )
    .expect("a settled task answers its sender");

    let link = receipt
        .causality
        .expect("the receipt carries the figures of the task it answers");
    assert_eq!(link.task, task);
    assert_eq!(link.hop, 2, "the receipt sits at the task's own depth");
    assert_eq!(link.family.as_deref(), Some(root.as_str()));
    assert_eq!(link.hop_budget, Some(7));
    assert_eq!(link.origin.as_deref(), Some("_supervisor"));
    assert_eq!(link.labels, causality.labels);
    assert_eq!(link.parent_task, None, "a receipt is no link in the chain");
    assert_eq!(link.reply_to, None);
}
