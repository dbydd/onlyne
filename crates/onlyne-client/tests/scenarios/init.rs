//! Workspace init: the legacy-config refusal, the on-disk modes of the role key and
//! socket, and the role fragment `onlyne init` prints.

use onlyne_client::{
    ops::init::{InitArgs, init, legacy_error_code},
    session::{adapter_socket::AdapterSocket, dispatch::DispatchState},
};
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn legacy_refusal_exits_2_and_creates_no_files() {
    let dir = tempdir().unwrap();
    let channels = dir.path().join(".onlyne/channels");
    std::fs::create_dir_all(&channels).unwrap();

    let server_dir = tempdir().unwrap();
    let server_spec = server_dir.path().join(".onlyne/spec.toml");
    std::fs::create_dir_all(server_spec.parent().unwrap()).unwrap();
    std::fs::write(&server_spec, "[server]\nname = \"srv\"\nlisten = \"127.0.0.1:7899\"\ncert_pin = \"sha256/0000000000000000000000000000000000000000000000000000000000000000\"\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let res = rt.block_on(init(InitArgs {
        workspace: dir.path().to_path_buf(),
        role: "planner".into(),
        server_root: server_dir.path().to_path_buf(),
        prose: String::new(),
    }));

    assert!(res.is_err());
    assert_eq!(legacy_error_code(), 2);
    // Assert no files created in workspace beyond channels
    let entries: Vec<_> = std::fs::read_dir(dir.path().join(".onlyne"))
        .unwrap()
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].as_ref().unwrap().file_name(), "channels");
}

#[test]
fn permissions_mode_600_for_role_key_and_socket() {
    let ws_dir = tempdir().unwrap();
    let server_dir = tempdir().unwrap();
    let server_spec = server_dir.path().join(".onlyne/spec.toml");
    std::fs::create_dir_all(server_spec.parent().unwrap()).unwrap();
    std::fs::write(&server_spec, "[server]\nname = \"srv\"\nlisten = \"127.0.0.1:7899\"\ncert_pin = \"sha256/0000000000000000000000000000000000000000000000000000000000000000\"\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let fragment = rt
        .block_on(init(InitArgs {
            workspace: ws_dir.path().to_path_buf(),
            role: "planner".into(),
            server_root: server_dir.path().to_path_buf(),
            prose: "v1 smoke prose".into(),
        }))
        .unwrap();
    assert!(fragment.starts_with("[[client]]\n"));
    // The knob comments ride the fragment as TOML comments; the effective
    // entry is the ten live lines in their fixed shape, and the comments are
    // the documented vocabulary behind them.
    let lines: Vec<&str> = fragment
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    assert_eq!(lines.len(), 9, "fragment shape is fixed: {fragment:?}");
    assert_eq!(lines[0], "[[client]]");
    assert_eq!(lines[1], "role = \"planner\"");
    assert!(
        lines[2].starts_with("key = \"ed25519/"),
        "key line is {}",
        lines[2]
    );
    assert_eq!(
        &lines[3..],
        &[
            "admin = false",
            "max_sessions = 1",
            "allowed_senders = [\"*\", \"planner\"]",
            "allowed_targets = [\"planner\"]",
            "prose = \"v1 smoke prose\"",
            "session_command = [\"pi\", \"--session-id\", \"{session}\", \"--session-dir\", \".pi/sessions\", \"-ns\"]",
        ]
    );
    assert!(
        lines[8].starts_with("session_command = "),
        "the live entry closes with the command line: {fragment:?}"
    );
    assert!(fragment.contains("key = \"ed25519/"));

    let key_path = ws_dir.path().join(".onlyne/keys/role.key");
    assert!(key_path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // The fragment publishes the public half of the stored seed, and that
    // string is a curve point: a fragment carrying the seed instead publishes
    // the private half and fails `parse_public` for roughly half of all seeds.
    let stored = onlyne_net::KeyPair::load(&key_path).unwrap();
    let published = lines[2].strip_prefix("key = ").unwrap().trim_matches('"');
    assert_eq!(published, stored.public_str());
    assert!(onlyne_net::parse_public(published).is_ok());

    // Stale socket cleanup and socket mode 0600
    let db_path = ws_dir.path().join(".onlyne/client.db");
    let store = ClientStore::open(&db_path).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let dispatch = DispatchState::new(
        "planner",
        ws_dir.path(),
        vec!["agent".into()],
        2,
        backend,
        store,
    );
    let adapter = AdapterSocket {
        workspace: ws_dir.path().to_path_buf(),
        role: "planner".into(),
        cluster: "c".into(),
        server: "s".into(),
        dispatch,
    };

    // Create a stale socket file
    let sock_path = adapter.path();
    std::fs::create_dir_all(sock_path.parent().unwrap()).unwrap();
    std::fs::write(&sock_path, "stale").unwrap();
    assert!(sock_path.exists());

    let (listener, endpoint) = rt.block_on(adapter.bind()).unwrap();
    assert_eq!(
        endpoint.actual(),
        sock_path.as_path(),
        "the bound endpoint is the path the accessor named: {}",
        endpoint.actual().display(),
    );
    #[cfg(unix)]
    {
        assert!(
            endpoint.marker().exists(),
            "the bind publishes the served path in {}",
            endpoint.marker().display(),
        );
    }
    // Windows cannot bind a unix UDS: the file at `run/s` is a marker naming the
    // NPFS pipe the listener holds, so there is no separate `run/socket` to
    // publish and the served path is the canonical spelling.
    #[cfg(not(unix))]
    {
        assert_eq!(endpoint.actual(), endpoint.natural());
        let served = std::fs::read_to_string(endpoint.actual()).expect("read the served run/s");
        assert!(
            served.starts_with("v1:"),
            "the served file names the pipe, got {served:?}"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let sock_mode = std::fs::metadata(endpoint.actual())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(sock_mode, 0o600);
    }
    drop(listener);
}

/// The printed fragment is a complete role entry: pasting it into `spec.toml`
/// and reloading yields a role whose `session_command` the client can spawn.
///
/// A fragment without that line parses and registers, and then leaves every
/// delivery staged with no process behind it: the box the operator assembled by
/// hand parks its tasks `in_flight` and no component says why. The seed is the
/// one `examples/supervisor/run.py` writes into its own ring entries.
#[test]
fn init_fragment_is_a_pasteable_spawnable_role() {
    let ws_dir = tempdir().unwrap();
    let server_dir = tempdir().unwrap();
    let server_spec = server_dir.path().join(".onlyne/spec.toml");
    std::fs::create_dir_all(server_spec.parent().unwrap()).unwrap();
    std::fs::write(&server_spec, "[server]\nname = \"srv\"\nlisten = \"127.0.0.1:7899\"\ncert_pin = \"sha256/0000000000000000000000000000000000000000000000000000000000000000\"\n").unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let fragment = rt
        .block_on(init(InitArgs {
            workspace: ws_dir.path().to_path_buf(),
            role: "planner".into(),
            server_root: server_dir.path().to_path_buf(),
            prose: "v1 smoke prose".into(),
        }))
        .unwrap();

    let spec = format!(
        "[server]\nname = \"srv\"\nlisten = \"127.0.0.1:7899\"\ncert_pin = \"sha256/0000000000000000000000000000000000000000000000000000000000000000\"\n\n{fragment}"
    );
    let parsed = onlyne_config::Spec::parse_str(&spec).expect("the pasted fragment is a spec");
    let planner = parsed
        .client
        .iter()
        .find(|entry| entry.role == "planner")
        .expect("the fragment registers the role");
    assert_eq!(
        planner.session_command,
        vec![
            "pi",
            "--session-id",
            "{session}",
            "--session-dir",
            ".pi/sessions",
            "-ns"
        ],
        "a registered role carries the spawn command the client renders per task"
    );
}
