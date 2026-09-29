use onlyne_config::{
    AcpSection, BACKEND_IS_GONE, ClientConfig, DEFAULT_BACKOFF_MS, DEFAULT_FAULT_HISTORY_DAYS,
    DEFAULT_HEARTBEAT_GRACE_SECS, DEFAULT_HEARTBEAT_TIMEOUT_MS, DEFAULT_MAX_SESSIONS,
    DEFAULT_NOTE_QUEUE, DEFAULT_RECONNECT_GRACE_SECS, DEFAULT_REQUEUE_MAX_ATTEMPTS,
    DEFAULT_REQUEUE_TTL_SECS, DEFAULT_RESYNC_LAG, DEFAULT_STALE_WATCH_SECS,
    DEFAULT_STALL_REPORT_SECS, DEFAULT_TEMPLATE_ROOT, Drive, Env, IntentPolicy, Placement,
    RuntimeSection, Spec, SpecDiff, Timeouts, canonical_bytes, config_client_schema, redact,
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
stale_watch_secs = 45
heartbeat_grace_secs = 75
agent_package = ""
template_root = ".onlyne/templates"

[[client]]
role = "planner"
key = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
prose = """Read the incoming task, produce a concise completion reply, ..."""
admin = false
max_sessions = 3
allowed_senders = ["*"]
allowed_targets = ["builder", "reviewer"]
timeout = { ready_ms = 30000, idle_ms = 60000 }
intent = { attempts = 3, backoff_ms = [1000, 2000, 4000] }

[client.runtime]
drive = "plugin"
command = ["pi", "--session-id", "{session}"]

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
    assert_eq!(spec.server.stale_watch_secs, 45);
    assert_eq!(spec.server.heartbeat_grace_secs, 75);
    assert_eq!(spec.server.agent_package, "");
    assert_eq!(spec.server.template_root, DEFAULT_TEMPLATE_ROOT);
    assert_eq!(
        spec.server.requeue_max_attempts,
        DEFAULT_REQUEUE_MAX_ATTEMPTS
    );
    assert_eq!(spec.server.requeue_ttl_secs, DEFAULT_REQUEUE_TTL_SECS);

    let planner = &spec.client[0];
    assert_eq!(planner.role, "planner");
    assert_eq!(planner.key, KEY_A);
    assert_eq!(
        planner.prose,
        "Read the incoming task, produce a concise completion reply, ..."
    );
    assert!(!planner.admin);
    assert_eq!(planner.max_sessions, 3);
    assert_eq!(planner.allowed_senders, vec!["*"]);
    assert_eq!(planner.allowed_targets, vec!["builder", "reviewer"]);
    assert_eq!(planner.runtime.drive, Drive::Plugin);
    assert_eq!(
        planner.runtime.command,
        vec!["pi", "--session-id", "{session}"]
    );
    assert_eq!(planner.timeout.ready_ms, 30_000);
    assert_eq!(planner.timeout.idle_ms, 60_000);
    assert_eq!(planner.intent.attempts, 3);
    assert_eq!(planner.intent.backoff_ms, DEFAULT_BACKOFF_MS);
    assert_eq!(planner.aggregate, "");

    let supervisor = &spec.client[1];
    assert_eq!(supervisor.role, "_supervisor");
    assert!(!supervisor.admin);
    assert_eq!(supervisor.max_sessions, DEFAULT_MAX_SESSIONS);
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
    assert_eq!(spec.server.stale_watch_secs, DEFAULT_STALE_WATCH_SECS);
    assert_eq!(
        spec.server.heartbeat_grace_secs,
        DEFAULT_HEARTBEAT_GRACE_SECS
    );
    assert_eq!(spec.server.agent_package, "");
    assert_eq!(spec.server.template_root, ".onlyne/templates");
    assert_eq!(spec.server.requeue_max_attempts, 0);
    assert_eq!(spec.server.requeue_ttl_secs, 0);
    assert!(!spec.client[0].admin);
    assert_eq!(spec.client[0].max_sessions, 1);
    assert_eq!(spec.client[0].prose, "");
    assert_eq!(spec.client[0].aggregate, "");
    assert_eq!(spec.client[0].intent.attempts, 3);
    assert_eq!(spec.client[0].intent.backoff_ms, vec![1000, 2000, 4000]);
}
#[test]
fn client_stall_report_defaults_when_omitted() {
    let config = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap();
    assert_eq!(config.stall_report_secs, DEFAULT_STALL_REPORT_SECS);
    assert_eq!(config.stall_report_secs, 1800);

    let configured = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"
stall_report_secs = 0

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap();
    assert_eq!(configured.stall_report_secs, 0);
}

#[test]
fn client_placement_defaults_absent_and_reads_override() {
    let config = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap();
    // Absent means "probe the pane hosts and fall back to headless", which is
    // the placement `None` stands for: an empty string would be a value, and a
    // value nobody wrote must not read as one.
    assert_eq!(config.placement, None);

    let configured = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"
placement = "headless"

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap();
    assert_eq!(configured.placement, Some(Placement::Headless));
}

/// The fused key is refused in both files, with its own line and both
/// replacements named: a cluster that keeps running under a policy nobody set
/// is the failure this refusal exists to remove (`docs/v2-CONTRACT.md`
/// §"Slice 2").
#[test]
fn a_backend_key_is_refused_by_both_loaders_with_its_line() {
    let client = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"
backend = "acp"

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap_err();
    assert_eq!(
        client.to_string(),
        format!("config.toml:4: {BACKEND_IS_GONE}")
    );

    let spec = Spec::parse_str(&format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"
backend = "herdr"
"#
    ))
    .unwrap_err();
    assert_eq!(spec.to_string(), format!("spec.toml:9: {BACKEND_IS_GONE}"));
}

/// `[client.runtime]` is a table, so its keys land on the entry they were
/// written under rather than on the next one.
#[test]
fn a_runtime_table_lands_on_its_own_entry() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"

[client.runtime]
drive = "acp"
command = ["python3", "agent.py"]

[[client]]
role = "builder"
key = "{KEY_A}"
"#
    );
    let spec = Spec::parse_str(&text).expect("both entries parse");
    assert_eq!(spec.client[0].runtime.drive, Drive::Acp);
    assert_eq!(spec.client[0].runtime.command, ["python3", "agent.py"]);
    // The second entry carries no table, so it keeps the default drive.
    assert_eq!(spec.client[1].runtime, RuntimeSection::default());
}

#[test]
fn an_unknown_spec_key_loads_and_is_named() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"
extra = true
"#
    );
    let spec = Spec::parse_str(&text).expect("a key no field declares must not refuse the start");
    assert_eq!(spec.server.name, "cluster-a");
    assert_eq!(
        onlyne_config::keys::unknown_spec_keys(&text),
        Ok(vec!["server.extra".to_string()]),
        "the ignored key still has to reach the operator"
    );
}

/// The three removed spellings are each refused by name. The loader's refusal
/// names the key, so an operator editing a spec that still carries one learns
/// which key to delete and what replaces it.
#[test]
fn relay_required_is_refused_by_name() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"
relay_required = ["writer"]
"#
    );
    let err = Spec::parse_str(&text).expect_err("relay_required must be refused");
    assert!(err.to_string().contains("relay_required"), "{err}");
    assert!(err.to_string().contains("allowed_targets"), "{err}");
}

#[test]
fn relay_required_count_is_refused_by_name() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"
relay_required_count = 2
"#
    );
    let err = Spec::parse_str(&text).expect_err("relay_required_count must be refused");
    assert!(err.to_string().contains("relay_required_count"), "{err}");
    assert!(err.to_string().contains("allowed_targets"), "{err}");
}

#[test]
fn relay_count_is_refused_by_name() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"
relay_count = 2
"#
    );
    let err = Spec::parse_str(&text).expect_err("relay_count must be refused");
    assert!(err.to_string().contains("relay_count"), "{err}");
    assert!(err.to_string().contains("allowed_targets"), "{err}");
}

/// A spec whose single `[[hook]]` is written from the three lines as given, so
/// a case can point at the line its own column lands on (7, 8, and 9).
fn hook_spec(on: &str, run: &str, timeout: &str) -> String {
    format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[hook]]
{on}
{run}
{timeout}
"#
    )
}

/// The declaration the plan writes reads back field for field
/// (`docs/v2-PLAN.md` §"事件钩子", `docs/v2-CONTRACT.md` §"Slice 7").
#[test]
fn a_hook_declaration_reads_on_run_and_timeout() {
    let spec = Spec::parse_str(&hook_spec(
        r#"on = ["delivery_blocked", "turn_end_without_complete"]"#,
        r#"run = ["./hooks/notify-supervisor.sh"]"#,
        r#"timeout = "10s""#,
    ))
    .expect("the plan's own declaration parses");
    assert_eq!(spec.hook.len(), 1, "{spec:?}");
    assert_eq!(
        spec.hook[0].on,
        ["delivery_blocked", "turn_end_without_complete"]
    );
    assert_eq!(spec.hook[0].run, ["./hooks/notify-supervisor.sh"]);
    assert_eq!(spec.hook[0].timeout, "10s");
}

/// A class outside the closed set is refused by name with its line, like any
/// other key the spec declares: a hook bound to a spelling nobody publishes is
/// an operator policy that silently never fires (`docs/v2-CONTRACT.md`
/// §"Slice 7").
#[test]
fn a_hook_bound_to_an_unknown_class_is_refused_by_name() {
    let text = hook_spec(
        r#"on = ["delivery_blocked", "delivery_blocked_typo"]"#,
        r#"run = ["./hooks/notify-supervisor.sh"]"#,
        r#"timeout = "10s""#,
    );
    let err = Spec::parse_str(&text).expect_err("a class outside the set is refused");
    assert!(
        err.to_string().starts_with("spec.toml:7:"),
        "the refusal points at the line that carries `on`: {err}"
    );
    assert!(err.to_string().contains("delivery_blocked_typo"), "{err}");
    assert!(
        err.to_string().contains("delivery_blocked") && err.to_string().contains("handoff"),
        "the set it accepts comes with the refusal: {err}"
    );
}

/// The other two columns refuse the same way, each on its own line: a command
/// that names nothing cannot be spawned, and a bound that is not a duration
/// leaves a hook with no bound at all (`docs/v2-CONTRACT.md` §"Slice 7").
#[test]
fn an_empty_run_or_an_unparseable_timeout_is_refused_with_its_line() {
    let empty_run = hook_spec(r#"on = ["fault"]"#, "run = []", r#"timeout = "10s""#);
    let err = Spec::parse_str(&empty_run).expect_err("a hook with no command is refused");
    assert!(err.to_string().starts_with("spec.toml:8:"), "{err}");
    assert!(err.to_string().contains("must name a command"), "{err}");

    let bad_timeout = hook_spec(
        r#"on = ["fault"]"#,
        r#"run = ["./hooks/notify.sh"]"#,
        r#"timeout = "10 seconds""#,
    );
    let err = Spec::parse_str(&bad_timeout).expect_err("an unparseable bound is refused");
    assert!(err.to_string().starts_with("spec.toml:9:"), "{err}");
    assert!(err.to_string().contains("10 seconds"), "{err}");
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
fn an_unknown_client_key_loads_and_is_named() {
    let text = r#"role = "planner"
cert_pin = "sha256/abcdef"
key_path = "keys/role.key"
plugins = []
extra = true

[server]
host = "127.0.0.1"
port = 7811
"#;
    let config =
        ClientConfig::parse_str(text).expect("a key no field declares must not refuse the start");
    assert_eq!(config.role, "planner");
    assert_eq!(
        onlyne_config::keys::unknown_client_keys(text),
        Ok(vec!["extra".to_string()])
    );
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
fn a_runtime_command_placeholder_is_refused_by_name_and_line() {
    let text = format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"

[client.runtime]
drive = "plugin"
command = ["pi", "{{unknown}}"]
"#
    );
    let err = Spec::parse_str(&text).unwrap_err();
    assert_eq!(
        err.to_string(),
        "spec.toml:12: role planner has unknown runtime command placeholder {unknown}"
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

/// One declaration is the whole policy: the edges a role may address are the
/// edges it owes, and a spec that names no relay key serializes no relay key.
///
/// The two halves are read off the same list, so there is nothing left to
/// disagree with: `allowed_targets` is both the server's ACL and the client's
/// completion guard, and a role that owes nothing leaves it empty.
#[test]
fn allowed_targets_is_the_whole_policy_and_absence_stays_off() {
    let spec = Spec::parse_str(&format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"

[[client]]
role = "planner"
key = "{KEY_A}"
allowed_targets = ["writer", "auditor"]

[[client]]
role = "sweeper"
key = "{KEY_C}"
"#
    ))
    .unwrap();

    assert_eq!(
        spec.client[0].allowed_targets,
        vec!["writer".to_string(), "auditor".to_string()]
    );
    // The role that names no target owes nothing, and that is the whole of an
    // empty policy: the default list is empty, not a separate "no guard" flag.
    assert!(spec.client[1].allowed_targets.is_empty());

    // A spec that never names a relay key serializes none, so its canonical
    // bytes — and with them its hash — are the ones the box already carried.
    let plain = Spec::parse_str(&format!(
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
    let value = toml::Value::try_from(&plain).unwrap();
    let bytes = String::from_utf8(canonical_bytes(&value)).unwrap();
    assert!(!bytes.contains("relay"), "{bytes}");
}

#[test]
fn spec_diff_reports_changed_requeue_gates() {
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
    let after = Spec::parse_str(&format!(
        r#"[server]
name = "cluster-a"
listen = "0.0.0.0:7811"
cert_pin = "{CERT_HEX}"
requeue_max_attempts = 2
requeue_ttl_secs = 3600

[[client]]
role = "planner"
key = "{KEY_A}"
"#
    ))
    .unwrap();
    let diff = SpecDiff::between(&before, &after);
    assert_eq!(
        diff.changed_server_fields,
        vec!["requeue_max_attempts", "requeue_ttl_secs"]
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
fn the_published_client_schema_carries_stall_report_secs() {
    let schema: serde_json::Value = serde_json::from_str(config_client_schema()).unwrap();
    assert_eq!(schema["properties"]["stall_report_secs"]["default"], 1800);
    assert_eq!(
        schema["properties"]["stall_report_secs"]["format"],
        "uint64"
    );
}

#[test]
fn the_published_client_schema_carries_reconnect_grace_secs() {
    let schema: serde_json::Value = serde_json::from_str(config_client_schema()).unwrap();
    assert_eq!(
        schema["properties"]["reconnect_grace_secs"]["default"], DEFAULT_RECONNECT_GRACE_SECS,
        "the published schema must state the parser's own default"
    );
    assert_eq!(
        schema["properties"]["reconnect_grace_secs"]["format"],
        "uint64"
    );
}

#[test]
fn the_published_client_schema_carries_placement_and_drops_backend() {
    let schema: serde_json::Value = serde_json::from_str(config_client_schema()).unwrap();
    assert!(
        schema["properties"].get("backend").is_none(),
        "the fused key must not be documented anywhere: the loader refuses it by name"
    );
    assert_eq!(
        schema["properties"]["placement"]["default"],
        serde_json::Value::Null
    );
    let names: Vec<&str> = schema["definitions"]["Placement"]["oneOf"]
        .as_array()
        .expect("the placement enum renders one arm per value")
        .iter()
        .filter_map(|arm| arm["enum"][0].as_str())
        .collect();
    assert_eq!(names, ["herdr", "orca", "zellij", "headless", "external"]);
    // The ignored-key report reads these names, so a key the schema does not
    // name is one the loader calls unknown.
    assert_eq!(
        onlyne_config::keys::unknown_client_keys("placement = \"headless\"\n"),
        Ok(vec![]),
        "the key the loader accepts is named by the published schema"
    );
}

#[test]
fn the_published_client_schema_carries_acp() {
    let schema: serde_json::Value = serde_json::from_str(config_client_schema()).unwrap();
    assert_eq!(
        schema["properties"]["acp"]["default"],
        serde_json::json!({
            "mode": "",
            "model": "",
            "reasoning_effort": "",
            "permission": "deny",
        })
    );
    let section = &schema["definitions"]["AcpSection"];
    assert_eq!(section["properties"]["mode"]["default"], "");
    assert_eq!(section["properties"]["permission"]["default"], "deny");
    assert!(
        section.get("additionalProperties").is_none(),
        "an `[acp]` key the struct does not declare is ignored at load, so the \
         published schema says nothing about extras: {section}"
    );
    assert!(
        section.get("required").is_none(),
        "every `[acp]` key is optional, so a partial table validates: {section}"
    );
}

/// `schema/spec.schema.json` is regenerated by hand (`config-schema`), and the
/// ignored-key report reads its property names. A property that survives the
/// slice without the regeneration is a key the loader would still call known.
///
/// The three removed spellings have no property left, so the schema cannot
/// teach an editor a key the loader refuses.
#[test]
fn the_published_spec_schema_drops_the_relay_keys() {
    let schema: serde_json::Value = serde_json::from_str(onlyne_config::spec_schema()).unwrap();
    let entry = &schema["definitions"]["ClientEntry"];
    assert_eq!(
        entry["required"],
        serde_json::json!(["key", "role"]),
        "one list is the whole policy, and it stays optional"
    );
    let properties = entry["properties"]
        .as_object()
        .expect("the entry's properties");
    for key in ["relay_required", "relay_count", "relay_required_count"] {
        assert!(
            !properties.contains_key(key),
            "{key} is gone and must not survive in the published schema: {properties:?}"
        );
    }
    assert_eq!(
        entry["properties"]["allowed_targets"]["items"]["type"], "string",
        "the one declaration is the list that stayed"
    );
}

#[test]
fn the_published_spec_schema_carries_the_requeue_keys() {
    let schema: serde_json::Value = serde_json::from_str(onlyne_config::spec_schema()).unwrap();
    let server = &schema["definitions"]["ServerSection"];
    assert_eq!(server["properties"]["requeue_max_attempts"]["default"], 0);
    assert_eq!(
        server["properties"]["requeue_max_attempts"]["format"],
        "uint32"
    );
    assert_eq!(server["properties"]["requeue_ttl_secs"]["default"], 0);
    assert_eq!(server["properties"]["requeue_ttl_secs"]["format"], "uint64");
    let required = server["required"].as_array().expect("required keys");
    assert!(
        !required
            .iter()
            .any(|key| key == "requeue_max_attempts" || key == "requeue_ttl_secs"),
        "the requeue keys stay optional so an old spec still validates: {required:?}"
    );
}

#[test]
fn orca_worktree_defaults_to_the_host_and_reads_a_selector() {
    let base = r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"

[server]
host = "127.0.0.1"
port = 7811
"#;
    assert_eq!(ClientConfig::parse_str(base).unwrap().orca.worktree, "host");
    assert_eq!(
        ClientConfig::parse_str(&format!("{base}\n[orca]\nworktree = \"inherit\"\n"))
            .unwrap()
            .orca
            .worktree,
        "inherit"
    );
    assert_eq!(
        ClientConfig::parse_str(&format!("{base}\n[orca]\nworktree = \"id:folder:abc\"\n"))
            .unwrap()
            .orca
            .worktree,
        "id:folder:abc"
    );
    // A key written in the wrong table now loads, and the report names it where
    // it sits: this line lands inside `[server]`, which declares no `worktree`.
    let misplaced = format!("{base}\nworktree = \"host\"\n");
    let config = ClientConfig::parse_str(&misplaced).expect("a foreign key must not refuse it");
    assert_eq!(config.orca.worktree, "host", "the default stands");
    assert_eq!(
        onlyne_config::keys::unknown_client_keys(&misplaced),
        Ok(vec!["server.worktree".to_string()]),
        "the ignored key still reaches the log"
    );
    let inside = format!("{base}\n[orca]\nworktree = \"host\"\nextra = 1\n");
    assert_eq!(
        ClientConfig::parse_str(&inside)
            .expect("[orca] ignores a foreign key")
            .orca
            .worktree,
        "host"
    );
    assert_eq!(
        onlyne_config::keys::unknown_client_keys(&inside),
        Ok(vec!["orca.extra".to_string()])
    );
}

#[test]
fn acp_section_parses_and_resolves_the_four_knobs() {
    let base = r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"

[server]
host = "127.0.0.1"
port = 7811
"#;
    let config = ClientConfig::parse_str(&format!(
        "{base}\n[acp]\nmode = \"acceptEdits\"\nmodel = \"qwen3-coder\"\nreasoning_effort = \"high\"\npermission = \"allow\"\n"
    ))
    .unwrap();
    assert_eq!(config.acp.mode, "acceptEdits");
    assert_eq!(config.acp.model, "qwen3-coder");
    assert_eq!(config.acp.reasoning_effort, "high");
    assert_eq!(config.acp.permission, "allow");
    let mut resolved = config.clone();
    resolved.resolve_secrets(&Env::default()).unwrap();
    assert_eq!(resolved.acp, config.acp);
}

#[test]
fn acp_defaults_to_deny_with_empty_agent_knobs_when_the_table_is_absent() {
    let config = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap();
    assert_eq!(config.acp.mode, "");
    assert_eq!(config.acp.model, "");
    assert_eq!(config.acp.reasoning_effort, "");
    assert_eq!(config.acp.permission, "deny");
    let mut resolved = config.clone();
    resolved.resolve_secrets(&Env::default()).unwrap();
    assert_eq!(resolved.acp, AcpSection::default());
}

#[test]
fn acp_permission_rejects_a_value_outside_deny_and_allow() {
    let base = r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"

[server]
host = "127.0.0.1"
port = 7811
"#;
    let err = ClientConfig::parse_str(&format!(
        "{base}\n[acp]\nmode = \"\"\nmodel = \"\"\nreasoning_effort = \"\"\npermission = \"nonsense\"\n"
    ))
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "config.toml:13: acp.permission must be `deny` or `allow`, got `nonsense`"
    );

    let inline = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"
acp = { mode = "", model = "", reasoning_effort = "", permission = "nonsense" }

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap_err();
    assert_eq!(
        inline.to_string(),
        "config.toml:4: acp.permission must be `deny` or `allow`, got `nonsense`"
    );
}

#[test]
fn acp_partial_table_keeps_deny_and_empty_agent_knobs() {
    let config = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"

[server]
host = "127.0.0.1"
port = 7811

[acp]
mode = "yolo"
"#,
    )
    .unwrap();
    assert_eq!(config.acp.mode, "yolo");
    assert_eq!(config.acp.model, "");
    assert_eq!(config.acp.reasoning_effort, "");
    assert_eq!(config.acp.permission, "deny");
    let mut resolved = config.clone();
    resolved.resolve_secrets(&Env::default()).unwrap();
    assert_eq!(resolved.acp, config.acp);
}
