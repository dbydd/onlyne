//! Contract of the testkit products the e2e scripts execute: the fake agent
//! binary and the shipped script files.

use std::path::Path;
use std::time::Duration;

use onlyne_adapter::AdapterServer;
use onlyne_proto::{
    Capability, ErrorCode, HelloAck, HelloArgs, HostOp, Mount, PROTOCOL_VERSION, ServerInfo,
};
use onlyne_testkit::{
    FakeAgent, HostSim, HostSimSpec, default_agent_capabilities, sample_assign, script_from_path,
};

/// Workspace config in the shape `onlyne-client init` writes it.
fn config_toml(workspace: &Path, role: &str) -> String {
    format!(
        "role = {role:?}\ncert_pin = \"sha256/AAAA\"\nkey_path = {:?}\nplugins = []\n\n[server]\nhost = \"127.0.0.1\"\nport = 7899\n",
        workspace
            .join(".onlyne/keys/role.key")
            .display()
            .to_string(),
    )
}

fn ack(hello: &HelloArgs) -> Result<HelloAck, (ErrorCode, String)> {
    let role = match hello.mount.as_ref() {
        Some(Mount::Agent(mount)) => mount.role.clone(),
        _ => String::new(),
    };
    Ok(HelloAck {
        protocol: PROTOCOL_VERSION,
        role,
        session_id: None,
        generation: 1,
        prose: String::new(),
        server: ServerInfo {
            connected: true,
            cluster: "local".to_string(),
            name: "test".to_string(),
        },
        host_capabilities: vec![Capability::Probe, Capability::Recycle],
    })
}

/// The e2e scripts run `onlyne-agent-fake --workspace DIR` with no `--role`, so
/// the mount role has to come from the workspace config the client reads.
#[tokio::test]
async fn fake_agent_mounts_the_role_named_in_the_workspace_config() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    std::fs::create_dir_all(workspace.join(".onlyne/run")).unwrap();
    std::fs::write(
        workspace.join(".onlyne/config.toml"),
        config_toml(&workspace, "cluster-b"),
    )
    .unwrap();

    let listener = tokio::net::UnixListener::bind(workspace.join(".onlyne/run/s")).unwrap();
    let (hello_tx, hello_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        AdapterServer::accept_unix(stream, move |hello| {
            let _ = hello_tx.send(hello.mount.clone());
            ack(hello)
        })
        .await
    });

    let status = tokio::process::Command::new(env!("CARGO_BIN_EXE_onlyne-agent-fake"))
        .args(["--workspace", workspace.to_str().unwrap(), "--once"])
        .status()
        .await
        .expect("run onlyne-agent-fake");
    assert!(status.success(), "fake agent exited with {status}");

    let mount = tokio::time::timeout(Duration::from_secs(5), hello_rx)
        .await
        .expect("the fake agent must answer hello")
        .expect("hello sender must stay alive");
    match mount {
        Some(Mount::Agent(mount)) => assert_eq!(mount.role, "cluster-b"),
        other => panic!("hello must mount an agent role, got {other:?}"),
    }
}

/// Plan line 498: `echo-complete.json` asserts the fake agent's received
/// `assign.prose` equals the prose its role spec entry carries. The literal lives
/// in `e2e/lib.sh` and in the shipped script, so this pins the two together and
/// then drives the shipped script in both directions.
#[tokio::test]
async fn shipped_script_asserts_the_assign_prose() {
    let prose = include_str!("../e2e/lib.sh")
        .lines()
        .find(|line| line.starts_with("E2E_PROSE="))
        .expect("lib.sh declares E2E_PROSE")
        .split('\'')
        .nth(1)
        .expect("E2E_PROSE is single-quoted");
    let script = script_from_path(Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scripts/echo-complete.json"
    )))
    .expect("parse echo-complete.json");
    let asserted = script
        .steps
        .iter()
        .find_map(|step| step.get("assert_prose_equals"))
        .and_then(serde_json::Value::as_str)
        .expect("echo-complete.json asserts assign.prose");
    assert_eq!(
        asserted, prose,
        "one prose literal for lib.sh and the script"
    );

    let matching = tempfile::tempdir().expect("tempdir");
    let mut spec = HostSimSpec::agent("planner", prose, vec![]);
    spec.scripted = vec![HostOp::Assign(sample_assign("task body", prose))];
    let host = HostSim::new(spec);
    let (agent, task) = host.clone().connect_agent();
    FakeAgent::new(
        "planner",
        default_agent_capabilities(),
        script.clone(),
        matching.path(),
    )
    .run(&agent)
    .await
    .expect("matching prose passes the shipped assertion");
    task.abort();
    assert_eq!(
        std::fs::read_to_string(matching.path().join("prose.log")).expect("prose.log"),
        prose
    );

    let mismatched = tempfile::tempdir().expect("tempdir");
    let mut spec = HostSimSpec::agent("planner", prose, vec![]);
    spec.scripted = vec![HostOp::Assign(sample_assign("task body", "other prose"))];
    let host = HostSim::new(spec);
    let (agent, task) = host.clone().connect_agent();
    let error = FakeAgent::new(
        "planner",
        default_agent_capabilities(),
        script,
        mismatched.path(),
    )
    .run(&agent)
    .await
    .expect_err("mismatched prose must fail the shipped assertion");
    assert!(
        error.to_string().contains("assert_prose_equals failed"),
        "error names the assertion: {error}"
    );
    assert!(!mismatched.path().join("prose.log").exists());
    task.abort();
}
