use onlyne_config::{
    ClientConfig, DEFAULT_BACKOFF_MS, DEFAULT_FAULT_HISTORY_DAYS, DEFAULT_HEARTBEAT_TIMEOUT_MS,
    DEFAULT_MAX_SESSIONS, DEFAULT_NOTE_QUEUE, DEFAULT_RESYNC_LAG, DEFAULT_TEMPLATE_ROOT, Env,
    IntentPolicy, Spec, SpecDiff, Timeouts, canonical_bytes, config_client_schema, redact,
    spec_hash,
};
use std::fs;

const KEY_A: &str = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const KEY_B: &str = "ed25519/AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
const KEY_C: &str = "ed25519/AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";
const CERT_HEX: &str = "sha256/0000000000000000000000000000000000000000000000000000000000000000";

const SAMPLE_SPEC: &str = r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
note_queue = false
fault_history_days = 14
resync_lag = 256
heartbeat_timeout_ms = 30000
agent_package = ""
template_root = ".onlyne/templates"

[[client]]
role = "planner"
key = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
prose = """Read the incoming task, produce a concise completion reply, ..."""
admin = false
max_sessions = 3
reuse = true
allowed_senders = ["*"]
allowed_targets = ["builder", "reviewer"]
session_command = ["pi", "--session-id", "{session}"]
timeout = { ready_ms = 30000, running_ms = 120000, idle_ms = 60000 }
intent = { attempts = 3, backoff_ms = [1000, 2000, 4000] }

[[client]]
role = "_supervisor"
key = "ed25519/AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE="
aggregate = "cluster-b"
allowed_senders = ["*"]
allowed_targets = ["_supervisor"]

[[gateway]]
id = "tg1"
platform = "telegram"
key = "ed25519/AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI="
enabled = true

[[route]]
gateway = "tg1"
channel = "telegram"
conversation = "1234"
to = { role = "planner" }

[[route]]
gateway = "tg1"
channel = "telegram"
to = { role = "_fallback" }
"#;

#[test]
fn sample_spec_parses_and_defaults_are_asserted() {
    let spec = Spec::parse_str(SAMPLE_SPEC).unwrap();
    assert_eq!(spec.server.name, "cluster-a");
    assert_eq!(spec.server.listen, "0.0.0.0:7811");
    assert_eq!(spec.server.cert_pin, CERT_HEX);
    assert_eq!(spec.server.note_queue, DEFAULT_NOTE_QUEUE);
    assert_eq!(spec.server.fault_history_days, DEFAULT_FAULT_HISTORY_DAYS);
    assert_eq!(spec.server.resync_lag, DEFAULT_RESYNC_LAG);
    assert_eq!(
        spec.server.heartbeat_timeout_ms,
        DEFAULT_HEARTBEAT_TIMEOUT_MS
    );
    assert_eq!(spec.server.agent_package, "");
    assert_eq!(spec.server.template_root, DEFAULT_TEMPLATE_ROOT);

    let planner = &spec.client[0];
    assert_eq!(planner.role, "planner");
    assert_eq!(planner.key, KEY_A);
    assert_eq!(
        planner.prose,
        "Read the incoming task, produce a concise completion reply, ..."
    );
    assert!(!planner.admin);
    assert_eq!(planner.max_sessions, 3);
    assert!(planner.reuse);
    assert_eq!(planner.allowed_senders, vec!["*"]);
    assert_eq!(planner.allowed_targets, vec!["builder", "reviewer"]);
    assert_eq!(
        planner.session_command,
        vec!["pi", "--session-id", "{session}"]
    );
    assert_eq!(planner.timeout.ready_ms, 30_000);
    assert_eq!(planner.timeout.running_ms, 120_000);
    assert_eq!(planner.timeout.idle_ms, 60_000);
    assert_eq!(planner.intent.attempts, 3);
    assert_eq!(planner.intent.backoff_ms, DEFAULT_BACKOFF_MS);
    assert_eq!(planner.aggregate, "");

    let supervisor = &spec.client[1];
    assert_eq!(supervisor.role, "_supervisor");
    assert!(!supervisor.admin);
    assert_eq!(supervisor.max_sessions, DEFAULT_MAX_SESSIONS);
    assert!(!supervisor.reuse);
    assert_eq!(supervisor.prose, "");
    assert_eq!(supervisor.aggregate, "cluster-b");
    assert_eq!(supervisor.timeout, Timeouts::default());
    assert_eq!(supervisor.intent, IntentPolicy::default());

    assert_eq!(spec.gateway[0].id, "tg1");
    assert_eq!(spec.gateway[0].platform, "telegram");
    assert_eq!(spec.gateway[0].key, KEY_C);
    assert!(spec.gateway[0].enabled);

    assert_eq!(spec.route[0].gateway, "tg1");
    assert_eq!(spec.route[0].conversation.as_deref(), Some("1234"));
    assert_eq!(spec.route[0].to.role, "planner");
    assert_eq!(spec.route[0].to.session, None);
    assert_eq!(spec.route[1].to.role, "_fallback");
}

#[test]
fn omitted_defaults_match_v1_contract() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"
"#
    );
    let spec = Spec::parse_str(&text).unwrap();
    assert!(!spec.server.note_queue);
    assert_eq!(spec.server.fault_history_days, 14);
    assert_eq!(spec.server.resync_lag, 256);
    assert_eq!(spec.server.heartbeat_timeout_ms, 30_000);
    assert_eq!(spec.server.agent_package, "");
    assert_eq!(spec.server.template_root, ".onlyne/templates");
    assert!(!spec.client[0].admin);
    assert_eq!(spec.client[0].max_sessions, 1);
    assert!(!spec.client[0].reuse);
    assert_eq!(spec.client[0].prose, "");
    assert_eq!(spec.client[0].aggregate, "");
    assert_eq!(spec.client[0].intent.attempts, 3);
    assert_eq!(spec.client[0].intent.backoff_ms, vec![1000, 2000, 4000]);
}

#[test]
fn unknown_key_error_has_line_number() {
    let err = Spec::parse_str(&format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"
extra = true
"#
    ))
    .unwrap_err();
    assert!(err.to_string().starts_with("spec.toml:5:"), "{err}");
}

#[test]
fn type_error_has_line_number() {
    let err = Spec::parse_str(&format!(
        r#"[server]
name = "cluster-a"
listen = 7811
cert_pin = "{CERT_HEX}"
"#
    ))
    .unwrap_err();
    assert!(err.to_string().starts_with("spec.toml:3:"), "{err}");
}
#[test]
fn missing_server_name_points_at_server_table() {
    let err = Spec::parse_str(
        r#"[server]
listen = "0.0.0.0:7811"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
"#,
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "spec.toml:1: missing field `name`");
}

#[test]
fn missing_client_key_points_at_client_table() {
    let err = Spec::parse_str(&format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
"#
    ))
    .unwrap_err();
    assert_eq!(err.to_string(), "spec.toml:6: missing field `key`");
}

#[test]
fn missing_route_target_points_at_route_table() {
    let err = Spec::parse_str(&format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[route]]
gateway = "tg1"
channel = "telegram"
"#
    ))
    .unwrap_err();
    assert_eq!(err.to_string(), "spec.toml:6: missing field `to`");
}

#[test]
fn client_config_unknown_key_has_line_number() {
    let err = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "sha256/abcdef"
key_path = "keys/role.key"
plugins = []
extra = true

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap_err();
    assert!(err.to_string().starts_with("config.toml:5:"), "{err}");
}

#[test]
fn duplicate_role_is_caught_and_names_both_lines() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"

[[client]]
role = "planner"
key = "{KEY_B}"
"#
    );
    let err = Spec::parse_str(&text).unwrap_err();
    assert_eq!(
        err.to_string(),
        "spec.toml:11: duplicate client role `planner` at lines 7 and 11"
    );
}

#[test]
fn duplicate_gateway_id_is_caught_and_names_both_lines() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[gateway]]
id = "tg1"
platform = "telegram"
key = "{KEY_A}"

[[gateway]]
id = "tg1"
platform = "telegram"
key = "{KEY_B}"
"#
    );
    let err = Spec::parse_str(&text).unwrap_err();
    assert_eq!(
        err.to_string(),
        "spec.toml:12: duplicate gateway id `tg1` at lines 7 and 12"
    );
}

#[test]
fn key_length_validation_names_role() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "ed25519/AAA="
"#
    );
    let err = Spec::parse_str(&text).unwrap_err();
    assert_eq!(
        err.to_string(),
        "spec.toml:8: role planner has invalid key: decoded key must be 32 bytes, got 2"
    );
}

#[test]
fn session_command_placeholder_validation_names_unknown_placeholder() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"
session_command = ["pi", "{{unknown}}"]
"#
    );
    let err = Spec::parse_str(&text).unwrap_err();
    assert_eq!(
        err.to_string(),
        "spec.toml:9: role planner has unknown session_command placeholder {unknown}"
    );
}

#[test]
fn spec_hash_is_stable_across_reordered_semantic_content() {
    let a = r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
"#;
    let b = r#"[server]
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
listen = "0.0.0.0:7811"
name = "cluster-a"
"#;
    let value_a: toml::Value = a.parse().unwrap();
    let value_b: toml::Value = b.parse().unwrap();
    assert_eq!(
        spec_hash(&canonical_bytes(&value_a)),
        spec_hash(&canonical_bytes(&value_b))
    );
}

#[test]
fn env_secret_resolution_set_and_unset() {
    let env = Env::from_vars([("ONLYNE_CERT", CERT_HEX), ("ONLYNE_KEY", "keys/role.key")]);
    let mut config = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "$ONLYNE_CERT"
key_path = "$ONLYNE_KEY"
plugins = ["pi"]

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap();
    config.resolve_secrets(&env).unwrap();
    assert_eq!(config.cert_pin, CERT_HEX);
    assert_eq!(config.key_path, "keys/role.key");

    let mut missing = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "$MISSING_CERT"
key_path = "keys/role.key"

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap();
    let err = missing
        .resolve_secrets(&Env::from_vars(std::iter::empty::<(&str, &str)>()))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "missing secret $MISSING_CERT for cert_pin; set the environment variable"
    );
}

#[test]
fn spec_diff_render_added_role_and_changed_allowed_targets() {
    let before = Spec::parse_str(&format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"
allowed_targets = ["builder"]
"#
    ))
    .unwrap();
    let after = Spec::parse_str(&format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"
allowed_targets = ["builder", "reviewer"]

[[client]]
role = "reviewer"
key = "{KEY_B}"
allowed_targets = ["planner"]
"#
    ))
    .unwrap();
    let diff = SpecDiff::between(&before, &after);
    assert_eq!(diff.added_roles, vec!["reviewer"]);
    assert_eq!(diff.changed_roles[0].role, "planner");
    assert_eq!(
        diff.changed_roles[0].changed_fields,
        vec!["allowed_targets"]
    );
    assert_eq!(
        diff.render(),
        "add role reviewer\nchange role planner: allowed_targets"
    );
}

#[test]
fn load_validate_reads_next_spec_and_renders_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("spec.toml");
    let before = Spec::parse_str(&format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"
"#
    ))
    .unwrap();
    fs::write(
        &path,
        format!(
            r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"

[[route]]
gateway = "tg1"
channel = "telegram"
to = {{ role = "planner" }}
"#
        ),
    )
    .unwrap();
    assert_eq!(
        before.load_validate(&path).unwrap().render(),
        "add route tg1/telegram/*/planner/-"
    );
}

#[test]
fn redaction_covers_key_cert_pin_and_resolved_secret() {
    let spec = Spec::parse_str(SAMPLE_SPEC).unwrap();
    let rendered = redact::spec(&spec);
    assert!(rendered.contains("ed2551…"));
    assert!(rendered.contains("sha256…"));
    assert!(!rendered.contains(&KEY_A[6..]));
    assert!(!rendered.contains(&CERT_HEX[6..]));

    let parsed: toml::Value = r#"key = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
cert_pin = "$ONLYNE_CERT"
plain = "visible"
"#
    .parse()
    .unwrap();
    let rendered_value = redact::value(&parsed);
    assert!(rendered_value.contains("ed2551…"));
    assert!(rendered_value.contains("$ONLYN…"));
    assert!(!rendered_value.contains("AAAAAAAAAAAAAAAA"));
}

#[test]
fn generated_schemas_parse_as_json() {
    serde_json::from_str::<serde_json::Value>(config_client_schema()).unwrap();
    serde_json::from_str::<serde_json::Value>(onlyne_config::spec_schema()).unwrap();
}
