//! Driving a session backend: the pane guard on protocol commands, and the
//! environment a spawn carries.

use crate::common::sample_envelope;
use onlyne_client::session::dispatch::{DispatchState, dispatch};
use onlyne_session::SessionLedger;
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use tempfile::tempdir;

/// Captures each spawn spec the dispatcher handed the backend.
#[derive(Clone, Default)]
struct RecordingSpawnBackend {
    inner: FakeBackend,
    specs: Arc<parking_lot::Mutex<Vec<onlyne_session::SpawnSpec>>>,
}

impl onlyne_session::SessionBackend for RecordingSpawnBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn capabilities(&self) -> onlyne_session::Capabilities {
        self.inner.capabilities()
    }
    fn available(&self) -> anyhow::Result<bool> {
        self.inner.available()
    }
    fn spawn(&self, spec: onlyne_session::SpawnSpec) -> anyhow::Result<onlyne_session::SessionRef> {
        self.specs.lock().push(spec.clone());
        self.inner.spawn(spec)
    }
    fn attach(
        &self,
        session: &onlyne_session::SessionRef,
    ) -> anyhow::Result<onlyne_session::SessionRef> {
        self.inner.attach(session)
    }
    fn probe(
        &self,
        session: &onlyne_session::SessionRef,
    ) -> anyhow::Result<onlyne_session::ResourceProbe> {
        self.inner.probe(session)
    }
    fn close(
        &self,
        session: &onlyne_session::SessionRef,
        reason: onlyne_session::CloseReason,
        force: bool,
    ) -> anyhow::Result<()> {
        self.inner.close(session, reason, force)
    }
}

/// A fake backend that answers with a pane backend's name, so a dispatch test
/// can drive the pane guard without a real terminal host.
#[derive(Clone, Default)]
struct NamedBackend {
    inner: FakeBackend,
    name: &'static str,
}

impl onlyne_session::SessionBackend for NamedBackend {
    fn name(&self) -> &'static str {
        self.name
    }
    fn capabilities(&self) -> onlyne_session::Capabilities {
        self.inner.capabilities()
    }
    fn available(&self) -> anyhow::Result<bool> {
        self.inner.available()
    }
    fn spawn(&self, spec: onlyne_session::SpawnSpec) -> anyhow::Result<onlyne_session::SessionRef> {
        self.inner.spawn(spec)
    }
    fn attach(
        &self,
        session: &onlyne_session::SessionRef,
    ) -> anyhow::Result<onlyne_session::SessionRef> {
        self.inner.attach(session)
    }
    fn probe(
        &self,
        session: &onlyne_session::SessionRef,
    ) -> anyhow::Result<onlyne_session::ResourceProbe> {
        self.inner.probe(session)
    }
    fn close(
        &self,
        session: &onlyne_session::SessionRef,
        reason: onlyne_session::CloseReason,
        force: bool,
    ) -> anyhow::Result<()> {
        self.inner.close(session, reason, force)
    }
}

/// A `--mode rpc` command on a pane backend names itself in the error and
/// leaves no trace: the task owns no slot and the store holds no row, so the
/// server takes the refusal as an ack and parks the reason in the ledger.
#[test]
fn pane_backend_refuses_a_protocol_session_command() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(NamedBackend {
        inner: FakeBackend::new(),
        name: "orca",
    });
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["pi".into(), "--mode".into(), "rpc".into(), "-ns".into()],
        2,
        backend,
        store.clone(),
    );

    let env = sample_envelope("planner", "task 1");
    let task_id = env.task_id().unwrap().to_string();
    let err = dispatch(&state, &env).unwrap_err();
    assert_eq!(
        err.to_string(),
        "orca backend cannot host a protocol session: --mode rpc speaks JSON-RPC on its own stdio and the pane would print the frames; set backend = \"exec\" or backend = \"acp\" in the workspace config"
    );
    assert_eq!(state.session_count(), 0, "the refused task owns no slot");
    assert!(
        store.get_session(&task_id).unwrap().is_none(),
        "the refused task owns no session row"
    );
}

/// The guard keys on the protocol tokens, not on the agent: the same pane
/// backend still spawns an interactive command, and the same protocol command
/// still spawns under a backend that is not a pane.
#[test]
fn pane_backend_still_spawns_an_interactive_session_command() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(NamedBackend {
        inner: FakeBackend::new(),
        name: "orca",
    });
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["pi".into(), "-ns".into(), "-nc".into()],
        2,
        backend,
        store.clone(),
    );

    dispatch(&state, &sample_envelope("planner", "task 1")).unwrap();
    assert_eq!(state.session_count(), 1);

    let protocol = DispatchState::new(
        "planner",
        dir.path(),
        vec!["pi".into(), "--mode".into(), "rpc".into(), "-ns".into()],
        2,
        Arc::new(FakeBackend::new()),
        store,
    );
    dispatch(&protocol, &sample_envelope("planner", "task 2")).unwrap();
}

/// Each protocol spelling earns its own refusal, naming the token the argv
/// actually carried.
#[test]
fn every_protocol_spelling_in_a_pane_session_command_is_refused() {
    for (name, command, token) in [
        (
            "zellij",
            vec!["agent".to_string(), "--acp".to_string()],
            "--acp",
        ),
        (
            "herdr",
            vec![
                "pi".to_string(),
                "--mode=rpc".to_string(),
                "-ns".to_string(),
            ],
            "--mode=rpc",
        ),
    ] {
        let dir = tempdir().unwrap();
        let store = ClientStore::open(dir.path().join("client.db")).unwrap();
        let backend = Arc::new(NamedBackend {
            inner: FakeBackend::new(),
            name,
        });
        let state = DispatchState::new("planner", dir.path(), command, 2, backend, store);
        let err = dispatch(&state, &sample_envelope("planner", "task 1")).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "{name} backend cannot host a protocol session: {token} speaks JSON-RPC on its own stdio and the pane would print the frames; set backend = \"exec\" or backend = \"acp\" in the workspace config"
            )
        );
        assert_eq!(state.session_count(), 0, "{name} refused {token}");
    }
}

#[test]
fn the_live_dispatch_leaves_pane_placement_to_the_backend() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(RecordingSpawnBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend.clone(),
        store,
    );

    let envelope = sample_envelope("planner", "task 1");
    let task_id = envelope.task_id().unwrap().to_string();
    dispatch(&state, &envelope).unwrap();

    let specs = backend.specs.lock();
    assert_eq!(specs.len(), 1, "dispatch spawns once for a fresh task");
    assert_eq!(specs[0].placement, None);
    assert_eq!(
        specs[0].env.get("ONLYNE_ROLE").map(String::as_str),
        Some("planner")
    );
    assert_eq!(
        specs[0].env.get("ONLYNE_TASK_ID").map(String::as_str),
        Some(task_id.as_str())
    );
    assert_eq!(
        specs[0].env.get("ONLYNE_SESSION_ID").map(String::as_str),
        Some(task_id.as_str())
    );
    // The socket travels with the identity: the spawn site resolves the same
    // workspace root that lands in `cwd`, so the session reaches the client that
    // spawned it through the path that one tree serves.
    assert_eq!(
        specs[0].env.get("ONLYNE_SOCKET").map(String::as_str),
        Some(
            onlyne_layout::RoleWorkspace::resolve(dir.path())
                .socket_path()
                .to_string_lossy()
                .as_ref()
        ),
        "the session is handed the served socket of its own workspace"
    );
}
