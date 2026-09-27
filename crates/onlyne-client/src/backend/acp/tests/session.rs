//! Bringing a session up, configuring it, and closing it.

use super::*;

#[test]
fn the_role_prose_lives_in_a_client_owned_block_of_the_instruction_file() {
    // The on-disk spelling an operator finds in their workspace, spelled out
    // here rather than taken from the module: it is the surface they read.
    const BEGIN: &str = "<!-- onlyne:role-prose:begin -->";
    const END: &str = "<!-- onlyne:role-prose:end -->";
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let path = fake.root.path().join("AGENTS.md");

    // No file yet: the prose is written into one of its own.
    let mut prose = fake.spec("t-prose");
    prose.prose = "You plan the work.".to_string();
    let first = backend.spawn(prose).expect("spawn");
    assert_eq!(read(&path), format!("{BEGIN}\nYou plan the work.\n{END}\n"));

    // The file belongs to the operator: a block already there is replaced where
    // it stands, and not a byte outside the markers moves.
    fs::write(
        &path,
        format!("# Operator notes\n\nkeep me\n\n{BEGIN}\nstale prose\n{END}\n\ntail\n"),
    )
    .expect("the operator's file");
    let mut prose = fake.spec("t-prose-2");
    prose.prose = "You review the work.".to_string();
    let second = backend.spawn(prose).expect("spawn");
    let written = read(&path);
    assert!(
        written.starts_with("# Operator notes\n\nkeep me\n\n"),
        "{written}"
    );
    assert!(written.ends_with("tail\n"), "{written}");
    assert!(written.contains("You review the work."), "{written}");
    assert!(!written.contains("stale prose"), "{written}");

    // A role that no longer has prose takes its block out, and nothing else:
    // prose this client would be standing behind after it stopped is worse than
    // no prose at all.
    let third = backend.spawn(fake.spec("t-prose-3")).expect("spawn");
    let written = read(&path);
    assert!(!written.contains(BEGIN), "{written}");
    assert!(!written.contains(END), "{written}");
    assert!(
        written.starts_with("# Operator notes\n\nkeep me\n"),
        "{written}"
    );
    assert!(written.ends_with("tail\n"), "{written}");

    // A begin marker with no end after it owns the rest of the file, because
    // everything from this client's own marker on is text this client wrote: a
    // torn block is replaced rather than doubled.
    fs::write(&path, format!("keep\n\n{BEGIN}\nhalf a block\n")).expect("a torn block");
    let mut prose = fake.spec("t-prose-4");
    prose.prose = "Prose again.".to_string();
    let fourth = backend.spawn(prose).expect("spawn");
    assert_eq!(read(&path), format!("keep\n\n{BEGIN}\nProse again.\n{END}"));
    finish(&backend, &fake, &[&first, &second, &third, &fourth]);
}

#[test]
fn a_session_is_opened_with_the_tools_mount_that_speaks_for_it() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-mount")).expect("spawn");

    let mounts = traced_mounts(&fake);
    assert_eq!(mounts.len(), 1, "{mounts:?}");
    let mount = &mounts[0];
    assert_eq!(mount["type"], "stdio");
    assert_eq!(mount["name"], "onlyne");
    assert_eq!(
        mount["command"],
        fake.stub().to_string_lossy().as_ref(),
        "the mount runs the CLI this client resolved, not a second PATH lookup"
    );
    assert_eq!(mount["args"], serde_json::json!(["mcp"]));
    assert_eq!(
        mount_env(mount, "ONLYNE_MCP_TOKEN").as_deref(),
        Some(TOOLS_TOKEN)
    );
    assert_eq!(
        mount_env(mount, "ONLYNE_SOCKET").map(PathBuf::from),
        Some(RoleWorkspace::resolve(fake.root.path()).socket_path()),
        "the mount dials the socket this session's own workspace resolves to"
    );

    // The token is the mount's, and the agent child is not the mount: the
    // environment the model's own process runs with must not carry it (§3b).
    let env = traced_env(&fake);
    assert!(
        env.get("ONLYNE_MCP_TOKEN").is_none(),
        "the capability reached the agent's environment: {env}"
    );

    // A session the client handed an endpoint itself keeps that spelling: the
    // agent child was given that socket, and a mount dialing another one would
    // speak to a process nobody serves.
    let mut served = fake.spec("t-mount-served");
    served.env.insert(
        "ONLYNE_SOCKET".to_string(),
        "/tmp/onlyne-served.sock".to_string(),
    );
    let second = backend.spawn(served).expect("spawn");
    let mounts = traced_mounts(&fake);
    assert_eq!(
        mount_env(&mounts[1], "ONLYNE_SOCKET").as_deref(),
        Some("/tmp/onlyne-served.sock")
    );
    finish(&backend, &fake, &[&session, &second]);
}

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
fn an_empty_command_a_foreign_task_and_a_busy_session_are_refused() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let mut none = fake.spec("t-empty");
    none.command = Vec::new();
    assert!(
        backend
            .spawn(none)
            .expect_err("nothing to run is not a session")
            .to_string()
            .contains("[client.runtime] command` is empty"),
    );

    let session = backend.spawn(fake.spec("t-busy")).unwrap();
    let feed = backend.outcomes().unwrap();
    backend
        .deliver(&session, "t-busy", "MARK:ask slow turn")
        .expect("the first delivery");
    // One session serves the task it was opened for. A payload naming another
    // task is refused before this client claims the turn or writes anything
    // beside it: the second task needs a session of its own, and a turn that
    // journalled itself under a task the agent was never assigned could never
    // be reconciled afterwards.
    let foreign = backend
        .deliver(&session, "t-other", "second")
        .expect_err("a session takes one task");
    assert!(
        foreign.to_string().contains("needs a session of its own"),
        "{foreign}"
    );
    // The agent parks its turn on the ask and this client refuses it, so the
    // turn is long enough to observe: a second payload for its own task is
    // refused too, not queued behind a turn it was never meant to join.
    let busy = backend
        .deliver(&session, "t-busy", "second")
        .expect_err("one turn at a time");
    assert!(busy.to_string().contains("still running a turn"), "{busy}");
    let outcome = await_outcome(&feed, "t-busy");
    assert_eq!(outcome.outcome, NO_VERDICT);
    finish(&backend, &fake, &[&session]);
}
