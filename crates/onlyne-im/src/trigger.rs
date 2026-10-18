//! The push scripts: what the gateway runs when an event or a beat arrives.
//!
//! A script is the operator's, and the gateway knows nothing about where it
//! sends. The contract is three things only, and all three are the shape
//! `[[hook]]` already gives a script (`docs/v2.2-IM-GATEWAY-PLAN.md` §4.3):
//!
//! - `argv[1]` is the mode (`trigger` or `heartbeat`); the operator's own
//!   arguments follow, verbatim.
//! - stdin is one JSON object, then closed: the event for a `trigger` (with the
//!   conversation it came out of), the gateway's coordinates for a `heartbeat`.
//! - the environment carries `ONLYNE_SOCKET`, so a script runs
//!   `onlyne send --from <role> --to <role>` without discovering the socket.
//!
//! A nonzero exit or a timeout is retried under [`PushRetry`], so the event
//! reaches the script at least once and the script may dedup on `event.id`; the
//! failure that survives every attempt is reported as an `im_push_failed` fault
//! (`docs/v2.2-IM-GATEWAY-PLAN.md` §8).

use crate::config::{PushMode, PushScript};
use onlyne_im_backend::payload::ImEvent;
use serde::Serialize;
use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// The fault kind a push script's surviving failure is recorded under, on the
/// conversation's own role link (`docs/v2.2-IM-GATEWAY-PLAN.md` §8).
pub const PUSH_FAILED_KIND: &str = "im_push_failed";

/// What one run of a script was given, rendered to the JSON object on stdin.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum PushInput {
    Trigger {
        /// The gateway role this run speaks for: one root is one role, and a
        /// script sends to the cluster as this role.
        role: String,
        gateway: String,
        platform: String,
        /// The chat key of the conversation the event came out of. This is
        /// what a script puts in the delivery's first line so the reply comes
        /// back here (`docs/v2.3-CHAT-PROTOCOL.md` §5.1).
        chat: String,
        /// The platform conversation the event came out of. One `[[im.push]]`
        /// script serves every conversation of the gateway, so this is how a
        /// script tells two of them apart.
        conversation: String,
        /// Boxed: one `ImEvent` dwarfs the heartbeat variant, and this enum
        /// crosses a channel once per event.
        event: Box<ImEvent>,
        /// What the System One reflex decided about this event, when a
        /// gateway had an endpoint to ask (`crate::decider`). Absent means
        /// nothing was asked, which is every gateway that never configured
        /// one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decider: Option<crate::decider::DeciderRouting>,
        at: String,
    },
    Heartbeat {
        /// The gateway role this run speaks for.
        role: String,
        gateway: String,
        platform: String,
        /// The chat key of the conversation this beat is for.
        chat: String,
        at: String,
    },
}

impl PushInput {
    /// The `mode` word `argv[1]` receives.
    pub fn mode_word(&self) -> &'static str {
        match self {
            PushInput::Trigger { .. } => "trigger",
            PushInput::Heartbeat { .. } => "heartbeat",
        }
    }

    /// The role this run speaks for: the conversation's role in the cluster.
    pub fn role(&self) -> &str {
        match self {
            PushInput::Trigger { role, .. } | PushInput::Heartbeat { role, .. } => role,
        }
    }

    /// The platform message id an event carries, which is the key a redelivery
    /// repeats and a script dedups on. Absent for a heartbeat.
    pub fn event_id(&self) -> Option<&str> {
        match self {
            PushInput::Trigger { event, .. } => Some(event.id.as_str()),
            PushInput::Heartbeat { .. } => None,
        }
    }
}

/// How often one script runs for one input, and how long the gateway waits
/// before trying again. The default is the gateway's own policy: three
/// attempts under a doubling second, which turns a hiccup into a retry and a
/// broken script into a fault rather than a storm.
#[derive(Debug, Clone, Copy)]
pub struct PushRetry {
    pub attempts: u32,
    pub base: Duration,
}

impl Default for PushRetry {
    fn default() -> Self {
        Self {
            attempts: 3,
            base: Duration::from_secs(1),
        }
    }
}

/// Why one run failed, for the fault record.
#[derive(Debug)]
pub enum PushFailure {
    /// The process could not be spawned.
    Spawn(std::io::Error),
    /// The script ran past its bound.
    Timeout,
    /// The script exited nonzero.
    Exit(Option<i32>),
}

impl std::fmt::Display for PushFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PushFailure::Spawn(error) => write!(f, "could not start the script: {error}"),
            PushFailure::Timeout => write!(f, "the script ran past its bound"),
            PushFailure::Exit(Some(code)) => write!(f, "the script exited {code}"),
            PushFailure::Exit(None) => write!(f, "the script ended by signal"),
        }
    }
}

/// The conversation's own paths, handed to a script in its environment.
///
/// A script needs no path arithmetic: the workspace and its media directory
/// arrive as variables, so reading the chat log or an attachment is one `$VAR`.
#[derive(Debug, Clone, Default)]
pub struct PushEnv {
    /// The served socket, exported as `ONLYNE_SOCKET`.
    pub socket: Option<String>,
    /// The conversation workspace (`conversations/<role>/`), as
    /// `ONLYNE_IM_WORKSPACE`.
    pub workspace: Option<String>,
    /// The attachment directory, as `ONLYNE_IM_MEDIA`.
    pub media: Option<String>,
    /// What the System One reflex decided about this event, exported as the
    /// `ONLYNE_DECIDER_*` variables a script routes on
    /// (`crate::decider`).
    pub decider: Option<crate::decider::DeciderRouting>,
}

impl PushEnv {
    /// The environment pairs this adds to what the child inherits.
    fn pairs(&self, input: &PushInput) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(socket) = &self.socket {
            pairs.push(("ONLYNE_SOCKET", socket.clone()));
        }
        if let Some(workspace) = &self.workspace {
            pairs.push(("ONLYNE_IM_WORKSPACE", workspace.clone()));
        }
        if let Some(media) = &self.media {
            pairs.push(("ONLYNE_IM_MEDIA", media.clone()));
        }
        let decider = self.decider.as_ref().or(match input {
            PushInput::Trigger { decider, .. } => decider.as_ref(),
            PushInput::Heartbeat { .. } => None,
        });
        if let Some(decider) = decider {
            pairs.push(("ONLYNE_DECIDER_SHOULD_HANDLE", decider.should_handle_str()));
            pairs.push(("ONLYNE_DECIDER_TARGET", decider.target_role.clone()));
            pairs.push(("ONLYNE_DECIDER_CONFIDENCE", decider.target_confidence_str()));
            if let Some(reply) = &decider.suggested_reply_to {
                pairs.push(("ONLYNE_DECIDER_REPLY_TO", reply.clone()));
            }
        }
        pairs
    }
}

/// Run one push script with one input until it succeeds or its attempts run
/// out, bounded by [`PushRetry`] and by the script's own timeout.
///
/// Retrying is what makes delivery at-least-once, and the event's id is what
/// makes a repeat safe: the script dedups on it (`docs/v2.2-IM-GATEWAY-PLAN.md`
/// §8). The failure that is returned is the last attempt's, so the fault names
/// what the script did rather than what the gateway hoped.
pub async fn run_script_retrying(
    script: &PushScript,
    input: &PushInput,
    env: &PushEnv,
    env_extra: &BTreeMap<String, String>,
    retry: &PushRetry,
) -> Result<(), PushFailure> {
    let mut wait = retry.base;
    let mut attempt = 1;
    loop {
        match run_script(script, input, env, env_extra).await {
            Ok(()) => return Ok(()),
            Err(failure) if attempt >= retry.attempts.max(1) => return Err(failure),
            Err(failure) => {
                tracing::warn!(
                    attempt,
                    attempts = retry.attempts,
                    program = %script.run[0],
                    error = %failure,
                    "a push script failed; retrying"
                );
                tokio::time::sleep(wait).await;
                wait *= 2;
                attempt += 1;
            }
        }
    }
}

/// Run one push script with one input, bounded by the script's own timeout.
///
/// `env` carries the socket and the conversation's own paths; `env_extra` is
/// whatever else the caller wants the child to see. stdout and stderr are
/// inherited, so a script that logs lands in the gateway's own logs; the run's
/// answer is its exit status alone.
pub async fn run_script(
    script: &PushScript,
    input: &PushInput,
    env: &PushEnv,
    env_extra: &BTreeMap<String, String>,
) -> Result<(), PushFailure> {
    debug_assert_eq!(input.mode_word(), mode_word(script.mode));
    let mut command = Command::new(&script.run[0]);
    command
        .arg(input.mode_word())
        .args(&script.run[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    command.envs(env_extra);
    command.envs(env.pairs(input));
    let body = serde_json::to_vec(input).expect("PushInput serialises");
    let mut child = command.spawn().map_err(PushFailure::Spawn)?;
    if let Some(mut stdin) = child.stdin.take() {
        // The body is small and stdin closes right after; a write error here
        // means the script exited before reading, which its exit code reports.
        let _ = stdin.write_all(&body).await;
        let _ = stdin.write_all(b"\n").await;
        drop(stdin);
    }
    match tokio::time::timeout(script.timeout, child.wait()).await {
        Err(_) => {
            // `kill_on_drop` reaps the child when this future ends.
            Err(PushFailure::Timeout)
        }
        Ok(Err(error)) => Err(PushFailure::Spawn(error)),
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(PushFailure::Exit(status.code())),
    }
}

fn mode_word(mode: PushMode) -> &'static str {
    match mode {
        PushMode::Trigger => "trigger",
        PushMode::Heartbeat => "heartbeat",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_im_backend::payload::{ImEvent, ImEventKind};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    fn event() -> ImEvent {
        ImEvent {
            gateway: "tg1".into(),
            platform: "telegram".into(),
            channel: "telegram".into(),
            conversation: "-100".into(),
            kind: ImEventKind::Message,
            id: "msg-1".into(),
            text: Some("hello".into()),
            at: "2026-10-10T12:00:00Z".into(),
            ..ImEvent::default()
        }
    }

    /// Write an executable script and return a `[[im.push]]` over it.
    fn script(dir: &std::path::Path, body: &str) -> PushScript {
        let path = dir.join("push.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        PushScript {
            mode: PushMode::Trigger,
            run: vec![path.to_string_lossy().into_owned()],
            timeout: Duration::from_secs(10),
            every: None,
        }
    }

    #[tokio::test]
    async fn a_script_reads_the_event_on_stdin_and_the_socket_in_its_env() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen.txt");
        let path = out.to_string_lossy().into_owned();
        // `$1` is the mode; stdin is the event.
        let script = script(
            dir.path(),
            &format!("printf '%s\\n' \"$1\" \"$ONLYNE_SOCKET\" > {path}; cat >> {path}"),
        );
        let input = PushInput::Trigger {
            role: "tg-group-1".into(),
            gateway: "tg1".into(),
            platform: "telegram".into(),
            chat: "abc123".into(),
            conversation: "-100".into(),
            event: Box::new(event()),
            decider: None,
            at: "2026-10-10T12:00:00Z".into(),
        };
        run_script(
            &script,
            &input,
            &PushEnv {
                socket: Some("/tmp/onlyne.sock".into()),
                ..PushEnv::default()
            },
            &BTreeMap::new(),
        )
        .await
        .expect("the script succeeds");
        let seen = std::fs::read_to_string(&out).unwrap();
        let mut lines = seen.lines();
        assert_eq!(lines.next(), Some("trigger"), "{seen}");
        assert_eq!(lines.next(), Some("/tmp/onlyne.sock"), "{seen}");
        let body: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(body["mode"], "trigger");
        assert_eq!(body["role"], "tg-group-1");
        assert_eq!(body["chat"], "abc123");
        assert_eq!(body["conversation"], "-100");
        assert_eq!(body["event"]["id"], "msg-1");
    }

    /// A failing script is tried again, and the event it was handed repeats
    /// with the same id, so a script can dedup on it.
    #[tokio::test]
    async fn a_failing_script_is_retried_until_it_lands() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen.txt");
        let path = out.to_string_lossy().into_owned();
        // Fail once, then read the event: the second attempt is the one that
        // writes, and it carries the same `event.id` as the first.
        let runs = dir.path().join("runs.txt");
        let count = runs.to_string_lossy().into_owned();
        let script = script(
            dir.path(),
            &format!(
                "printf 'x' >> {count}; \
                 test $(wc -c < {count}) -gt 1 || exit 7; \
                 cat > {path}"
            ),
        );
        let input = PushInput::Trigger {
            role: "tg-group-1".into(),
            gateway: "tg1".into(),
            platform: "telegram".into(),
            chat: "abc123".into(),
            conversation: "-100".into(),
            event: Box::new(event()),
            decider: None,
            at: "2026-10-10T12:00:00Z".into(),
        };
        let retry = PushRetry {
            attempts: 3,
            base: Duration::from_millis(10),
        };
        run_script_retrying(
            &script,
            &input,
            &PushEnv::default(),
            &BTreeMap::new(),
            &retry,
        )
        .await
        .expect("the retry lands");
        assert_eq!(std::fs::read_to_string(&runs).unwrap(), "xx");
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(body["event"]["id"], "msg-1");
    }

    /// Attempts are finite: a script that never succeeds reports its failure
    /// rather than spinning forever.
    #[tokio::test]
    async fn a_script_that_always_fails_reports_its_last_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut script = script(dir.path(), "exit 9");
        script.mode = PushMode::Heartbeat;
        let input = PushInput::Heartbeat {
            role: "tg-group-1".into(),
            gateway: "tg1".into(),
            platform: "telegram".into(),
            chat: "abc123".into(),
            at: "2026-10-10T12:00:00Z".into(),
        };
        let retry = PushRetry {
            attempts: 2,
            base: Duration::from_millis(10),
        };
        match run_script_retrying(
            &script,
            &input,
            &PushEnv::default(),
            &BTreeMap::new(),
            &retry,
        )
        .await
        {
            Err(PushFailure::Exit(Some(9))) => {}
            other => panic!("expected exit 9, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut script = script(dir.path(), "exit 3");
        let input = PushInput::Heartbeat {
            role: "tg-group-1".into(),
            gateway: "tg1".into(),
            platform: "telegram".into(),
            chat: "abc123".into(),
            at: "2026-10-10T12:00:00Z".into(),
        };
        script.mode = PushMode::Heartbeat;
        match run_script(&script, &input, &PushEnv::default(), &BTreeMap::new()).await {
            Err(PushFailure::Exit(Some(3))) => {}
            other => panic!("expected exit 3, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_script_that_overruns_its_bound_is_a_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let mut script = script(dir.path(), "sleep 5");
        script.timeout = Duration::from_millis(100);
        let input = PushInput::Heartbeat {
            role: "tg-group-1".into(),
            gateway: "tg1".into(),
            platform: "telegram".into(),
            chat: "abc123".into(),
            at: "2026-10-10T12:00:00Z".into(),
        };
        script.mode = PushMode::Heartbeat;
        match run_script(&script, &input, &PushEnv::default(), &BTreeMap::new()).await {
            Err(PushFailure::Timeout) => {}
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    /// The reflex's decision reaches a script as variables, so a shell script
    /// routes on it without parsing JSON.
    #[tokio::test]
    async fn the_decider_reaches_a_script_as_environment_variables() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen.txt");
        let path = out.to_string_lossy().into_owned();
        let script = script(
            dir.path(),
            &format!(
                "printf '%s\\n' \"$ONLYNE_DECIDER_SHOULD_HANDLE\" \
                 \"$ONLYNE_DECIDER_TARGET\" \"$ONLYNE_DECIDER_CONFIDENCE\" \
                 \"$ONLYNE_DECIDER_REPLY_TO\" > {path}"
            ),
        );
        let input = PushInput::Trigger {
            role: "tg-group-1".into(),
            gateway: "tg1".into(),
            platform: "telegram".into(),
            chat: "abc123".into(),
            conversation: "-100".into(),
            event: Box::new(event()),
            decider: Some(crate::decider::DeciderRouting {
                should_handle: 0.8732,
                target_role: "_supervisor".into(),
                target_confidence: 0.5,
                suggested_reply_to: Some("m-4".into()),
                reply_confidence: 0.9,
            }),
            at: "2026-10-10T12:00:00Z".into(),
        };
        run_script(&script, &input, &PushEnv::default(), &BTreeMap::new())
            .await
            .expect("the script succeeds");
        let text = std::fs::read_to_string(&out).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("0.8732"));
        assert_eq!(lines.next(), Some("_supervisor"));
        assert_eq!(lines.next(), Some("0.5000"));
        assert_eq!(lines.next(), Some("m-4"));
    }

    /// A heartbeat carries no decision, so it exports none of them.
    #[tokio::test]
    async fn a_heartbeat_exports_no_decider_variables() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen.txt");
        let path = out.to_string_lossy().into_owned();
        let script = script(
            dir.path(),
            &format!("printf '[%s]' \"$ONLYNE_DECIDER_TARGET\" > {path}"),
        );
        let mut script = script;
        script.mode = PushMode::Heartbeat;
        let input = PushInput::Heartbeat {
            role: "tg-group-1".into(),
            gateway: "tg1".into(),
            platform: "telegram".into(),
            chat: "abc123".into(),
            at: "2026-10-10T12:00:00Z".into(),
        };
        run_script(&script, &input, &PushEnv::default(), &BTreeMap::new())
            .await
            .expect("the script succeeds");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "[]");
    }
}
