//! One agent process per command, and the permission asks it makes.

use super::*;

#[test]
fn a_permission_ask_is_refused_and_the_turn_still_reports_its_answer() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-deny")).unwrap();
    let (outcome, _lines, log) = run_turn(&backend, &session, "t-deny", "MARK:ask go");

    assert_eq!(outcome.outcome, TaskState::Done, "{outcome:?}");
    // The agent's own account of the answer is the closing line.
    assert_eq!(outcome.head.as_deref(), Some("answer=no"));
    assert!(log.contains("answer=no"), "{log}");
    let refusals = outcome.refusals.expect("the refusal is reported");
    assert!(
        refusals.contains("1 permission ask(s) refused"),
        "{refusals}"
    );
    assert!(refusals.contains("policy=deny"), "{refusals}");
    assert!(refusals.contains("Edit hello.py"), "{refusals}");
    assert!(refusals.contains("option=reject_once"), "{refusals}");
    assert!(fake.traced().contains("answered no"));
    finish(&backend, &fake, &[&session]);
}

#[test]
fn an_allowing_client_grants_once_and_never_forever() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions {
        allow_permissions: true,
        ..AcpOptions::default()
    });
    let session = backend.spawn(fake.spec("t-allow")).unwrap();
    let (outcome, _lines, _log) = run_turn(&backend, &session, "t-allow", "MARK:ask go");
    assert_eq!(outcome.head.as_deref(), Some("answer=once"));
    assert!(outcome.refusals.is_none(), "{:?}", outcome.refusals);
    assert!(fake.traced().contains("answered once"));
    finish(&backend, &fake, &[&session]);
}

#[test]
fn a_blanket_grant_is_never_this_clients_choice() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions {
        allow_permissions: true,
        ..AcpOptions::default()
    });
    let session = backend.spawn(fake.spec("t-always")).unwrap();
    let (outcome, _lines, _log) = run_turn(&backend, &session, "t-always", "MARK:askalways go");
    // Only `allow_always` was on offer, and an autonomy decision belongs to
    // the operator through the session mode, not to a client default.
    assert_eq!(outcome.head.as_deref(), Some("answer=cancelled"));
    let refusals = outcome.refusals.expect("declining is recorded");
    assert!(refusals.contains("declined Edit hello.py"), "{refusals}");
    assert!(refusals.contains("option=-"), "{refusals}");
    assert!(
        !fake.traced().contains("answered always"),
        "{}",
        fake.traced()
    );
    finish(&backend, &fake, &[&session]);
}

#[test]
fn refusals_belong_to_the_turn_that_asked() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-two")).unwrap();
    let (first, _lines, _log) = run_turn(&backend, &session, "t-two", "MARK:ask first");
    assert!(first.refusals.is_some());
    // The refusal accumulator belongs to the turn that asked, so a later
    // turn on the same conversation reports none of it.
    let (second, _lines, _log) = run_turn(&backend, &session, "t-two", "second");
    assert_eq!(second.outcome, TaskState::Done);
    assert!(second.refusals.is_none(), "{:?}", second.refusals);
    finish(&backend, &fake, &[&session]);
}

#[test]
fn one_agent_process_serves_every_session_of_its_command() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let first = backend.spawn(fake.spec("t-a")).unwrap();
    let mut second_spec = fake.spec("t-b");
    second_spec.env.insert("UNUSED".to_string(), "1".into());
    let second = backend.spawn(second_spec).unwrap();
    assert_eq!(
        first.backend_ref["pid"], second.backend_ref["pid"],
        "the same argv shares one process"
    );
    assert_eq!(first.backend_ref["id"], "sess-1");
    assert_eq!(second.backend_ref["id"], "sess-2");

    // A command that renders per task is a different key, and gets its own
    // agent: legal, and the cost of a per-task identity in the argv.
    let mut own = fake.spec("t-c");
    own.command.push("--task=t-c".to_string());
    let third = backend.spawn(own).unwrap();
    assert_ne!(third.backend_ref["pid"], first.backend_ref["pid"]);
    assert_eq!(backend.state.agents.lock().len(), 2);

    // Closing one session of a shared process leaves the other usable.
    backend
        .close(&first, CloseReason::Completed, false)
        .expect("close one");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !fake.traced().contains("close sess-1") {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(fake.traced().contains("close sess-1"), "{}", fake.traced());
    assert!(backend.probe(&second).unwrap().alive);
    let (outcome, _lines, _log) = run_turn(&backend, &second, "t-b", "still here");
    assert_eq!(outcome.outcome, TaskState::Done);
    assert_eq!(outcome.head.as_deref(), Some("I edited hello.py."));
    finish(&backend, &fake, &[&second, &third]);
}

#[test]
fn an_agent_that_dies_mid_turn_fails_its_task_with_the_detail() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-die")).unwrap();
    let (outcome, lines, log) = run_turn(&backend, &session, "t-die", "MARK:die go");

    assert_eq!(outcome.outcome, TaskState::Failed);
    assert_eq!(outcome.head, None);
    let note = outcome.note.expect("the death is explained");
    assert!(note.contains("7"), "{note}");
    assert!(note.contains("boom"), "{note}");
    let record = &onlyne_records(&lines, "turn")[0];
    assert_eq!(record["stop_reason"], "(error)");
    // The turn's updates were already routed before the pipe closed, so the
    // journal keeps what the agent managed to say.
    assert!(
        lines
            .iter()
            .any(|line| line["sessionUpdate"] == "agent_thought_chunk"),
        "{lines:?}"
    );
    assert!(log.contains("> Reading the file.\n"), "{log}");
    assert!(!backend.probe(&session).unwrap().alive);
    assert!(
        !backend
            .attach(&session)
            .expect_err("a dead agent cannot be attached")
            .to_string()
            .contains("not held"),
    );
    assert!(backend.state.agents.lock().is_empty());
    let _ = backend.close(&session, CloseReason::Fault, false);
}
