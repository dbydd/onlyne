use std::time::Duration;

use onlyne_config::{
    ClientConfig, ClientSection, SessionPolicy, SessionScope, config_client_schema,
};

/// A workspace `config.toml` with no `[client.session]` table: eight lines, so
/// a refusal in an appended table lands on line 10.
const BASE_CLIENT: &str = r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"

[server]
host = "127.0.0.1"
port = 7811
"#;

/// A workspace written before v2 carries no `[client.session]` table at all,
/// and it must keep loading on the scope that absence means.
#[test]
fn an_absent_client_session_table_is_the_oneshot_default() {
    let config = ClientConfig::parse_str(BASE_CLIENT).unwrap();
    assert_eq!(config.client.session.scope, SessionScope::Oneshot);
    assert_eq!(config.client.session.idle_close, None);
    assert_eq!(config.client, ClientSection::default());
    assert_eq!(config.client.session, SessionPolicy::default());
    assert_eq!(
        onlyne_config::keys::unknown_client_keys(BASE_CLIENT),
        Ok(vec![])
    );
}

#[test]
fn each_scope_spelling_reads_its_own_session_scope() {
    for (written, scope) in [
        ("oneshot", SessionScope::Oneshot),
        ("task", SessionScope::Task),
        ("role", SessionScope::Role),
    ] {
        let text = format!("{BASE_CLIENT}\n[client.session]\nscope = \"{written}\"\n");
        assert_eq!(
            ClientConfig::parse_str(&text).unwrap().client.session.scope,
            scope,
            "`{written}` is one of the three scopes"
        );
    }
    // A partial table keeps the scope default rather than needing both keys.
    let partial = ClientConfig::parse_str(&format!(
        "{BASE_CLIENT}\n[client.session]\nidle_close = 30\n"
    ))
    .unwrap();
    assert_eq!(partial.client.session.scope, SessionScope::Oneshot);
    assert_eq!(
        partial.client.session.idle_close,
        Some(Duration::from_secs(30))
    );
}

#[test]
fn idle_close_reads_a_duration_or_bare_seconds() {
    for (written, secs) in [
        ("30s", 30_u64),
        ("5m", 300),
        ("2h", 7_200),
        ("1d", 86_400),
        ("90", 90),
    ] {
        let text = format!("{BASE_CLIENT}\n[client.session]\nidle_close = \"{written}\"\n");
        assert_eq!(
            ClientConfig::parse_str(&text)
                .unwrap()
                .client
                .session
                .idle_close,
            Some(Duration::from_secs(secs)),
            "`{written}` is a duration"
        );
    }
    // A bare integer is the same reading, and `0` is a real value rather than
    // an absent one: it means no idle close at all.
    let text = format!("{BASE_CLIENT}\n[client.session]\nidle_close = 0\n");
    let config = ClientConfig::parse_str(&text).unwrap();
    assert_eq!(config.client.session.idle_close, Some(Duration::ZERO));
    assert_ne!(config.client.session.idle_close, None);
}

#[test]
fn a_malformed_idle_close_is_refused_with_its_line() {
    for written in ["2 hours", "abc", "5x", "", "-5"] {
        let text = format!("{BASE_CLIENT}\n[client.session]\nidle_close = \"{written}\"\n");
        let err = ClientConfig::parse_str(&text).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "config.toml:10: client.session.idle_close must be a duration (`30s`, `5m`, \
                 `2h`, `1d`, or bare seconds), got `{written}`"
            )
        );
    }
    let negative = ClientConfig::parse_str(&format!(
        "{BASE_CLIENT}\n[client.session]\nidle_close = -5\n"
    ))
    .unwrap_err();
    assert!(
        negative
            .to_string()
            .starts_with("config.toml:10: client.session.idle_close must be a duration"),
        "a negative count of seconds is refused too: {negative}"
    );
}

/// A typo in `scope` is a refusal with the line, never a silent `oneshot`: read
/// as the default it would change which conversation a delivery lands in.
#[test]
fn an_unknown_scope_is_refused_and_never_becomes_oneshot() {
    let err = ClientConfig::parse_str(&format!(
        "{BASE_CLIENT}\n[client.session]\nscope = \"onesho\"\nidle_close = \"2h\"\n"
    ))
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "config.toml:10: client.session.scope must be `oneshot`, `task` or `role`, got `onesho`"
    );
    // The inline form is written before `[server]`, so the key is a top-level
    // one and the refusal points at the line that carries the value.
    let inline = ClientConfig::parse_str(
        r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"
client = { session = { scope = "tasks" } }

[server]
host = "127.0.0.1"
port = 7811
"#,
    )
    .unwrap_err();
    assert_eq!(
        inline.to_string(),
        "config.toml:4: client.session.scope must be `oneshot`, `task` or `role`, got `tasks`"
    );
}

#[test]
fn the_client_session_table_reads_after_the_other_tables() {
    let text = r#"role = "planner"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"
key_path = "keys/role.key"
plugins = ["demo"]
placement = "headless"

[server]
host = "127.0.0.1"
port = 7811

[acp]
mode = "acceptEdits"
permission = "allow"

[client.session]
scope = "task"
idle_close = "2h"
"#;
    let config = ClientConfig::parse_str(text).unwrap();
    assert_eq!(config.client.session.scope, SessionScope::Task);
    assert_eq!(
        config.client.session.idle_close,
        Some(Duration::from_secs(7_200))
    );
    assert_eq!(config.acp.permission, "allow");
    let bad = text.replace("scope = \"task\"", "scope = \"rol\"");
    assert_eq!(
        ClientConfig::parse_str(&bad).unwrap_err().to_string(),
        "config.toml:16: client.session.scope must be `oneshot`, `task` or `role`, got `rol`"
    );
}

/// `schema/config-client.schema.json` is regenerated by hand, and the
/// ignored-key report reads its property names: a `client.session` key the
/// published schema does not name is a real key the loader then calls unknown.
#[test]
fn the_published_client_schema_carries_the_session_table() {
    let schema: serde_json::Value = serde_json::from_str(config_client_schema()).unwrap();
    assert_eq!(
        schema["properties"]["client"]["default"],
        serde_json::json!({ "session": { "scope": "oneshot", "idle_close": null } })
    );
    let policy = &schema["definitions"]["SessionPolicy"];
    assert_eq!(policy["properties"]["scope"]["default"], "oneshot");
    assert!(
        policy.get("required").is_none(),
        "every `[client.session]` key is optional, so a partial table validates: {policy}"
    );
    let names: Vec<&str> = schema["definitions"]["SessionScope"]["oneOf"]
        .as_array()
        .expect("the three scopes")
        .iter()
        .filter_map(|arm| arm["enum"][0].as_str())
        .collect();
    assert_eq!(names, ["oneshot", "task", "role"]);
    assert_eq!(
        onlyne_config::keys::unknown_client_keys(
            "client = { session = { scope = \"role\", idle_close = \"2h\" } }\n"
        ),
        Ok(vec![]),
        "every key the table declares is named by the published schema"
    );
}
