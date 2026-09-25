use super::*;

#[test]
fn pane_run_line_quotes_for_posix_and_cmd() {
    let cases: &[(&[&str], &str, &str)] = &[
        (
            &["echo", "hello world"],
            "'echo' 'hello world'",
            "\"echo\" \"hello world\"",
        ),
        (&["a'b"], "'a'\\''b'", "\"a'b\""),
        (&["say", r#"x"y"#], "'say' 'x\"y'", "\"say\" \"x\"\"y\""),
    ];
    for (argv, posix, cmd) in cases {
        let tokens: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        let posix_line = tokens
            .iter()
            .map(|arg| posix_shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let cmd_line = tokens
            .iter()
            .map(|arg| cmd_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(posix_line, *posix, "posix {argv:?}");
        assert_eq!(cmd_line, *cmd, "cmd {argv:?}");
    }
    let live = pane_run_line(&["echo".into(), "hello world".into()]);
    #[cfg(unix)]
    assert_eq!(live, "'echo' 'hello world'");
    #[cfg(windows)]
    assert_eq!(live, "\"echo\" \"hello world\"");
}

#[test]
fn agent_name_follows_herdr_charset_and_length() {
    assert_eq!(
        agent_name("planner", "ABCD1234-ffff-4000-8000-000000000001"),
        "onlyne-planner-abcd1234"
    );
    assert!(agent_name("planner", "abcd1234ffff").len() <= 32);
    assert!(is_agent_name(&agent_name("planner", "abcd1234ffff")));
}
