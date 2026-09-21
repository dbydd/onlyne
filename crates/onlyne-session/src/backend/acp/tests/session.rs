//! Bringing a session up, configuring it, and closing it.

use super::*;

#[test]
fn a_spawned_acp_session_names_the_agent_it_runs_on() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-1")).expect("spawn");
    assert_eq!(session.backend, "acp");
    assert_eq!(session.task_id, "t-1");
    assert_eq!(session.backend_ref["id"], "sess-1");
    assert!(session.backend_ref["pid"].as_u64().unwrap() > 1);
    assert_eq!(session.backend_ref["agent"], fake.command().join(" "));
    assert!(backend.self_driven());
    assert!(backend.outcomes().is_some());
    assert!(backend.probe(&session).unwrap().alive);
    assert!(!backend.capabilities().rename);
    assert!(backend.attach(&session).expect("the live session attaches") == session);
    finish(&backend, &fake, &[&session]);
}

#[test]
fn a_configured_mode_and_model_are_applied_before_the_session_is_ready() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions {
        mode: "acceptEdits".into(),
        model: "qfmodel".into(),
        reasoning_effort: "high".into(),
        allow_permissions: false,
    });
    let session = backend.spawn(fake.spec("t-config")).unwrap();
    let traced = fake.traced();
    assert!(traced.contains("set_mode acceptEdits"), "{traced}");
    assert!(
        traced.contains("set_config_option model=qfmodel"),
        "{traced}"
    );
    assert!(
        traced.contains("set_config_option reasoning_effort=high"),
        "{traced}"
    );
    finish(&backend, &fake, &[&session]);
}

#[test]
fn an_agent_that_refuses_the_configured_mode_refuses_the_session() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions {
        mode: "yolo".into(),
        ..AcpOptions::default()
    });
    let mut spec = fake.spec("t-reject");
    spec.env.insert("ACP_REJECT_CONFIG".to_string(), "1".into());
    let error = backend
        .spawn(spec)
        .expect_err("a mode the agent will not take is not a session");
    assert!(error.to_string().contains("rejected mode"), "{error}");
    assert!(fake.traced().contains("set_mode yolo"), "{}", fake.traced());
    // The reservation the failed open took is given back, and with it the
    // process, so a refusing agent cannot accumulate.
    assert!(backend.state.agents.lock().is_empty());
    assert!(backend.state.sessions.lock().is_empty());
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !fake.traced().contains("eof") {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(fake.traced().contains("eof"), "{}", fake.traced());
}

#[test]
fn an_agent_without_session_close_leaves_the_call_closed() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let mut spec = fake.spec("t-noclose");
    spec.env.insert("ACP_NO_CLOSE".to_string(), "1".into());
    let session = backend.spawn(spec).unwrap();
    backend
        .close(&session, CloseReason::Completed, false)
        .expect("close succeeds without the method");
    assert!(!fake.traced().contains("close sess-"), "{}", fake.traced());
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !backend.state.agents.lock().is_empty() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(backend.state.agents.lock().is_empty());
}

#[test]
fn closing_a_session_this_client_does_not_hold_is_not_an_error() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-gone")).unwrap();
    backend
        .close(&session, CloseReason::Cancelled, false)
        .expect("the first close");
    backend
        .close(&session, CloseReason::Cancelled, false)
        .expect("a second close is a no-op");
    assert!(
        backend
            .deliver(&session, "t-gone", "anything")
            .expect_err("a released session takes no payload")
            .to_string()
            .contains("no live session"),
    );
    finish(&backend, &fake, &[]);
}

#[test]
fn a_session_with_no_command_and_a_busy_session_are_refused() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let mut none = fake.spec("t-empty");
    none.command = Vec::new();
    assert!(
        backend
            .spawn(none)
            .expect_err("nothing to run is not a session")
            .to_string()
            .contains("no session_command"),
    );

    let session = backend.spawn(fake.spec("t-busy")).unwrap();
    let feed = backend.outcomes().unwrap();
    backend
        .deliver(&session, "t-busy", "MARK:ask slow turn")
        .expect("the first delivery");
    // The agent parks its turn on the ask and this client refuses it, so the
    // turn is long enough to observe: a second payload for the same session is
    // refused, not queued behind a turn it was never meant to join.
    let busy = backend
        .deliver(&session, "t-other", "second")
        .expect_err("one turn at a time");
    assert!(busy.to_string().contains("still running a turn"), "{busy}");
    let outcome = await_outcome(&feed, "t-busy");
    assert_eq!(outcome.outcome, TaskState::Done);
    finish(&backend, &fake, &[&session]);
}
