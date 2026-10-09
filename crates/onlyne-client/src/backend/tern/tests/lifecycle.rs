//! A real-Tern lifecycle proof: what one spawn, a second spawn and their
//! closes do to a host this test owns.
//!
//! Ignored by default (`cargo test -p onlyne-client -- --ignored tern_lifecycle`)
//! because it needs the app: the real `tern` binary, which it finds through
//! `TERN_COMMAND` or at the backend's default path. Everything else is
//! hermetic — a scratch `TERN_CONFIG_DIR` and a daemon on its own socket, so
//! the operator's windows, sessions and panes are never addressed. The daemon
//! is this test's child and dies with it; a session or two may outlive a
//! panic inside the scratch daemon, whose launched programs are `sleep`s that
//! end on their own.

use super::*;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The real binary this proof drives, when the host has one.
fn tern_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("TERN_COMMAND") {
        if !path.trim().is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    let default = PathBuf::from(crate::backend::select::TERN_BINARY);
    default.exists().then_some(default)
}

/// A scratch daemon this test owns: killed and reaped on drop, whatever the
/// body did.
struct Daemon {
    child: std::process::Child,
    config: PathBuf,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.config);
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// A backend whose every call goes to the scratch daemon, over the real
/// binary and the real process runner.
fn backend(daemon: &Daemon, binary: &Path) -> TernBackend {
    let env: BTreeMap<String, String> = [
        ("TERN_COMMAND".to_string(), binary.to_string_lossy().into()),
        (
            "TERN_CONFIG_DIR".to_string(),
            daemon.config.to_string_lossy().into(),
        ),
        (
            "TERN_DAEMON_SOCKET".to_string(),
            daemon.socket.to_string_lossy().into(),
        ),
    ]
    .into();
    TernBackend::with_env(Arc::new(ProcessRunner), env)
}

/// One `SpawnSpec` against the scratch host: a role of a cluster this test
/// names, so a leaked session can never collide with a real one.
fn spec(task: usize) -> SpawnSpec {
    SpawnSpec {
        cwd: std::env::temp_dir(),
        task_id: format!("00000000-0000-4000-8000-{task:012}"),
        command: vec!["sleep".into(), "5".into()],
        env: [
            ("ONLYNE_ROLE".to_string(), "researcher".to_string()),
            ("ONLYNE_CLUSTER".to_string(), "lifecycle".to_string()),
        ]
        .into_iter()
        .collect(),
        tools_token: String::new(),
        prose: String::new(),
        focus: None,
        placement: None,
        rename: None,
    }
}

/// Whether the scratch daemon answers yet.
fn ready(backend: &TernBackend) -> bool {
    for _ in 0..50 {
        if backend.listing().is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// One block's id, under the tab and session that hold it.
fn topology(backend: &TernBackend) -> Vec<(String, Option<String>, Vec<String>)> {
    let listing = backend.listing().expect("listing the scratch daemon");
    listing
        .sessions
        .iter()
        .map(|session| {
            (
                session.id.clone(),
                session.name.clone(),
                session
                    .tabs
                    .iter()
                    .flat_map(|tab| tab.blocks.iter().map(|block| block.id.clone()))
                    .collect(),
            )
        })
        .collect()
}

fn blocks_of_role_tab(backend: &TernBackend, tab_id: &str) -> Vec<String> {
    let listing = backend.listing().expect("listing the scratch daemon");
    listing
        .tab(tab_id)
        .map(|tab| tab.blocks.iter().map(|block| block.id.clone()).collect())
        .unwrap_or_default()
}

#[test]
#[ignore = "needs the real Tern app; run with `cargo test -p onlyne-client -- --ignored`"]
fn tern_lifecycle_spawn_split_and_close_on_a_real_host() {
    let Some(binary) = tern_binary() else {
        eprintln!("skip: no tern binary (set TERN_COMMAND or install the app)");
        return;
    };
    let tag = std::process::id();
    let config = std::env::temp_dir().join(format!("onlyne-tern-lifecycle-{tag}"));
    std::fs::create_dir_all(config.join("plugins")).expect("scratch config dir");
    let socket = std::env::temp_dir().join(format!("onlyne-tern-lifecycle-{tag}.sock"));
    let child = Command::new(&binary)
        .env("TERN_CONFIG_DIR", &config)
        .args(["daemon", "--socket"])
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning the scratch tern daemon");
    let daemon = Daemon {
        child,
        config,
        socket,
    };
    let backend = backend(&daemon, &binary);
    assert!(ready(&backend), "the scratch daemon never answered");

    // An operator pane this test made first: it must outlive every agent
    // close below.
    let operator = backend
        .json(vec![
            "new".into(),
            "session".into(),
            "onlyne-lifecycle-operator".into(),
            "--cwd".into(),
            std::env::temp_dir().to_string_lossy().into(),
            "--json".into(),
            "--".into(),
            "sleep".into(),
            "30".into(),
        ])
        .expect("operator session");
    let operator_block =
        crate::backend::tern::policy::created_id(&operator, "block").expect("operator block id");

    // Fresh cluster: one spawn, and the host holds exactly one agent pane —
    // no scaffolding shell, the agent is the first block of a role-named tab.
    let first = backend.spawn(spec(1)).expect("first spawn");
    let one = crate::backend::tern::policy::TernRef::from_session(&first).expect("first ref");
    let blocks = blocks_of_role_tab(&backend, &one.tab_id);
    assert_eq!(
        blocks,
        vec![one.pane_id.clone()],
        "a fresh spawn holds exactly its agent pane, no anchor shell"
    );
    let listing = backend.listing().expect("listing after first spawn");
    let tab = listing.tab(&one.tab_id).expect("role tab after spawn");
    assert_eq!(
        tab.name.as_deref(),
        Some("researcher"),
        "the agent's block renamed its tab to the role"
    );

    // A second spawn of the same role adds exactly one pane to the same tab.
    let second = backend.spawn(spec(2)).expect("second spawn");
    let two = crate::backend::tern::policy::TernRef::from_session(&second).expect("second ref");
    assert_eq!(two.tab_id, one.tab_id, "both agents share the role tab");
    let blocks = blocks_of_role_tab(&backend, &one.tab_id);
    assert_eq!(
        blocks.len(),
        2,
        "a second same-role spawn adds one pane: {blocks:?}"
    );
    assert!(blocks.contains(&one.pane_id) && blocks.contains(&two.pane_id));

    // Closing every ref removes their panes and the role tab; the operator
    // pane survives them both.
    backend
        .close(&first, CloseReason::Completed, false)
        .expect("first close");
    backend
        .close(&second, CloseReason::Completed, false)
        .expect("second close");
    let blocks = blocks_of_role_tab(&backend, &one.tab_id);
    assert!(
        blocks.is_empty(),
        "the role tab goes with its last pane: {blocks:?}"
    );
    let topology = topology(&backend);
    assert!(
        topology
            .iter()
            .any(|(_, _, blocks)| blocks.contains(&operator_block)),
        "the operator pane survives the agent closes: {topology:?}"
    );

    // Leave the scratch host bare before the daemon dies with the frame.
    let _ = backend.json(vec![
        "kill".into(),
        "session".into(),
        "onlyne:lifecycle".into(),
        "--json".into(),
    ]);
    let _ = backend.json(vec![
        "kill".into(),
        "session".into(),
        "onlyne-lifecycle-operator".into(),
        "--json".into(),
    ]);
}
