use super::*;
use onlyne_wire::socket::UNIX_SOCKET_PATH_MAX;
use tempfile::tempdir;

/// The spawn environment carries identity and nothing about the obligation.
///
/// The relay check is this client's own (`guards.rs`), read off the role's
/// `allowed_targets`, so a session process is handed no policy to enforce and
/// no variable to read one from.
#[test]
fn the_spawn_environment_carries_no_relay_policy() {
    let plain = session_env("planner", "s-1", "t-1", "cluster-a", Path::new(""));
    assert_eq!(plain["ONLYNE_SESSION_ID"], "s-1");
    assert_eq!(plain["ONLYNE_TASK_ID"], "t-1");
    assert_eq!(plain["ONLYNE_ROLE"], "planner");
    // The topology name is the address a host backend groups sessions under.
    assert_eq!(plain["ONLYNE_CLUSTER"], "cluster-a");
    // A surface that answered nothing names nothing: the plugin keeps its own
    // resolution for a hand-started session.
    assert!(!plain.contains_key("ONLYNE_SOCKET"), "{plain:?}");
    for key in [
        "ONLYNE_RELAY_REQUIRED",
        "ONLYNE_RELAY_COUNT",
        "ONLYNE_RELAY_REQUIRED_COUNT",
    ] {
        assert!(
            !plain.contains_key(key),
            "the obligation has no second reader, so no variable carries it: {plain:?}"
        );
    }

    // No welcome yet, so no topology to name: the key stays out rather than
    // arriving empty.
    let unregistered = session_env("planner", "s-1", "t-1", "", Path::new(""));
    assert!(!unregistered.contains_key("ONLYNE_CLUSTER"));
}

/// A workspace whose canonical socket spelling overflows `sun_path` still
/// hands the session the short served path, and the directory the session
/// starts in answers that same socket.
///
/// `SpawnSpec.cwd` is the workspace root and `ONLYNE_SOCKET` is the served
/// endpoint; both come out of one tree, so a plugin that resolves the socket
/// from its own cwd reaches the listener the client bound. Windows keeps the
/// canonical spelling as the bound spelling, so the premise lives on unix.
#[cfg(unix)]
#[test]
fn a_deep_workspace_hands_the_session_the_short_served_socket() {
    let segment = "deep-workspace-segment-aaaaaaaaaaaaaaaaaaaaaaaa";
    let dir = tempdir().unwrap();
    let workspace = dir.path().join(segment).join(segment).join("leaf");
    std::fs::create_dir_all(workspace.join(".onlyne/run")).unwrap();
    let layout = RoleWorkspace::resolve(&workspace);
    let natural = layout.socket_path_natural();
    assert!(
        natural.as_os_str().len() > UNIX_SOCKET_PATH_MAX,
        "the premise: the canonical spelling is over the bound: {} bytes at {}",
        natural.as_os_str().len(),
        natural.display(),
    );
    let served = served_socket(&workspace);
    assert!(
        served.as_os_str().len() <= UNIX_SOCKET_PATH_MAX,
        "the served spelling fits the bound: {} bytes at {}",
        served.as_os_str().len(),
        served.display(),
    );
    assert_ne!(served, natural, "the socket moved off the canonical path");

    let env = session_env("planner", "s-1", "t-1", "", &served);
    assert_eq!(env["ONLYNE_SOCKET"], served.to_string_lossy().as_ref());
    let spec = SpawnSpec {
        cwd: workspace.clone(),
        task_id: "t-1".into(),
        command: vec!["pi".into()],
        env,
        tools_token: String::new(),
        prose: String::new(),
        focus: None,
        placement: None,
        rename: None,
    };
    assert_eq!(
        RoleWorkspace::resolve(&spec.cwd).socket_path(),
        PathBuf::from(&spec.env["ONLYNE_SOCKET"]),
        "one tree answers both the cwd and the socket"
    );
}
