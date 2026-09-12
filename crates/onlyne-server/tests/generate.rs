use onlyne_config::{ClientEntry, IntentPolicy, ServerSection, Spec, Timeouts};
use onlyne_server::generate::{GenerateArgs, generate};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const ZERO_KEY: &str = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const ZERO_PIN: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn client_entry(role: &str) -> ClientEntry {
    ClientEntry {
        role: role.to_string(),
        key: ZERO_KEY.to_string(),
        prose: format!("prose for {role}"),
        admin: false,
        max_sessions: 1,
        reuse: false,
        allowed_senders: vec![],
        allowed_targets: vec![],
        session_command: vec![],
        timeout: Timeouts {
            ready_ms: 30_000,
            running_ms: 120_000,
            idle_ms: 60_000,
        },
        intent: IntentPolicy {
            attempts: 3,
            backoff_ms: vec![1000, 2000, 4000],
        },
        aggregate: String::new(),
    }
}

fn spec_with_roles(roles: &[&str]) -> Spec {
    Spec {
        server: ServerSection {
            name: "test-cluster".to_string(),
            listen: "127.0.0.1:17811".to_string(),
            cert_pin: ZERO_PIN.to_string(),
            agent_package: String::new(),
            ..Default::default()
        },
        client: roles.iter().map(|role| client_entry(role)).collect(),
        gateway: vec![],
        route: vec![],
    }
}

fn write_template(root: &Path, relative: &str, files: &[(&str, &str)]) {
    let base = root.join(".onlyne/templates").join(relative);
    for (name, content) in files {
        let path = base.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

fn args(root: &Path, out: &Path) -> GenerateArgs {
    GenerateArgs {
        root: root.to_path_buf(),
        templates: vec![],
        roles: vec![],
        out: Some(out.to_path_buf()),
        force: false,
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

#[test]
fn first_run_creates_plan_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(
        &root,
        "dev/planner",
        &[("AGENTS.md", "hello {{role}} pin={{cert_pin}}")],
    );
    fs::write(
        root.join(".onlyne/templates/dev/planner/.pi/onlyne.json"),
        "{}",
    )
    .ok();
    let spec = spec_with_roles(&["planner"]);
    let report = generate(&args(&root, &out), &spec).expect("generate");
    assert_eq!(report.fragment.lines().next().unwrap(), "[[client]]");
    assert!(report.fragment.contains("ed25519/"));
    let ws = out.join("dev/planner");
    let agents = fs::read_to_string(ws.join("AGENTS.md")).unwrap();
    assert!(agents.contains("hello planner"));
    assert!(!agents.contains("{{"));
    let config = fs::read_to_string(ws.join(".onlyne/config.toml")).unwrap();
    assert!(config.contains("role = \"planner\""));
    assert!(config.contains("key_path = \"keys/role.key\""));
    assert!(config.contains("[server]"));
    let key = ws.join(".onlyne/keys/role.key");
    assert_eq!(
        fs::metadata(&key).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::metadata(&key).unwrap().len(), 32);
    assert!(!ws.join(".onlyne/run").exists());
    assert!(!ws.join("spec.toml").exists());
}

#[test]
fn rerun_without_force_refuses_with_byte_exact_message() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "v1 {{role}}")]);
    let spec = spec_with_roles(&["planner"]);
    generate(&args(&root, &out), &spec).unwrap();
    let before = fs::read(out.join("dev/planner/AGENTS.md")).unwrap();
    let err = generate(&args(&root, &out), &spec).expect_err("must refuse");
    assert_eq!(err.exit_code(), 4);
    assert_eq!(
        err.to_string(),
        format!(
            "onlyne: workspace exists at {}; pass --force to overwrite",
            out.join("dev/planner").display()
        )
    );
    assert_eq!(fs::read(out.join("dev/planner/AGENTS.md")).unwrap(), before);
    assert_eq!(walk(&out).len(), walk(&out).len());
}

#[test]
fn force_replaces_generated_and_preserves_runtime_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "v1 {{role}}")]);
    let spec = spec_with_roles(&["planner"]);
    generate(&args(&root, &out), &spec).unwrap();
    let ws = out.join("dev/planner");
    let key_path = ws.join(".onlyne/keys/role.key");
    let key_before = fs::read(&key_path).unwrap();
    let key_mtime = fs::metadata(&key_path).unwrap().modified().unwrap();
    fs::write(ws.join(".onlyne/client.db"), b"sentinel-db").unwrap();
    fs::create_dir_all(ws.join(".onlyne/run")).unwrap();
    fs::write(ws.join(".onlyne/run/s"), b"sentinel-sock").unwrap();
    fs::create_dir_all(ws.join(".onlyne/logs")).unwrap();
    fs::write(ws.join(".onlyne/logs/server.log"), b"sentinel-log").unwrap();
    let db_mtime = fs::metadata(ws.join(".onlyne/client.db"))
        .unwrap()
        .modified()
        .unwrap();
    fs::write(
        root.join(".onlyne/templates/dev/planner/AGENTS.md"),
        "v2 {{role}}",
    )
    .unwrap();
    let mut forced = args(&root, &out);
    forced.force = true;
    generate(&forced, &spec).expect("force");
    assert_eq!(
        fs::read_to_string(ws.join("AGENTS.md")).unwrap(),
        "v2 planner"
    );
    assert_eq!(fs::read(&key_path).unwrap(), key_before);
    assert_eq!(
        fs::metadata(&key_path).unwrap().modified().unwrap(),
        key_mtime
    );
    assert_eq!(
        fs::read(ws.join(".onlyne/client.db")).unwrap(),
        b"sentinel-db"
    );
    assert_eq!(
        fs::metadata(ws.join(".onlyne/client.db"))
            .unwrap()
            .modified()
            .unwrap(),
        db_mtime
    );
    assert_eq!(
        fs::read(ws.join(".onlyne/run/s")).unwrap(),
        b"sentinel-sock"
    );
    assert!(ws.join(".onlyne/run").is_dir());
    assert_eq!(
        fs::read(ws.join(".onlyne/logs/server.log")).unwrap(),
        b"sentinel-log"
    );
    assert!(ws.join(".onlyne/logs").is_dir());
}

#[test]
fn role_filter_selects_subset() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "p {{role}}")]);
    write_template(&root, "dev/builder", &[("AGENTS.md", "b {{role}}")]);
    let spec = spec_with_roles(&["planner", "builder"]);
    let mut filtered = args(&root, &out);
    filtered.roles = vec!["builder".to_string()];
    let report = generate(&filtered, &spec).unwrap();
    assert!(out.join("dev/builder/AGENTS.md").is_file());
    assert!(!out.join("dev/planner").exists());
    assert!(report.fragment.contains("builder"));
    assert!(!report.fragment.contains("planner"));
    assert_eq!(report.roles.len(), 1);
}

#[test]
fn empty_intersection_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "p")]);
    let spec = spec_with_roles(&["planner"]);
    let mut filtered = args(&root, &out);
    filtered.roles = vec!["ghost".to_string()];
    let err = generate(&filtered, &spec).expect_err("empty intersection");
    assert_eq!(err.exit_code(), 4);
    assert_eq!(
        err.to_string(),
        "onlyne: no role matches the requested templates/roles"
    );
    assert!(!out.exists());
}

#[test]
fn ambiguous_basename_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "dev")]);
    write_template(&root, "prod/planner", &[("AGENTS.md", "prod")]);
    let spec = spec_with_roles(&["planner"]);
    let err = generate(&args(&root, &out), &spec).expect_err("ambiguous");
    assert_eq!(err.exit_code(), 4);
    assert_eq!(
        err.to_string(),
        "onlyne: template for role planner is ambiguous: dev/planner, prod/planner"
    );
    assert!(!out.exists());
}

#[test]
fn missing_template_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    let spec = spec_with_roles(&["ghost"]);
    let err = generate(&args(&root, &out), &spec).expect_err("missing template");
    assert_eq!(err.exit_code(), 4);
    assert_eq!(
        err.to_string(),
        format!(
            "onlyne: no template directory named ghost under {}",
            root.join(".onlyne/templates").display()
        )
    );
    assert!(!out.exists());
}

#[test]
fn explicit_template_takes_its_own_topology() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "dev")]);
    write_template(&root, "prod/planner", &[("AGENTS.md", "prod")]);
    let spec = spec_with_roles(&["planner"]);
    let mut picked = args(&root, &out);
    picked.templates = vec!["prod/planner".to_string()];
    generate(&picked, &spec).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("prod/planner/AGENTS.md")).unwrap(),
        "prod"
    );
    assert!(!out.join("dev").exists());
}

#[test]
fn agent_package_placeholder_without_setting_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(
        &root,
        "dev/planner",
        &[("AGENTS.md", "pkg={{agent_package}}")],
    );
    let spec = spec_with_roles(&["planner"]);
    let err = generate(&args(&root, &out), &spec).expect_err("unset package");
    assert_eq!(err.exit_code(), 4);
    assert_eq!(
        err.to_string(),
        "onlyne: agent_package not set in spec.toml [server]"
    );
    assert!(!out.exists());
}

#[test]
fn agent_package_absent_leaves_no_vendored_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "plain {{role}}")]);
    let spec = spec_with_roles(&["planner"]);
    generate(&args(&root, &out), &spec).unwrap();
    assert!(!out.join("dev/planner/.onlyne/agent").exists());
}

#[test]
fn agent_package_is_vendored_and_settings_point_at_the_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    let package = tmp.path().join("pkg-source/pi-onlyne");
    fs::create_dir_all(package.join("target")).unwrap();
    fs::create_dir_all(package.join(".git")).unwrap();
    fs::write(package.join("package.json"), "{\"name\":\"pi-onlyne\"}").unwrap();
    fs::write(package.join("target/junk.bin"), b"junk").unwrap();
    fs::write(package.join(".git/config"), b"git").unwrap();
    fs::create_dir_all(&root).unwrap();
    write_template(
        &root,
        "dev/planner",
        &[
            ("AGENTS.md", "plug {{agent_package}}"),
            (".pi/settings.json", "{\"packages\":[\"PLACEHOLDER\"]}"),
        ],
    );
    fs::write(
        root.join(".onlyne/templates/dev/planner/.pi/settings.json"),
        format!(
            "{{\"packages\":[{}]}}",
            serde_json::to_string(&package.display().to_string()).unwrap()
        ),
    )
    .unwrap();
    let mut spec = spec_with_roles(&["planner"]);
    spec.server.agent_package = package.display().to_string();
    generate(&args(&root, &out), &spec).unwrap();
    let ws = out.join("dev/planner");
    assert!(ws.join(".onlyne/agent/pi-onlyne/package.json").is_file());
    assert!(!ws.join(".onlyne/agent/pi-onlyne/target").exists());
    assert!(!ws.join(".onlyne/agent/pi-onlyne/.git").exists());
    let settings = fs::read_to_string(ws.join(".pi/settings.json")).unwrap();
    // The settings entry is the one path pi resolves relative to the settings
    // directory, so it carries the `../` its loader needs; nothing else in the
    // generated workspace names the package that way.
    assert!(
        settings.contains("\"../.onlyne/agent/pi-onlyne\""),
        "{settings}"
    );
    assert!(!settings.contains(&package.display().to_string()));
    let agents = fs::read_to_string(ws.join("AGENTS.md")).unwrap();
    assert!(agents.contains("plug .onlyne/agent/pi-onlyne"));
}

#[test]
fn agent_package_placeholder_in_settings_renders_the_parent_relative_form() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    let package = tmp.path().join("pkg-source/pi-onlyne");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("package.json"), "{\"name\":\"pi-onlyne\"}").unwrap();
    fs::create_dir_all(&root).unwrap();
    write_template(
        &root,
        "dev/planner",
        &[
            ("AGENTS.md", "plug {{agent_package}}"),
            (
                ".pi/settings.json",
                "{\"packages\":[\"{{agent_package}}\"]}",
            ),
        ],
    );
    let mut spec = spec_with_roles(&["planner"]);
    spec.server.agent_package = package.display().to_string();
    generate(&args(&root, &out), &spec).unwrap();
    let ws = out.join("dev/planner");
    assert!(ws.join(".onlyne/agent/pi-onlyne/package.json").is_file());
    let settings = fs::read_to_string(ws.join(".pi/settings.json")).unwrap();
    // The settings file carries the form pi resolves from `<ws>/.pi`.
    assert!(
        settings.contains("\"../.onlyne/agent/pi-onlyne\""),
        "{settings}"
    );
    let agents = fs::read_to_string(ws.join("AGENTS.md")).unwrap();
    // Everything else stays workspace-root-relative.
    assert!(agents.contains("plug .onlyne/agent/pi-onlyne"));
    assert!(!agents.contains("../.onlyne"));
}

#[test]
fn manifest_shape_is_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "p")]);
    let spec = spec_with_roles(&["planner"]);
    let report = generate(&args(&root, &out), &spec).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&report.manifest).unwrap();
    let mut keys: Vec<_> = manifest.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, vec!["generated_at", "roles", "server_root"]);
    chrono::DateTime::parse_from_rfc3339(manifest["generated_at"].as_str().unwrap()).unwrap();
    assert!(manifest["server_root"].as_str().unwrap().starts_with('/'));
    let role = &manifest["roles"][0];
    let mut role_keys: Vec<_> = role.as_object().unwrap().keys().cloned().collect();
    role_keys.sort();
    assert_eq!(role_keys, vec!["dir", "key", "role", "template"]);
    assert_eq!(role["role"], "planner");
    assert_eq!(role["dir"], "dev/planner");
    assert_eq!(role["template"], "dev/planner");
    assert!(role["key"].as_str().unwrap().starts_with("ed25519/"));
    assert_eq!(
        fs::read_to_string(out.join(".onlyne-generation.json")).unwrap(),
        report.manifest
    );
}

#[test]
fn cert_pin_from_server_cert_reaches_every_config() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "p")]);
    write_template(&root, "dev/builder", &[("AGENTS.md", "b")]);
    let spec = spec_with_roles(&["planner", "builder"]);
    generate(&args(&root, &out), &spec).unwrap();
    let cert = onlyne_net::load_or_create(
        &onlyne_layout::ServerRoot::resolve(&root).key_path(),
        "test-cluster",
    )
    .unwrap();
    assert!(cert.spki_pin.starts_with("sha256/"));
    for role in ["planner", "builder"] {
        let config =
            fs::read_to_string(out.join("dev").join(role).join(".onlyne/config.toml")).unwrap();
        assert!(config.contains(&cert.spki_pin), "pin missing for {role}");
    }
    assert_eq!(
        fs::metadata(onlyne_layout::ServerRoot::resolve(&root).key_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn absolute_path_scan_removes_this_run_and_exits_4() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    let leaked = root.display().to_string();
    write_template(
        &root,
        "dev/planner",
        &[("AGENTS.md", &format!("leaked {leaked}"))],
    );
    let spec = spec_with_roles(&["planner"]);
    let err = generate(&args(&root, &out), &spec).expect_err("must scan");
    assert_eq!(err.exit_code(), 4);
    assert!(
        err.to_string()
            .starts_with("onlyne: generated workspace embeds absolute path ")
    );
    assert!(err.to_string().ends_with("dev/planner/AGENTS.md"));
    assert!(!out.exists());
}

#[test]
fn absolute_out_path_in_a_template_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    let leaked = out.display().to_string();
    write_template(
        &root,
        "dev/planner",
        &[("AGENTS.md", &format!("workspace at {leaked}"))],
    );
    let spec = spec_with_roles(&["planner"]);
    let err = generate(&args(&root, &out), &spec).expect_err("must scan");
    assert_eq!(err.exit_code(), 4);
    assert!(
        err.to_string()
            .starts_with("onlyne: generated workspace embeds absolute path ")
    );
    assert!(!out.exists());
}

#[test]
fn home_style_text_is_not_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(
        &root,
        "dev/planner",
        &[(
            "AGENTS.md",
            "Write notes under /Users/example/notes or /home/example/notes, or use ~/onlyne.",
        )],
    );
    let spec = spec_with_roles(&["planner"]);
    generate(&args(&root, &out), &spec).expect("home-style prose is legal template content");
    assert!(out.join("dev/planner/AGENTS.md").is_file());
}

#[test]
fn relocated_workspace_loads_without_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(
        &root,
        "dev/builder",
        &[("AGENTS.md", "b {{role}}"), (".pi/onlyne.json", "{}")],
    );
    let spec = spec_with_roles(&["builder"]);
    generate(&args(&root, &out), &spec).unwrap();
    let moved = tmp.path().join("elsewhere/b1");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    copy_dir(&out.join("dev/builder"), &moved);
    let config = onlyne_config::ClientConfig::load(moved.join(".onlyne/config.toml")).unwrap();
    assert_eq!(config.role, "builder");
    assert!(config.cert_pin.starts_with("sha256/"));
    assert_eq!(config.key_path, "keys/role.key");
    let needle = out.display().to_string();
    for path in walk(&moved) {
        if path.is_file() {
            let bytes = fs::read(&path).unwrap();
            assert!(
                !bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes()),
                "generated path survived relocation in {}",
                path.display()
            );
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_onlyne-server"))
}

fn run_cli(args: &[&str]) -> std::process::Output {
    std::process::Command::new(binary())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn init_writes_spec_prints_pin_and_refuses_second_run() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server");
    let output = run_cli(&[
        "init",
        "--root",
        root.to_str().unwrap(),
        "--listen",
        "127.0.0.1:7899",
    ]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].starts_with("sha256/"),
        "unexpected pin line: {stdout}"
    );
    let spec = fs::read_to_string(root.join(".onlyne/spec.toml")).unwrap();
    assert!(spec.contains("[server]"));
    assert!(spec.contains("listen = \"127.0.0.1:7899\""));
    assert!(spec.contains(&format!("cert_pin = \"{}\"", lines[0])));
    assert!(spec.contains("name = \"server\""));
    onlyne_config::Spec::load(root.join(".onlyne/spec.toml")).unwrap();
    let key = root.join(".onlyne/keys/server.key");
    assert_eq!(
        fs::metadata(&key).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let again = run_cli(&[
        "init",
        "--root",
        root.to_str().unwrap(),
        "--listen",
        "127.0.0.1:7899",
    ]);
    assert_eq!(again.status.code(), Some(4));
    let stderr = String::from_utf8(again.stderr).unwrap();
    assert_eq!(
        stderr.trim_end(),
        format!(
            "onlyne: workspace exists at {}; pass --force to overwrite",
            root.join(".onlyne/spec.toml").display()
        )
    );
}

#[test]
fn missing_listen_is_a_bad_argument() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server");
    let output = run_cli(&["init", "--root", root.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!root.join(".onlyne/spec.toml").exists());
}

#[test]
fn cli_generate_stdout_is_only_the_fragment() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server");
    let out = tmp.path().join("gen");
    let init = run_cli(&[
        "init",
        "--root",
        root.to_str().unwrap(),
        "--listen",
        "127.0.0.1:7899",
    ]);
    assert_eq!(init.status.code(), Some(0));
    let fragment = format!("\n[[client]]\nrole = \"planner\"\nkey = \"{ZERO_KEY}\"\n");
    fs::write(
        root.join(".onlyne/spec.toml"),
        fs::read_to_string(root.join(".onlyne/spec.toml")).unwrap() + &fragment,
    )
    .unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "p {{role}}")]);
    let output = run_cli(&[
        "generate",
        "--root",
        root.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().next().unwrap(), "[[client]]");
    assert!(stdout.contains("role = \"planner\""));
    assert!(stdout.contains("ed25519/"));
    assert!(!stdout.contains("onlyne-server:"));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("onlyne-server: generated 1 role(s) at"));
    assert!(out.join("dev/planner/AGENTS.md").is_file());
    let seeded = onlyne_config::Spec::load(root.join(".onlyne/spec.toml")).unwrap();
    assert_eq!(seeded.client.len(), 1);
    assert_eq!(seeded.client[0].role, "planner");
}

#[test]
fn appended_fragments_parse_for_two_roles() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("srv");
    let out = tmp.path().join("ws");
    fs::create_dir_all(&root).unwrap();
    write_template(&root, "dev/planner", &[("AGENTS.md", "p")]);
    write_template(&root, "dev/builder", &[("AGENTS.md", "b")]);
    let spec = spec_with_roles(&["planner", "builder"]);
    let report = generate(&args(&root, &out), &spec).unwrap();
    let mut text = String::from(
        "[server]\nname = \"test-cluster\"\nlisten = \"127.0.0.1:17811\"\ncert_pin = \"",
    );
    text.push_str(ZERO_PIN);
    text.push_str("\"\n\n");
    text.push_str(&report.fragment);
    let parsed = onlyne_config::Spec::parse_str(&text).unwrap();
    assert_eq!(parsed.client.len(), 2);
    assert!(parsed.client.iter().any(|row| row.role == "planner"));
    assert!(parsed.client.iter().any(|row| row.role == "builder"));
}

// ---------------------------------------------------------------------------
// Process verbs: start, stop, status. The real daemon is never spawned.
// ---------------------------------------------------------------------------

fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn init_root(root: &Path) {
    let output = run_cli(&[
        "init",
        "--root",
        root.to_str().unwrap(),
        "--listen",
        "127.0.0.1:7899",
    ]);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn start_refuses_when_a_live_pid_is_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server");
    init_root(&root);
    let pid = std::process::id();
    fs::write(root.join(".onlyne/run/server.pid"), format!("{pid}\n")).unwrap();
    let output = run_cli(&["start", "--root", root.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        format!("onlyne: server already running at pid {pid}")
    );
    assert_eq!(
        fs::read_to_string(root.join(".onlyne/run/server.pid")).unwrap(),
        format!("{pid}\n")
    );
}

#[test]
fn stop_without_a_pid_file_says_not_running() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server");
    init_root(&root);
    let output = run_cli(&["stop", "--root", root.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        "onlyne: server not running"
    );
    assert!(!root.join(".onlyne/run/server.pid").exists());
}

#[test]
fn stop_clears_a_stale_pid_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server");
    init_root(&root);
    let pid_path = root.join(".onlyne/run/server.pid");
    fs::write(&pid_path, format!("{}\n", dead_pid())).unwrap();
    let output = run_cli(&["stop", "--root", root.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        "onlyne: server not running"
    );
    assert!(!pid_path.exists(), "a stale pid file is removed");
}

#[test]
fn status_reports_a_stopped_server_as_json() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server");
    init_root(&root);
    fs::write(
        root.join(".onlyne/run/server.pid"),
        format!("{}\n", dead_pid()),
    )
    .unwrap();
    let output = run_cli(&["status", "--root", root.to_str().unwrap(), "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8(output.stdout).unwrap().trim()).unwrap();
    assert_eq!(report["running"], serde_json::Value::Bool(false));
    assert_eq!(report["pid"], serde_json::Value::Null);
    assert_eq!(report["socket_present"], serde_json::Value::Bool(false));
    assert_eq!(report["store_reachable"], serde_json::Value::Bool(false));
    assert!(report["spec_hash"].as_str().is_some());
    assert!(report["root"].as_str().unwrap().ends_with("server"));
    assert!(
        report["socket"]
            .as_str()
            .unwrap()
            .ends_with(".onlyne/run/s")
    );
}

#[test]
fn status_reports_a_reachable_store() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server");
    init_root(&root);
    onlyne_store::ServerLedger::open(root.join(".onlyne/state.db"), 14).unwrap();
    let output = run_cli(&["status", "--root", root.to_str().unwrap(), "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8(output.stdout).unwrap().trim()).unwrap();
    assert_eq!(report["store_reachable"], serde_json::Value::Bool(true));
    assert_eq!(report["running"], serde_json::Value::Bool(false));
}
