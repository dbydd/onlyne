//! `onlyne-client init` ships two artifacts an operator reads: the
//! `.onlyne/config.toml` it writes in the workspace, and the `[[client]]`
//! fragment it prints for `spec.toml`. Both carry their optional vocabulary as
//! comment lines, and the rule the whole file rests on is that a comment costs
//! nothing: the entry a paste produces is exactly the entry with no comment
//! uncommented. These cases drive the real binary and pin that rule against the
//! parser in `onlyne-config`, so a template that drifts from the defaults it
//! quotes, or a comment line that loses its `#`, fails here.

use onlyne_client::init::toml_string;
use onlyne_config::{AcpSection, ClientEntry, IntentPolicy, Spec, Timeouts};
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Output};

const SERVER_SPEC: &str = "[server]\nname = \"srv\"\nlisten = \"127.0.0.1:7899\"\ncert_pin = \"sha256/0000000000000000000000000000000000000000000000000000000000000000\"\n";

/// `onlyne-client init` prints its fragment and exits 0; anything else is a
/// refusal the cases below report verbatim.
const EXIT_OK: i32 = 0;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_onlyne-client"))
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A server root holding the spec `init` reads its endpoint and pin from.
fn server_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join(".onlyne/spec.toml");
    std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
    std::fs::write(&spec, SERVER_SPEC).unwrap();
    dir
}

/// Run `onlyne-client init` for one role in one workspace.
fn init(workspace: &Path, server: &Path, role: &str, prose: &str) -> Output {
    Command::new(bin())
        .env_remove("ONLYNE_BACKEND")
        .args([
            "init",
            "--workspace",
            workspace.to_str().unwrap(),
            "--role",
            role,
            "--server-root",
            server.to_str().unwrap(),
            "--prose",
            prose,
        ])
        .output()
        .unwrap()
}

fn config_text(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join(".onlyne/config.toml")).expect("init wrote the config")
}

fn comment_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn live_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Strip the `# ` prefix from every comment line that spells a TOML key or a
/// table header, and leave the prose lines alone. That set is the template's
/// whole documented vocabulary, and uncommenting it in place is exactly the edit
/// an operator makes by hand, so the result is the config a paste produces.
fn uncomment(text: &str) -> String {
    text.lines()
        .map(|line| match line.strip_prefix("# ") {
            Some(body)
                if body.contains(" = ") || (body.starts_with('[') && body.ends_with(']')) =>
            {
                body
            }
            _ => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn line_index(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|line| line == needle)
        .unwrap_or_else(|| panic!("{needle:?} is not a line of:\n{text}"))
}

fn sorted_keys(table: &toml::Table) -> Vec<String> {
    let mut keys: Vec<String> = table.keys().cloned().collect();
    keys.sort();
    keys
}

/// The template documents `backend` above the live `[server]` header and the
/// `[acp]` table below it, and that placement is load-bearing in TOML: a key
/// belongs to whichever table was last opened. Uncomment the block in place and
/// the config must load with each key in its own table. A family moved to the
/// wrong side of `[server]` produces a config that fails `deny_unknown_fields`,
/// which is the silent breakage a line-by-line text match would wave through.
#[test]
fn uncommenting_the_template_lands_each_key_in_its_own_table() {
    let server = server_root();
    let workspace = tempfile::tempdir().unwrap();
    let output = init(workspace.path(), server.path(), "planner", "plan the work");
    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "init answers with the fragment: {}",
        stderr_of(&output)
    );
    let fragment = stdout_of(&output);
    let config = config_text(workspace.path());

    // Relative order, recorded as three index comparisons.
    let backend_key = line_index(&config, "# backend = \"auto\"");
    let server_header = line_index(&config, "[server]");
    let acp_header = line_index(&config, "# [acp]");
    assert!(
        backend_key < server_header && server_header < acp_header,
        "the backend vocabulary sits above [server] and the [acp] block below it:\n{config}"
    );

    let range = config
        .lines()
        .take(server_header)
        .filter(|line| line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for name in [
        "herdr", "orca", "zellij", "exec", "headless", "acp", "fake", "auto",
    ] {
        assert!(
            range.contains(name),
            "the backend comment names {name}, a value an operator may write: {range}"
        );
    }
    assert!(
        range.contains("ONLYNE_BACKEND"),
        "and it states the precedence the environment holds over the key: {range}"
    );

    let acp_intro = config
        .lines()
        .skip(server_header + 1)
        .take_while(|line| *line != "# [acp]")
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        acp_intro.contains("deny") && acp_intro.contains("allow"),
        "the [acp] block closes by naming both answers to a permission request: {acp_intro}"
    );

    let uncommented: toml::Table = toml::from_str(&uncomment(&config))
        .expect("uncommenting the documented vocabulary leaves valid TOML");
    assert_eq!(
        sorted_keys(&uncommented),
        [
            "acp", "backend", "cert_pin", "key_path", "plugins", "role", "server"
        ],
        "one uncommented key per table it belongs to, and the prose lines stay comments"
    );
    assert_eq!(
        sorted_keys(
            uncommented["server"]
                .as_table()
                .expect("[server] is a table")
        ),
        ["host", "port"],
        "the [server] table keeps the two keys its header owns"
    );
    assert_eq!(
        sorted_keys(
            uncommented["acp"]
                .as_table()
                .expect("# [acp] opens a table")
        ),
        ["mode", "model", "permission", "reasoning_effort"],
        "the [acp] block carries exactly the four keys the ACP backend reads"
    );
    let loaded = onlyne_config::ClientConfig::parse_str(&uncomment(&config))
        .expect("the uncommented template loads as a config");
    assert_eq!(
        loaded.backend, "auto",
        "backend belongs to the top level of the config"
    );
    assert_eq!(
        loaded.acp,
        AcpSection::default(),
        "uncommenting the [acp] block applies the parser's own defaults"
    );

    // The knob vocabulary closes the printed fragment, after every live line, so
    // a paste keeps one entry per `[[]]` header.
    let first_comment = fragment
        .lines()
        .position(|line| line.starts_with('#'))
        .expect("the fragment carries its optional keys as comments");
    assert_eq!(
        first_comment,
        live_lines(&fragment).len(),
        "the comments form one block at the foot of the fragment:\n{fragment}"
    );
    assert!(
        fragment
            .lines()
            .skip(first_comment)
            .all(|line| line.starts_with('#')),
        "the comment block runs to the foot of the fragment:\n{fragment}"
    );
    for knob in [
        "# timeout = {",
        "# intent = {",
        "# aggregate = ",
        "# relay_required = ",
        "# relay_count = ",
    ] {
        assert!(
            comment_lines(&fragment)
                .iter()
                .any(|line| line.starts_with(knob)),
            "the fragment documents {knob:?}:\n{fragment}"
        );
    }
}

/// A comment carries weight only in the form TOML ignores. Strip the comment lines
/// and what is left parses, carries exactly the live keys, and lands on the
/// parser's own defaults for every documented knob. A template that invented a
/// value would set a policy no operator asked for, and a comment line that lost
/// its `#` would change the entry a paste produces.
#[test]
fn stripping_the_comments_leaves_exactly_the_live_keys() {
    let server = server_root();
    let workspace = tempfile::tempdir().unwrap();
    let output = init(workspace.path(), server.path(), "planner", "plan");
    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "{}",
        stderr_of(&output)
    );
    let fragment = stdout_of(&output);
    let config = config_text(workspace.path());

    let stripped_config = live_lines(&config).join("\n") + "\n";
    let table: toml::Table =
        toml::from_str(&stripped_config).expect("the live lines of the config are valid TOML");
    assert_eq!(
        sorted_keys(&table),
        ["cert_pin", "key_path", "plugins", "role", "server"],
        "the live config sets exactly the five keys a workspace needs:\n{stripped_config}"
    );
    // The same text with the comments left in place is what the client loads, and
    // each documented surface reads as the parser's own default.
    let loaded: onlyne_config::ClientConfig =
        toml::from_str(&config).expect("init wrote a config the client can load");
    assert_eq!(
        loaded.backend,
        String::new(),
        "backend stays unset: {config}"
    );
    assert_eq!(loaded.acp, AcpSection::default(), "[acp] stays absent");

    let stripped_fragment = live_lines(&fragment).join("\n") + "\n";
    let entry_table: toml::Table =
        toml::from_str(&stripped_fragment).expect("the live lines of the fragment are valid TOML");
    let clients = entry_table["client"].as_array().expect("[[client]] array");
    assert_eq!(clients.len(), 1, "the fragment is one entry");
    assert_eq!(
        sorted_keys(clients[0].as_table().expect("one client table")),
        [
            "admin",
            "allowed_senders",
            "allowed_targets",
            "key",
            "max_sessions",
            "prose",
            "reuse",
            "role",
            "session_command",
        ],
        "the live entry carries exactly the nine keys the fragment writes"
    );

    let spec = format!("{SERVER_SPEC}\n{fragment}");
    let parsed = Spec::parse_str(&spec).expect("the pasted fragment is a spec");
    let planner = parsed
        .client
        .iter()
        .find(|entry| entry.role == "planner")
        .expect("the fragment registers the role");
    assert_eq!(planner.aggregate, String::new());
    assert_eq!(planner.timeout, Timeouts::default());
    assert_eq!(planner.intent, IntentPolicy::default());
    assert_eq!(planner.relay_required, None);
    assert_eq!(planner.relay_count, None);
}

/// Every commented default is a claim about `onlyne-config`, and the claim worth
/// testing is the one quoted from the code. Read each expected value off the
/// parser's own defaults, so the day a default moves is the day the template
/// fails.
#[test]
fn the_documented_defaults_are_the_parsers_defaults() {
    let server = server_root();
    let workspace = tempfile::tempdir().unwrap();
    let output = init(workspace.path(), server.path(), "planner", "plan");
    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "{}",
        stderr_of(&output)
    );
    let fragment = stdout_of(&output);
    let config = config_text(workspace.path());

    let timeouts = Timeouts::default();
    assert!(
        fragment.contains(&format!(
            "# timeout = {{ ready_ms = {}, running_ms = {}, idle_ms = {} }}",
            timeouts.ready_ms, timeouts.running_ms, timeouts.idle_ms
        )),
        "the timeout line quotes `Timeouts::default()`:\n{fragment}"
    );
    let intent = IntentPolicy::default();
    let backoff: Vec<String> = intent.backoff_ms.iter().map(u64::to_string).collect();
    assert!(
        fragment.contains(&format!(
            "# intent = {{ attempts = {}, backoff_ms = [{}] }}",
            intent.attempts,
            backoff.join(", ")
        )),
        "the intent line quotes `IntentPolicy::default()`:\n{fragment}"
    );

    let entry: ClientEntry =
        toml::from_str("role = \"r\"\nkey = \"k\"\n").expect("a minimal entry parses");
    assert!(
        fragment.contains(&format!("# aggregate = {}", toml_string(&entry.aggregate))),
        "the aggregate line quotes the entry default:\n{fragment}"
    );
    // The relay guard is the knob with two spellings and one default: no guard.
    // The parser's own answer for a minimal entry is quoted in the block, so the
    // comment and the code cannot drift apart silently.
    assert_eq!(entry.relay_required, None);
    assert_eq!(entry.relay_count, None);
    assert!(
        fragment.contains("# relay_required = []"),
        "the fragment shows the empty-list spelling of that default:\n{fragment}"
    );
    assert!(
        fragment.contains("# relay_count = "),
        "and the count spelling beside it:\n{fragment}"
    );

    let acp = AcpSection::default();
    assert!(
        config.contains(&format!("# permission = {}", toml_string(&acp.permission))),
        "the [acp] block quotes `AcpSection::default().permission`:\n{config}"
    );
    for (key, value) in [
        ("mode", &acp.mode),
        ("model", &acp.model),
        ("reasoning_effort", &acp.reasoning_effort),
    ] {
        assert!(
            config.contains(&format!("# {key} = {}", toml_string(value))),
            "the [acp] block quotes the empty default of {key}:\n{config}"
        );
    }
}

/// The template is one literal string, so two runs must agree byte for byte.
/// Identity lines come from a generated key and differ per workspace; the
/// comment block has no input to vary over, and a run that reordered it would
/// split the operator's paste from the documented order.
#[test]
fn repeated_runs_print_the_same_template() {
    let server = server_root();
    let workspace = tempfile::tempdir().unwrap();
    let first = init(workspace.path(), server.path(), "planner", "plan");
    let first_fragment = stdout_of(&first);
    let first_config = config_text(workspace.path());
    let second = init(workspace.path(), server.path(), "planner", "plan");
    let second_fragment = stdout_of(&second);
    let second_config = config_text(workspace.path());
    assert_eq!(
        second.status.code(),
        Some(EXIT_OK),
        "{}",
        stderr_of(&second)
    );
    assert_eq!(
        second_fragment, first_fragment,
        "re-running init over its own workspace prints the same fragment"
    );
    assert_eq!(
        second_config, first_config,
        "re-running init leaves the config it wrote byte-identical"
    );

    let other_server = server_root();
    let other = tempfile::tempdir().unwrap();
    let third = init(other.path(), other_server.path(), "planner", "plan");
    assert_eq!(third.status.code(), Some(EXIT_OK), "{}", stderr_of(&third));
    assert_eq!(
        comment_lines(&stdout_of(&third)),
        comment_lines(&first_fragment),
        "the comment block arrives in one fixed order across workspaces"
    );
    assert_eq!(
        comment_lines(&config_text(other.path())),
        comment_lines(&first_config),
        "and so does the workspace config's"
    );
}
