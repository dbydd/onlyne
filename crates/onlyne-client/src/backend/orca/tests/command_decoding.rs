use super::*;

#[test]
fn spawn_command_enters_workspace_exports_env_and_quotes_args() {
    let mut env = BTreeMap::new();
    env.insert("ONLYNE_TASK".into(), "task one".into());
    env.insert("QUOTED".into(), "a'b".into());
    let command = spawn_command_posix(&SpawnSpec {
        cwd: "/tmp/work space".into(),
        task_id: "task-1".into(),
        command: vec!["pi".into(), "--model".into(), "gpt 5".into()],
        env,
        focus: None,
        placement: None,
        rename: None,
    })
    .unwrap();
    assert_eq!(
        command,
        "cd '/tmp/work space' && env 'ONLYNE_TASK=task one' 'QUOTED=a'\\''b' 'pi' '--model' 'gpt 5'; exit"
    );
}

/// One `spawn_command_cmd` row: cwd, env pairs, argv, expected line.
type CmdCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a [&'a str], &'a str);

#[test]
fn spawn_command_cmd_quotes_cwd_env_and_args() {
    let cases: &[CmdCase<'_>] = &[
        (
            r"C:\work space",
            &[("ONLYNE_TASK", "task one"), ("QUOTED", "a'b")],
            &["pi", "--model", "gpt 5"],
            r#"cd /d "C:\work space" && set "ONLYNE_TASK=task one" && set "QUOTED=a'b" && "pi" "--model" "gpt 5" & exit"#,
        ),
        (
            r"C:\ws",
            &[],
            &["echo", r#"say "hi""#],
            r#"cd /d "C:\ws" && "echo" "say ""hi""" & exit"#,
        ),
    ];
    for (cwd, env, argv, expected) in cases {
        let mut map = BTreeMap::new();
        for (key, value) in *env {
            map.insert((*key).into(), (*value).into());
        }
        let command = spawn_command_cmd(&SpawnSpec {
            cwd: (*cwd).into(),
            task_id: "task-1".into(),
            command: argv.iter().map(|s| (*s).to_string()).collect(),
            env: map,
            focus: None,
            placement: None,
            rename: None,
        })
        .unwrap();
        assert_eq!(command, *expected, "cwd={cwd}");
    }
}

/// The line is evaluated by the tab's shell, so quoting and the trailing
/// `exit` have to survive a real one: run the generated line through `sh`
/// and read back an environment value and an argument that both carry
/// spaces, quotes and expansion characters.
#[cfg(unix)]
#[test]
fn spawn_command_round_trips_awkward_env_and_args_through_a_shell() {
    let mut env = BTreeMap::new();
    env.insert("ONLYNE_MIX".into(), "a'b\"c$d".into());
    let cwd = tempfile::tempdir().unwrap();
    let command = spawn_command(&SpawnSpec {
        cwd: cwd.path().to_path_buf(),
        task_id: "task-1".into(),
        command: vec![
            "sh".into(),
            "-c".into(),
            "printf '%s|%s' \"$ONLYNE_MIX\" \"$1\"".into(),
            "onlyne".into(),
            "a b'c\"d;e && exit".into(),
        ],
        env,
        focus: None,
        placement: None,
        rename: None,
    })
    .unwrap();
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .output()
        .unwrap();
    assert!(output.status.success(), "{command}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "a'b\"c$d|a b'c\"d;e && exit"
    );
}

/// Run one generated line the way the tab's shell does, with a sentinel
/// after it: the sentinel printing means the shell came back to a prompt
/// instead of exiting, which is the stuck tab this tail exists to prevent.
#[cfg(unix)]
fn run_through_shell(line: &str) -> (std::process::ExitStatus, String) {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{line}\nprintf 'AFTER'"))
        .output()
        .unwrap();
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// The tail reclaims the tab whatever the command does, which is why it is
/// `; exit` and not ` && exit`.
///
/// Mirrors the three measured controls for the tab's lifetime — a command
/// that exits leaves no tab, and only a bare command does — as: the line
/// carries its shell out on success (A), it carries the shell out on a
/// failure too (B), and the same failing line without the tail returns to
/// the prompt (C), which is the tab that used to stay `running`. With `&&`
/// the failing case behaved as C: a crashed agent never reached the tail.
#[cfg(unix)]
#[test]
fn spawn_command_line_exits_the_tab_shell_on_success_and_on_failure() {
    let cwd = tempfile::tempdir().unwrap();
    let line = |command: Vec<String>| {
        spawn_command(&SpawnSpec {
            cwd: cwd.path().to_path_buf(),
            task_id: "task-1".into(),
            command,
            env: BTreeMap::new(),
            focus: None,
            placement: None,
            rename: None,
        })
        .unwrap()
    };

    // A: the command succeeds and the shell still stops.
    let (status, output) =
        run_through_shell(&line(vec!["sh".into(), "-c".into(), "exit 0".into()]));
    assert!(status.success(), "{status}");
    assert_eq!(output, "");

    // B: the command fails with 7, and its status survives the tail.
    let failing = line(vec![
        "sh".into(),
        "-c".into(),
        "printf '%s' \"$1\"; exit 7".into(),
        "onlyne".into(),
        "a b'c\"d".into(),
    ]);
    let (status, output) = run_through_shell(&failing);
    assert_eq!(status.code(), Some(7), "{failing}");
    assert_eq!(output, "a b'c\"d");

    // C: the same failing line without the tail comes back to the prompt.
    let (_, output) = run_through_shell(failing.strip_suffix("; exit").unwrap());
    assert_eq!(output, "a b'c\"dAFTER");
}

#[test]
fn spawn_command_rejects_an_empty_command() {
    let error = spawn_command(&SpawnSpec {
        cwd: "/tmp/work".into(),
        task_id: "task-1".into(),
        command: vec![],
        env: BTreeMap::new(),
        focus: None,
        placement: None,
        rename: None,
    })
    .unwrap_err();
    assert!(error.to_string().contains("requires a command"));
}
