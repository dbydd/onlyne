//! The CLI layer: which binary, which window, which environment.

use super::*;
use std::path::Path;

#[test]
fn the_default_binary_is_the_absolute_app_path_not_a_path_lookup() {
    // A bare `tern` would answer from whatever is on `PATH`, and an unrelated
    // binary there would answer with a document this module cannot read. The
    // default is absolute so the client's `PATH` cannot choose.
    let (backend, _) = backend(Script::default());
    assert_eq!(
        backend.command,
        "/Applications/Tern.app/Contents/MacOS/tern"
    );
    assert_eq!(backend.command, super::super::cli::default_command());
}

#[test]
fn tern_command_overrides_the_binary() {
    let mut env = BTreeMap::new();
    env.insert("TERN_COMMAND".into(), "/opt/tern".into());
    let backend = TernBackend::with_env(Arc::new(Script::default()), env);
    assert_eq!(backend.command, "/opt/tern");
}

#[test]
fn an_empty_tern_command_falls_back_to_the_default() {
    let mut env = BTreeMap::new();
    env.insert("TERN_COMMAND".into(), String::new());
    let backend = TernBackend::with_env(Arc::new(Script::default()), env);
    assert_eq!(
        backend.command,
        "/Applications/Tern.app/Contents/MacOS/tern"
    );
}

#[test]
fn no_window_flag_without_a_window_key() {
    // A client outside any pane drives the first window, which is what
    // omitting `--window` does. An empty `--window ""` is accepted and
    // ignored by tern, so the flag is left off rather than sent empty.
    let mut env = BTreeMap::new();
    env.insert("TERN_WINDOW_KEY".into(), "   ".into());
    let backend = TernBackend::with_env(Arc::new(Script::default()), env);
    assert_eq!(backend.window_args(), Vec::<String>::new());
}

#[test]
fn a_nonempty_window_key_is_appended_to_every_call() {
    let script = Arc::new(Script::default().reply("ls --json", listing(vec![])));
    let mut env = BTreeMap::new();
    env.insert("TERN_WINDOW_KEY".into(), "  w-42 ".into());
    let backend = TernBackend::with_env(script.clone(), env);
    backend.available().unwrap();
    assert_eq!(
        script.calls(),
        vec!["/Applications/Tern.app/Contents/MacOS/tern ls --json --window w-42".to_string()]
    );
}

#[test]
fn the_window_flag_lands_before_the_command_separator() {
    // `split` carries the session command after `--`, so a window argument
    // appended after it would land inside that command's own argv. The flag is
    // appended by `json`, which every mutating call goes through.
    let mut env = BTreeMap::new();
    env.insert("TERN_WINDOW_KEY".into(), "w-42".into());
    let script =
        Arc::new(Script::default().reply("split", created(2147483648, 2147483649, 2147483660)));
    let backend = TernBackend::with_env(script.clone(), env);
    backend
        .split_and_start(
            "2147483650",
            &spec(),
            PanePlacement::from_pane_count(0),
            "2147483648",
            "2147483649",
        )
        .unwrap();
    let call = script.calls().remove(0);
    let (before, after) = call.split_once(" -- ").expect("a -- separator");
    assert!(before.ends_with("--window w-42"), "{call}");
    assert!(after.starts_with("env "), "{call}");
}

#[test]
fn only_tern_variables_reach_the_cli() {
    // A pane's environment carries the client's — the tools-mount token
    // included, which is a capability a model in the pane must not read. The
    // session contract travels inside the block's own `env` prefix instead.
    let script = Arc::new(Script::default().reply("ls --json", listing(vec![])));
    let mut env = BTreeMap::new();
    env.insert("TERN_WINDOW_KEY".into(), "w-42".into());
    env.insert("TERN_PANE_SOCKET".into(), "/tmp/daemon.sock".into());
    env.insert("ONLYNE_ROLE".into(), "planner".into());
    env.insert("SECRET".into(), "do-not-leak".into());
    let backend = TernBackend::with_env(script, env);
    let passed = backend.cli_env();
    assert_eq!(
        passed.get("TERN_WINDOW_KEY").map(String::as_str),
        Some("w-42")
    );
    assert_eq!(
        passed.get("TERN_PANE_SOCKET").map(String::as_str),
        Some("/tmp/daemon.sock")
    );
    assert!(!passed.contains_key("ONLYNE_ROLE"), "{passed:?}");
    assert!(!passed.contains_key("SECRET"), "{passed:?}");
}

#[test]
fn the_session_contract_is_the_only_environment_a_pane_receives() {
    // The command is prefixed with `env NAME=VALUE` for exactly the spec's
    // environment — nothing from the client's own process.
    let mut spec = spec();
    spec.env.insert("ONLYNE_SESSION_ID".into(), "s-1".into());
    let argv = super::super::resource::launch_argv(&spec);
    assert_eq!(
        argv,
        vec![
            "env",
            "ONLYNE_CLUSTER=lab",
            "ONLYNE_ROLE=planner",
            "ONLYNE_SESSION_ID=s-1",
            "pi",
            "--session-id",
            "a b",
        ]
    );
    // A value carrying a space stays one argv token: it is `env`'s argument,
    // never a shell's word, so nothing quotes it and nothing re-splits it.
    assert_eq!(argv.last().map(String::as_str), Some("a b"));
}

#[test]
fn ids_are_read_as_numbers_or_as_the_strings_a_state_file_holds() {
    // Tern prints its ids as JSON numbers and quotes them in its refusals, so
    // both spellings are what a ref may carry. A number renders the way tern
    // renders it; a string is kept as written, so a ref round-trips byte for
    // byte.
    use serde_json::json;
    assert_eq!(super::super::policy::id_of(&json!(12)), Some("12".into()));
    assert_eq!(super::super::policy::id_of(&json!("12")), Some("12".into()));
    assert_eq!(super::super::policy::id_of(&Value::Null), None);
    // A create answer is a bare number; a build that nests it is read the same
    // way.
    assert_eq!(
        super::super::policy::created_id(&json!({"block": 12}), "block"),
        Some("12".into())
    );
    assert_eq!(
        super::super::policy::created_id(&json!({"block": {"id": 12}}), "block"),
        Some("12".into())
    );
    assert_eq!(super::super::policy::created_id(&json!({}), "block"), None);
}

#[test]
fn a_ref_written_with_number_ids_reads_back_as_strings() {
    let session = SessionRef {
        task_id: "t".into(),
        backend: "tern".into(),
        backend_ref: serde_json::json!({
            "tern": {
                "session_id": 2147483648u64,
                "tab_id": 2147483649u64,
                "pane_id": 2147483660u64,
            }
        }),
        generation: 1,
    };
    let reference = super::super::policy::TernRef::from_session(&session).unwrap();
    assert_eq!(reference.session_id, "2147483648");
    assert_eq!(reference.tab_id, "2147483649");
    assert_eq!(reference.pane_id, "2147483660");
    // Absent optional fields are empty, not missing: a ref written before they
    // existed still parses.
    assert_eq!(reference.base_pane, "");
    assert_eq!(reference.split_direction, "");
}

#[test]
fn a_ref_missing_its_tern_object_is_an_error_naming_the_field() {
    let session = SessionRef {
        task_id: "t".into(),
        backend: "tern".into(),
        backend_ref: serde_json::json!({"tern": {"session_id": "1"}}),
        generation: 1,
    };
    let error = super::super::policy::TernRef::from_session(&session).unwrap_err();
    assert!(error.to_string().contains("missing tab_id"), "{error}");
}

#[test]
fn an_absent_cwd_keeps_its_spelling_and_a_relative_one_is_absolutized() {
    // Tern resolves a relative `--cwd` against the daemon's directory, which
    // the client never chose, so a relative one is made absolute here.
    assert_eq!(
        super::super::cli::absolute_cwd(Path::new("/w/role")),
        "/w/role"
    );
    let relative = super::super::cli::absolute_cwd(Path::new("role"));
    assert!(
        relative.ends_with("/role") && relative.starts_with('/'),
        "{relative}"
    );
}
