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
fn a_replacement_process_keeps_the_sessions_and_reservations_of_its_own_command() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let dead = backend.spawn(fake.spec("t-dead")).unwrap();
    let (outcome, _lines, _log) = run_turn(&backend, &dead, "t-dead", "MARK:die go");
    assert_eq!(outcome.outcome, TaskState::Failed, "{outcome:?}");
    // The responder notices the corpse and takes it off the table on its own
    // clock; the session that served it stays named here until it is closed.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !backend.state.agents.lock().is_empty() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        backend.state.agents.lock().is_empty(),
        "the dead agent stayed"
    );

    // The same command is a runtime worth starting again, and a fresh agent
    // numbers its sessions from the top: this id is one the dead process
    // answered to, on a different process.
    let live = backend.spawn(fake.spec("t-live")).unwrap();
    assert_eq!(
        dead.backend_ref["id"], live.backend_ref["id"],
        "the replacement reused the session id, which is the case this guards"
    );
    assert_ne!(dead.backend_ref["pid"], live.backend_ref["pid"]);

    // Releasing the session of the process that already left must not release a
    // reservation of the process that did not: it never took one there, and a
    // release there would tear the replacement down under its own session.
    backend
        .close(&dead, CloseReason::Fault, false)
        .expect("closing a session of a dead process is allowed");
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        assert_eq!(
            backend.state.agents.lock().len(),
            1,
            "closing a dead session released the process that replaced it"
        );
        assert!(
            backend.state.sessions.lock().len() <= 1,
            "the dead session outlived its own close, or took the live one with it"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        backend.probe(&live).unwrap().alive,
        "the live agent is gone"
    );
    let (outcome, _lines, _log) = run_turn(&backend, &live, "t-live", "carry on");
    assert_eq!(outcome.outcome, TaskState::Done, "{outcome:?}");
    finish(&backend, &fake, &[&live]);
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
    // The reaper drops a dead agent on its own clock, the same way the close path
    // above is waited for. Read on the runner's schedule, never on the test's.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !backend.state.agents.lock().is_empty() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        backend.state.agents.lock().is_empty(),
        "the dead agent left the table within 20s",
    );
    let _ = backend.close(&session, CloseReason::Fault, false);
}
