//! Driving a session backend: what the dispatcher hands a spawn, and the
//! environment a session carries.

use crate::common::sample_envelope;
use onlyne_client::backend::fake::FakeBackend;
use onlyne_client::session::dispatch::{DispatchState, dispatch};
use onlyne_store::ClientStore;
use std::sync::Arc;
use tempfile::tempdir;

/// Captures each spawn spec the dispatcher handed the backend.
#[derive(Clone, Default)]
struct RecordingSpawnBackend {
    inner: FakeBackend,
    specs: Arc<parking_lot::Mutex<Vec<onlyne_client::backend::SpawnSpec>>>,
}

impl onlyne_client::backend::SessionBackend for RecordingSpawnBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn capabilities(&self) -> onlyne_client::backend::Capabilities {
        self.inner.capabilities()
    }
    fn available(&self) -> anyhow::Result<bool> {
        self.inner.available()
    }
    fn spawn(
        &self,
        spec: onlyne_client::backend::SpawnSpec,
    ) -> anyhow::Result<onlyne_client::backend::SessionRef> {
        self.specs.lock().push(spec.clone());
        self.inner.spawn(spec)
    }
    fn attach(
        &self,
        session: &onlyne_client::backend::SessionRef,
    ) -> anyhow::Result<onlyne_client::backend::SessionRef> {
        self.inner.attach(session)
    }
    fn probe(
        &self,
        session: &onlyne_client::backend::SessionRef,
    ) -> anyhow::Result<onlyne_client::backend::ResourceProbe> {
        self.inner.probe(session)
    }
    fn close(
        &self,
        session: &onlyne_client::backend::SessionRef,
        reason: onlyne_client::backend::CloseReason,
        force: bool,
    ) -> anyhow::Result<()> {
        self.inner.close(session, reason, force)
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
    dispatch(&state, &envelope)
        .unwrap()
        .expect("the role has room for it");

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
            onlyne_config::layout::RoleWorkspace::resolve(dir.path())
                .socket_path()
                .to_string_lossy()
                .as_ref()
        ),
        "the session is handed the served socket of its own workspace"
    );
}
