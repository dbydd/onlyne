//! The zellij backend: one zellij session per task.
//!
//! Session names are derived from the task id rather than remembered, so
//! `spawn`, `attach`, `probe` and `close` agree on the name with no state
//! carried between them — and the derivation is what fits the name inside
//! zellij's socket path budget.

use super::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Prefix every onlyne session name carries, so `zellij list-sessions` reads as
/// the onlyne sessions among the user's.
const SESSION_PREFIX: &str = "onlyne-";

/// How many task-id characters the name keeps after the prefix.
///
/// A uuid v4 is 32 hex digits plus four dashes, so twelve dash-free characters
/// are twelve hex digits: 48 bits of name entropy. Two tasks in one workspace
/// would have to share their first twelve id characters to collide, which is
/// negligible at workspace scale, and the truncation is what buys a 19-byte
/// name. Length is the point: zellij refuses a session whose IPC socket path
/// reaches 104 bytes on macOS (`sun_path`), the full `onlyne-<uuid>` name is 43
/// bytes, and a socket directory can spend most of the rest — 79 bytes on this
/// machine — so the untruncated name failed every spawn with zellij's report of
/// a negative character budget.
const SESSION_ID_CHARS: usize = 12;

/// Zellij's client-server contract directory, appended to the socket directory
/// (`zellij-utils/src/consts.rs`).
const CONTRACT_DIR: &str = "contract_version_1";

/// The longest session IPC socket path zellij accepts: `check_ipc_pipe_length`
/// refuses a path that reaches it (`zellij-client/src/lib.rs`), and
/// `ZELLIJ_SOCK_MAX_LENGTH` carries `sun_path`'s 104 bytes on macOS/BSD and 108
/// elsewhere.
#[cfg(target_os = "macos")]
const SOCK_PATH_LIMIT: usize = 104;
#[cfg(not(target_os = "macos"))]
const SOCK_PATH_LIMIT: usize = 108;

/// The session name for one task: the prefix plus the first
/// [`SESSION_ID_CHARS`] characters of the task id with its dashes dropped.
///
/// Pure by design. Nothing stores the name: `spawn` builds it, and `attach`,
/// `probe` and `close` rebuild it from the `SessionRef::task_id` they already
/// hold, so the name cannot disagree between the call that made a session and
/// the call that ends it.
fn short_session_name(task_id: &str) -> String {
    let id: String = task_id
        .chars()
        .filter(|c| *c != '-')
        .take(SESSION_ID_CHARS)
        .collect();
    format!("{SESSION_PREFIX}{id}")
}

/// The checked session name for one task: [`short_session_name`], refused when
/// not even that fits zellij's socket path budget.
fn session_name(task_id: &str) -> Result<String> {
    let name = short_session_name(task_id);
    check_socket_budget(&socket_dir(), &name)?;
    Ok(name)
}

/// Refuse a session name whose socket path overruns the budget zellij enforces,
/// naming the override that fixes it.
///
/// Shortening the name cannot help at this point — the socket directory itself
/// is what is over budget — so the only cure is a shorter directory, which
/// zellij reads from `ZELLIJ_SOCKET_DIR`. Left to zellij the operator instead
/// gets its report of a negative character budget, naming neither the cause nor
/// the cure.
fn check_socket_budget(dir: &Path, name: &str) -> Result<()> {
    let socket = dir.join(name);
    let path_len = socket.as_os_str().len();
    if path_len >= SOCK_PATH_LIMIT {
        anyhow::bail!(
            "zellij session {name} needs a {path_len}-byte socket path ({}), over the \
             {SOCK_PATH_LIMIT}-byte unix socket limit; set ZELLIJ_SOCKET_DIR to a shorter directory",
            socket.display()
        );
    }
    Ok(())
}

/// Zellij's session socket directory, rebuilt the way zellij computes it
/// (`zellij-utils/src/consts.rs`): `ZELLIJ_SOCKET_DIR` when set, else the
/// project runtime directory on platforms that have one, else a per-uid
/// directory under the temp dir — with the client-server contract directory
/// appended in every case.
fn socket_dir() -> PathBuf {
    let base = std::env::var("ZELLIJ_SOCKET_DIR").map_or_else(
        |_| {
            runtime_dir()
                .unwrap_or_else(|| std::env::temp_dir().join(format!("zellij-{}", temp_dir_uid())))
        },
        PathBuf::from,
    );
    base.join(CONTRACT_DIR)
}

/// The project runtime directory zellij prefers where a platform defines one.
/// `ProjectDirs::runtime_dir` is `Some` only on Linux, as
/// `$XDG_RUNTIME_DIR/zellij`.
#[cfg(target_os = "linux")]
fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join("zellij"))
}

#[cfg(not(target_os = "linux"))]
fn runtime_dir() -> Option<PathBuf> {
    None
}

/// The uid zellij stamps into its temp socket directory.
///
/// std exposes no `getuid`, and on macOS the temp dir is per-user, so its owner
/// is that uid there. Where the temp dir is shared the number can be a digit or
/// two off, which moves the budget check by the same amount; that check exists
/// for a socket directory far past the limit, where a digit cannot decide the
/// answer.
#[cfg(unix)]
fn temp_dir_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;

    std::fs::metadata(std::env::temp_dir()).map_or(0, |meta| meta.uid())
}

#[cfg(not(unix))]
fn temp_dir_uid() -> u32 {
    0
}

pub struct ZellijBackend {
    runner: Arc<dyn Runner>,
    command: String,
}
impl ZellijBackend {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            command: std::env::var("ZELLIJ_COMMAND").unwrap_or_else(|_| "zellij".into()),
        }
    }

    /// Make sure the session exists before an action is addressed to it, and
    /// report whether this call created it.
    ///
    /// `zellij run` is an action sent to a *live* session: with none, zellij
    /// answers `There is no active session!`. A task's first spawn therefore has
    /// to bring the session up first. `attach --create-background` makes one
    /// detached without a TTY (the interactive `--create` requires one), and it
    /// is not idempotent — on a session that already exists it exits 1 with
    /// `Session already exists` — so the listing decides rather than the exit
    /// code, which would break the day zellij rewords its message.
    fn ensure_session(&self, name: &str) -> Result<bool> {
        if self.session_listed(name)? {
            return Ok(false);
        }
        run_checked(
            self.runner.as_ref(),
            &self.command,
            &["attach".into(), "--create-background".into(), name.into()],
            None,
            &BTreeMap::new(),
        )
        .map_err(|error| anyhow::anyhow!("zellij attach --create-background {name}: {error}"))?;
        Ok(true)
    }

    /// Whether `list-sessions --short` names a session, which is the one place
    /// "this session exists" is decided: `attach` uses it to answer whether the
    /// resource is still there, and `spawn` uses it to decide on creating one.
    fn session_listed(&self, name: &str) -> Result<bool> {
        let out = self.runner.run(
            &self.command,
            &["list-sessions".into(), "--short".into()],
            None,
            &BTreeMap::new(),
        )?;
        Ok(out.status == 0
            && String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == name))
    }

    /// `kill-session` for an already-derived name.
    fn kill_session(&self, name: &str) -> Result<()> {
        run_checked(
            self.runner.as_ref(),
            &self.command,
            &["kill-session".into(), name.into()],
            None,
            &BTreeMap::new(),
        )
        .map(|_| ())
    }

    /// Undo a session this call created when the spawn then failed, so a spawn
    /// that cannot produce a usable session ref leaves nothing running behind
    /// it.
    ///
    /// Only a session created by *this* call is reclaimed: one that was already
    /// listed may have a live pane a previous session ref addresses, and a
    /// failed run is no reason to take that away. A failed cleanup is logged and
    /// not propagated, because the run's own error is what the caller must see.
    fn reclaim_created(&self, created: bool, name: &str) {
        if !created {
            return;
        }
        if let Err(error) = self.kill_session(name) {
            tracing::warn!(
                session = %name,
                %error,
                "zellij could not reclaim the session of a failed spawn"
            );
        }
    }
}
impl SessionBackend for ZellijBackend {
    fn name(&self) -> &'static str {
        "zellij"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            spawn: true,
            attach: true,
            probe: true,
            close: true,
            focus: false,
            rename: false,
        }
    }
    fn available(&self) -> Result<bool> {
        Ok(self
            .runner
            .run(
                &self.command,
                &["list-sessions".into(), "--short".into()],
                None,
                &BTreeMap::new(),
            )
            .map(|o| o.status == 0)
            .unwrap_or(false))
    }

    /// Bring the session up if this is the task's first spawn, then run the
    /// command as a pane in it.
    ///
    /// The ref records the pane the command runs in, so `probe`/`close` address
    /// the session by the name they derive and a caller can read the pane id.
    /// A spawn that cannot finish takes the session it created with it.
    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        let session = session_name(&spec.task_id)?;
        let created = self.ensure_session(&session)?;
        let mut args = vec![
            "--session".into(),
            session.clone(),
            "run".into(),
            "--cwd".into(),
            spec.cwd.to_string_lossy().into_owned(),
            "--no-focus".into(),
            "--".into(),
        ];
        args.extend(spec.command);
        let pane = match run_checked(self.runner.as_ref(), &self.command, &args, None, &spec.env) {
            Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            Err(error) => {
                self.reclaim_created(created, &session);
                return Err(error);
            }
        };
        if pane.is_empty() {
            self.reclaim_created(created, &session);
            return Err(anyhow::anyhow!("zellij run returned no pane id"));
        }
        Ok(SessionRef {
            task_id: spec.task_id,
            backend: self.name().into(),
            backend_ref: serde_json::json!({"session": session, "pane": pane}),
            generation: 1,
        })
    }
    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        let name = session_name(&session.task_id)?;
        if !self.session_listed(&name)? {
            return Err(anyhow::anyhow!("zellij session not found: {name}"));
        }
        Ok(session.clone())
    }
    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        let attached = self.attach(session).is_ok();
        Ok(ResourceProbe {
            alive: attached,
            attached,
            detail: None,
        })
    }
    fn close(&self, session: &SessionRef, _reason: CloseReason, _force: bool) -> Result<()> {
        self.kill_session(&session_name(&session.task_id)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::VecDeque;

    /// A uuid v4 task id, the shape the server mints.
    const TASK: &str = "550e8400-e29b-41d4-a716-446655440000";

    /// Answers one scripted `(status, stdout)` per call and records every argv,
    /// so a test reads back the session name the CLI was handed.
    #[derive(Default)]
    struct ScriptRunner {
        calls: Mutex<Vec<Vec<String>>>,
        script: Mutex<VecDeque<(i32, String)>>,
    }

    impl ScriptRunner {
        fn reply(self, status: i32, stdout: &str) -> Self {
            self.script.lock().push_back((status, stdout.to_string()));
            self
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().clone()
        }
    }

    impl Runner for ScriptRunner {
        fn run(
            &self,
            _program: &str,
            args: &[String],
            _cwd: Option<&Path>,
            _env: &BTreeMap<String, String>,
        ) -> Result<CommandOutput> {
            self.calls.lock().push(args.to_vec());
            let (status, stdout) = self
                .script
                .lock()
                .pop_front()
                .unwrap_or_else(|| (0, String::new()));
            Ok(CommandOutput {
                status,
                stdout: stdout.into_bytes(),
                stderr: Vec::new(),
            })
        }
    }

    fn spec() -> SpawnSpec {
        SpawnSpec {
            cwd: PathBuf::from("/tmp/ws"),
            task_id: TASK.into(),
            command: vec!["pi".into()],
            env: BTreeMap::new(),
            focus: None,
            rename: None,
        }
    }

    #[test]
    fn the_name_is_short_enough_for_the_socket_budget() {
        let name = short_session_name(TASK);
        assert_eq!(name, "onlyne-550e8400e29b");
        assert_eq!(name.len(), SESSION_PREFIX.len() + SESSION_ID_CHARS);
        // The full task id used to be the name. At 43 bytes it cannot fit
        // beside a socket directory this machine already spends 79 bytes on,
        // which is what made every zellij spawn fail.
        let full = format!("onlyne-{TASK}");
        assert_eq!(full.len(), 43);
        assert_ne!(name, full);
        assert!(!name.contains(TASK));
    }

    #[test]
    fn the_name_is_a_pure_function_of_the_task_id() {
        assert_eq!(short_session_name(TASK), short_session_name(TASK));
        assert_ne!(
            short_session_name(TASK),
            short_session_name("6ba7b810-9dad-11d1-80b4-00c04fd430c8")
        );
        // A task id that is not a uuid still maps to a name, and no id length
        // carries into it.
        assert_eq!(short_session_name("task-1"), "onlyne-task1");
        assert_eq!(
            short_session_name(&"a".repeat(400)).len(),
            SESSION_PREFIX.len() + SESSION_ID_CHARS
        );
    }

    #[test]
    fn the_budget_stops_one_byte_short_of_the_zellij_limit() {
        let name = short_session_name(TASK);
        let dir = |len: usize| PathBuf::from(format!("/{}", "d".repeat(len - 1)));
        // `dir` + "/" + `name` one byte under the limit is accepted; one byte
        // more is refused, naming the override that fixes it.
        let fits = SOCK_PATH_LIMIT - 1 - name.len() - 1;
        assert!(check_socket_budget(&dir(fits), &name).is_ok());
        let error = check_socket_budget(&dir(fits + 1), &name)
            .unwrap_err()
            .to_string();
        assert!(error.contains("ZELLIJ_SOCKET_DIR"), "{error}");
        assert!(error.contains(&name), "{error}");
    }

    /// One call's argv, as a comparable list.
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    /// The name every assertion here expects for [`TASK`].
    const NAME: &str = "onlyne-550e8400e29b";

    /// The `run` argv: the command becomes a pane of the named session.
    fn run_argv() -> Vec<String> {
        argv(&[
            "--session",
            NAME,
            "run",
            "--cwd",
            "/tmp/ws",
            "--no-focus",
            "--",
            "pi",
        ])
    }

    /// A spawn whose session does not exist brings it up first, because `zellij
    /// run` is an action addressed to a live session.
    #[test]
    fn spawn_creates_the_session_before_running_in_it() {
        let runner = Arc::new(
            ScriptRunner::default()
                .reply(0, "other\n")
                .reply(0, "")
                .reply(0, "terminal_3\n"),
        );
        let backend = ZellijBackend::new(runner.clone());
        let session = backend.spawn(spec()).unwrap();
        let calls = runner.calls();
        assert_eq!(calls[0], argv(&["list-sessions", "--short"]));
        assert_eq!(calls[1], argv(&["attach", "--create-background", NAME]));
        assert_eq!(calls[2], run_argv());
        assert_eq!(session.backend_ref["session"], NAME);
        assert_eq!(session.backend_ref["pane"], "terminal_3");
    }

    /// A session that is already listed is reused rather than created again.
    #[test]
    fn spawn_reuses_a_session_that_is_already_listed() {
        let runner = Arc::new(
            ScriptRunner::default()
                .reply(0, "other\nonlyne-550e8400e29b\n")
                .reply(0, "terminal_4\n"),
        );
        let backend = ZellijBackend::new(runner.clone());
        backend.spawn(spec()).unwrap();
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], argv(&["list-sessions", "--short"]));
        assert_eq!(calls[1], run_argv());
    }

    /// A create that fails stops the spawn: no run is attempted against a
    /// session that is not there.
    #[test]
    fn a_failed_create_short_circuits_before_the_run() {
        let runner = Arc::new(ScriptRunner::default().reply(0, "other\n").reply(1, ""));
        let backend = ZellijBackend::new(runner.clone());
        let error = backend.spawn(spec()).unwrap_err().to_string();
        assert!(error.contains("attach --create-background"), "{error}");
        assert_eq!(runner.calls().len(), 2);
    }

    /// A run that fails after this call created the session takes that session
    /// with it: nothing outlives a spawn that produced no session ref.
    #[test]
    fn a_failed_run_reclaims_the_session_it_created() {
        let runner = Arc::new(
            ScriptRunner::default()
                .reply(0, "other\n")
                .reply(0, "")
                .reply(1, "")
                .reply(0, ""),
        );
        let backend = ZellijBackend::new(runner.clone());
        let error = backend.spawn(spec()).unwrap_err().to_string();
        assert!(error.contains("status 1"), "{error}");
        let calls = runner.calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[3], argv(&["kill-session", NAME]));
    }

    /// A run that fails against a session this call did not create leaves it
    /// alone: that session may still back a live session ref.
    #[test]
    fn a_failed_run_keeps_a_session_it_did_not_create() {
        let runner = Arc::new(
            ScriptRunner::default()
                .reply(0, "onlyne-550e8400e29b\n")
                .reply(1, ""),
        );
        let backend = ZellijBackend::new(runner.clone());
        assert!(backend.spawn(spec()).is_err());
        assert_eq!(runner.calls().len(), 2);
    }

    /// `attach` matches the derived name in `list-sessions` and `close` kills
    /// it, even when the ref was written under the old 43-byte naming.
    #[test]
    fn attach_and_close_derive_the_name_instead_of_reading_the_ref() {
        let runner = Arc::new(
            ScriptRunner::default()
                .reply(0, "other\nonlyne-550e8400e29b\n")
                .reply(0, ""),
        );
        let backend = ZellijBackend::new(runner.clone());
        let stored = SessionRef {
            task_id: TASK.into(),
            backend: "zellij".into(),
            backend_ref: serde_json::json!({
                "session": format!("onlyne-{TASK}"),
                "pane": "terminal_3"
            }),
            generation: 1,
        };
        backend.attach(&stored).unwrap();
        backend
            .close(&stored, CloseReason::Completed, false)
            .unwrap();
        let calls = runner.calls();
        assert_eq!(calls[0], argv(&["list-sessions", "--short"]));
        assert_eq!(calls[1], argv(&["kill-session", NAME]));
    }
}
