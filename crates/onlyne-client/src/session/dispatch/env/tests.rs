use super::*;
use onlyne_layout::UNIX_SOCKET_PATH_MAX;
use tempfile::tempdir;

/// The guard reads its policy from the environment before its own
/// `relay.toml`, so what the client injects is the whole contract between
/// the spec and a spawned session: the list comma-joined, the count in
/// decimal, and neither variable at all when the spec names no policy.
#[test]
fn the_spawn_environment_carries_the_relay_policy_it_has() {
    let plain = session_env(
        "planner",
        "s-1",
        "t-1",
        &[],
        None,
        "cluster-a",
        Path::new(""),
    );
    assert_eq!(plain["ONLYNE_SESSION_ID"], "s-1");
    assert_eq!(plain["ONLYNE_TASK_ID"], "t-1");
    assert_eq!(plain["ONLYNE_ROLE"], "planner");
    // The topology name is the address a host backend groups sessions under.
    assert_eq!(plain["ONLYNE_CLUSTER"], "cluster-a");
    assert!(
        !plain.contains_key("ONLYNE_RELAY_REQUIRED") && !plain.contains_key("ONLYNE_RELAY_COUNT"),
        "no policy injects no key at all: {plain:?}"
    );
    // A surface that answered nothing names nothing: the plugin keeps its own
    // resolution for a hand-started session.
    assert!(!plain.contains_key("ONLYNE_SOCKET"), "{plain:?}");

    let listed = session_env(
        "planner",
        "s-1",
        "t-1",
        &["writer".to_string(), "auditor".to_string()],
        None,
        "",
        Path::new(""),
    );
    assert_eq!(listed["ONLYNE_RELAY_REQUIRED"], "writer,auditor");
    assert!(!listed.contains_key("ONLYNE_RELAY_COUNT"));
    // No welcome yet, so no topology to name: the key stays out rather than
    // arriving empty.
    assert!(!listed.contains_key("ONLYNE_CLUSTER"));

    let counted = session_env("planner", "s-1", "t-1", &[], Some(2), "", Path::new(""));
    assert_eq!(counted["ONLYNE_RELAY_COUNT"], "2");
    assert!(!counted.contains_key("ONLYNE_RELAY_REQUIRED"));

    // Both variables travel when the spec names both forms; the guard's own
    // precedence is what makes the list win.
    let both = session_env(
        "planner",
        "s-1",
        "t-1",
        &["writer".to_string()],
        Some(2),
        "",
        Path::new(""),
    );
    assert_eq!(both["ONLYNE_RELAY_REQUIRED"], "writer");
    assert_eq!(both["ONLYNE_RELAY_COUNT"], "2");
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

    let env = session_env("planner", "s-1", "t-1", &[], None, "", &served);
    assert_eq!(env["ONLYNE_SOCKET"], served.to_string_lossy().as_ref());
    let spec = SpawnSpec {
        cwd: workspace.clone(),
        task_id: "t-1".into(),
        command: vec!["pi".into()],
        env,
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
