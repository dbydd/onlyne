use onlyne_config::template::{
    Placeholders, Template, TemplateError, discover, load_tree, local_override, merge_fragment,
    scan_for_prefixes, substitute, substitute_at,
};
use std::borrow::Cow;
use std::fs;
use std::path::Path;

fn make_template(root: &Path, relative: &str) -> Template {
    let role_dir = root.join(relative);
    fs::create_dir_all(&role_dir).unwrap();
    let topology = role_dir
        .parent()
        .unwrap()
        .strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    Template {
        role_dir,
        topology,
        relative: relative.to_string(),
    }
}

fn placeholders(agent_package: Option<&str>) -> Placeholders {
    Placeholders {
        role: "planner".to_string(),
        cluster: "cluster-a".to_string(),
        server_name: "server-a".to_string(),
        listen: "127.0.0.1:7811".to_string(),
        cert_pin: "sha256/pin".to_string(),
        admin: "false".to_string(),
        max_sessions: "3".to_string(),
        agent_package: agent_package.map(str::to_string),
    }
}

#[test]
fn discovery_finds_role_and_topology() {
    let tmp = tempfile::tempdir().unwrap();
    let template = make_template(tmp.path(), "dev/planner");
    let found = discover(tmp.path(), "planner").unwrap();
    assert_eq!(found, vec![template]);
    assert_eq!(found[0].topology, "dev");
    assert_eq!(found[0].relative, "dev/planner");
}

#[test]
fn discovery_reports_sorted_ambiguity() {
    let tmp = tempfile::tempdir().unwrap();
    make_template(tmp.path(), "ops/planner");
    make_template(tmp.path(), "dev/planner");
    let error = discover(tmp.path(), "planner").unwrap_err();
    assert_eq!(
        error.to_string(),
        "onlyne: template for role planner is ambiguous: dev/planner, ops/planner"
    );
    assert_eq!(error.exit_code(), 4);
}

#[test]
fn discovery_reports_no_match() {
    let tmp = tempfile::tempdir().unwrap();
    let error = discover(tmp.path(), "planner").unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "onlyne: no template directory named planner under {}",
            tmp.path().display()
        )
    );
    assert!(matches!(error, TemplateError::NoTemplate { .. }));
}

#[test]
fn no_role_matches_message_is_exact() {
    let error = TemplateError::NoRoleMatches;
    assert_eq!(
        error.to_string(),
        "onlyne: no role matches the requested templates/roles"
    );
    assert_eq!(error.exit_code(), 4);
}

#[test]
fn merge_fragment_is_deep_and_derived_values_win() {
    let derived: toml::Value = r#"name = "derived"
[model]
provider = "derived-provider"
new = "derived-new"
[nested]
value = 7
"#
    .parse()
    .unwrap();
    let override_: toml::Value = r#"name = "override"
[model]
provider = "override-provider"
extra = "override-extra"
[nested]
value = 99
other = true
"#
    .parse()
    .unwrap();
    let merged = merge_fragment(&derived, &override_);
    assert_eq!(merged["name"].as_str(), Some("derived"));
    assert_eq!(
        merged["model"]["provider"].as_str(),
        Some("derived-provider")
    );
    assert_eq!(merged["model"]["new"].as_str(), Some("derived-new"));
    assert_eq!(merged["model"]["extra"].as_str(), Some("override-extra"));
    assert_eq!(merged["nested"]["value"].as_integer(), Some(7));
    assert_eq!(merged["nested"]["other"].as_bool(), Some(true));

    let derived_scalar: toml::Value = "value = 1".parse().unwrap();
    let override_scalar: toml::Value = "value = 2".parse().unwrap();
    assert_eq!(
        merge_fragment(&derived_scalar, &override_scalar)["value"].as_integer(),
        Some(1)
    );
}

#[test]
fn load_tree_prunes_dot_directories_and_separates_override() {
    let tmp = tempfile::tempdir().unwrap();
    let template = make_template(tmp.path(), "dev/planner");
    fs::write(template.role_dir.join("AGENTS.md"), b"instructions").unwrap();
    fs::create_dir_all(template.role_dir.join(".pi")).unwrap();
    fs::write(template.role_dir.join(".pi/settings.json"), b"hidden").unwrap();
    fs::create_dir_all(template.role_dir.join(".onlyne")).unwrap();
    fs::write(
        template.role_dir.join(".onlyne/config.toml"),
        b"[local]\nvalue = true\n",
    )
    .unwrap();
    let files = load_tree(&template).unwrap();
    assert_eq!(
        files,
        vec![("AGENTS.md".to_string(), b"instructions".to_vec())]
    );
    let override_ = local_override(&template).unwrap();
    assert_eq!(override_["local"]["value"].as_bool(), Some(true));
}

#[test]
fn substitute_replaces_all_eight_keys() {
    let input = b"{{role}}|{{cluster}}|{{server_name}}|{{listen}}|{{cert_pin}}|{{admin}}|{{max_sessions}}|{{agent_package}}";
    let actual = substitute(input, &placeholders(Some("/tmp/plugin"))).unwrap();
    assert_eq!(
        actual.as_ref(),
        b"planner|cluster-a|server-a|127.0.0.1:7811|sha256/pin|false|3|/tmp/plugin"
    );
}

#[test]
fn unknown_placeholder_names_key_and_path() {
    let error = substitute_at(
        b"hello {{wat}}",
        &placeholders(None),
        "dev/planner/AGENTS.md",
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "onlyne: unknown placeholder {{wat}} in dev/planner/AGENTS.md"
    );
}

#[test]
fn unset_agent_package_is_rejected() {
    let error = substitute(b"{{agent_package}}", &placeholders(None)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "onlyne: agent_package not set in spec.toml [server]"
    );
}

#[test]
fn binary_files_pass_through_unchanged() {
    let binary = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0xff, 0x00];
    let output = substitute(&binary, &placeholders(Some("plugin"))).unwrap();
    assert!(matches!(output, Cow::Borrowed(_)));
    assert_eq!(output.as_ref(), binary.as_slice());
}

#[test]
fn prefix_scan_reports_first_hit_and_clears() {
    let files = vec![
        ("AGENTS.md".to_string(), b"safe text".to_vec()),
        (
            "settings.json".to_string(),
            b"/absolute/server/root".to_vec(),
        ),
    ];
    let prefixes = vec!["/absolute/server".to_string(), "/other".to_string()];
    let error = scan_for_prefixes(&files, &prefixes).unwrap_err();
    assert_eq!(
        error.to_string(),
        "onlyne: generated workspace embeds absolute path settings.json"
    );
    assert!(scan_for_prefixes(&files[..1], &prefixes).is_ok());
}
