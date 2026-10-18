//! `onlyne triage`: one System-1 decision over the faults a cluster carries.
//!
//! The verb gathers a fault context — from a `[[hook]]` event on stdin or from
//! the admin socket's own `query_faults` — packs it into one `state` object, and
//! sends one batched `POST /v1/systemone` request asking the questions the
//! [`ReflexSpec`] behind `triage` declares — the stock pair being which action
//! (`action`) and how serious it is (`severity`). The model returns calibrated
//! probabilities rather than text, so the confidence bound is read off the
//! answer and compared against `--min-confidence` here, in code.
//!
//! No prompt text lives in this file. The instructions, the rubrics, the
//! thresholds, and any questions beyond the stock pair come from the decider
//! spec — `<root>/.onlyne/decider.json`, `ONLYNE_DECIDER_SPEC`, or
//! `--spec <path>` — so an operator retunes the reflex without a rebuild.
//! Answers to questions this code does not interpret by name land in
//! [`TriageDecision::extra`].
//!
//! Nothing runs unless the operator says `--apply`: the default is a printed
//! diagnosis, which is what a supervisor reads before deciding. A gateway that
//! is absent, or one that answers with an error, degrades to the same printed
//! line with `action` null and the reason in `error`, so an offline tree
//! behaves exactly as it did before this verb existed.

use std::collections::BTreeMap;
use std::io::Read;

use onlyne_net::{Answer, DeciderSpec, ReflexSpec, SystemOneClient};
use onlyne_proto::{
    AdminControl, AdminOp, ControlOp, QueryFaultsArgs, RepairFail, RepairTarget, new_id,
};
use serde_json::{Value, json};

use crate::flags::GlobalFlags;
use crate::render;
use crate::runtime::{EXIT_OK, block_on};
use crate::socket::{self, SocketTarget};
use crate::wire::{self, Outbound};

/// How many open faults one triage run reads for its context.
const FAULT_PAGE: u32 = 50;

/// Flags for `onlyne triage`.
#[derive(Debug, Clone, clap::Args)]
pub struct TriageArgs {
    /// Task ID to triage; when omitted (and not `--from-hook`), triages open
    /// faults on the server.
    #[arg(long)]
    pub task: Option<String>,
    /// Read a hook event JSON object from stdin (for use in `[[hook]]` `run`
    /// commands).
    #[arg(long)]
    pub from_hook: bool,
    /// Automatically apply the recommended recovery action (`retry`, `recycle`,
    /// or `fail`) when confidence meets `--min-confidence`.
    #[arg(long)]
    pub apply: bool,
    /// Minimum System-1 confidence (`0.0..=1.0`) required to auto-apply a
    /// recovery action.
    #[arg(long, default_value_t = 0.60)]
    pub min_confidence: f64,
    /// Path to a custom decider JSON spec file (overrides `.onlyne/decider.json`
    /// and `ONLYNE_DECIDER_SPEC`).
    #[arg(long)]
    pub spec: Option<std::path::PathBuf>,
}

/// The decision one triage run reaches.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TriageDecision {
    /// The action the `action` question chose: `retry`, `recycle`, `fail`, or
    /// `escalate`.
    pub action: String,
    /// Confidence the `action` answer carries.
    pub confidence: f64,
    /// Every action label mapped to its probability.
    pub probabilities: BTreeMap<String, f64>,
    /// The severity score the `severity` question returned.
    pub severity: f64,
    /// Answers to questions this verb does not interpret by name, keyed by
    /// question id. An operator who adds a third question to the triage reflex
    /// reads its answer here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Answer>,
}

/// Ask the System One endpoint to triage `state` using one explicit reflex spec.
///
/// The spec supplies the questions, so their text and their set are the
/// operator's to change. `action` and `severity` are the two this verb acts on;
/// anything else the spec asks is passed through in [`TriageDecision::extra`].
pub async fn evaluate_triage_with_spec(
    client: &SystemOneClient,
    spec: &ReflexSpec,
    state: Value,
) -> Result<TriageDecision, String> {
    let questions = spec.render_questions(&state);
    let response = client
        .evaluate(state, questions)
        .await
        .map_err(|error| error.to_string())?;
    let action = response
        .choice("action")
        .ok_or_else(|| "the systemone answer carries no `action` choice".to_string())?;
    let severity = response
        .score("severity")
        .ok_or_else(|| "the systemone answer carries no `severity` score".to_string())?;
    let extra = response
        .answers
        .iter()
        .filter(|(id, _)| id.as_str() != "action" && id.as_str() != "severity")
        .map(|(id, answer)| (id.clone(), answer.clone()))
        .collect();
    Ok(TriageDecision {
        action: action.choice.clone(),
        confidence: action.confidence,
        probabilities: action.probabilities.clone(),
        severity: severity.score,
        extra,
    })
}

/// `onlyne triage`.
pub fn run(flags: &GlobalFlags, args: TriageArgs) -> i32 {
    block_on(run_inner(flags, args))
}

async fn run_inner(flags: &GlobalFlags, args: TriageArgs) -> i32 {
    let hook = if args.from_hook {
        Some(read_hook_event())
    } else {
        None
    };
    // `--spec <path>` wins outright; without it the tree's own
    // `.onlyne/decider.json` and `ONLYNE_DECIDER_SPEC` apply.
    let spec = match &args.spec {
        Some(path) => match DeciderSpec::load_from_path(path) {
            Ok(spec) => spec,
            Err(error) => return crate::runtime::usage_error(error.to_string()),
        },
        None => DeciderSpec::load_for_root(flags.server_root.as_deref()),
    };
    let context = gather(flags, &args, hook.as_ref()).await;
    if context.is_empty() {
        return crate::runtime::usage_error(
            "onlyne: triage found no fault to read; pass --task <id>, --from-hook, or run it \
             against a server with open faults",
        );
    }
    let client = spec.build_client_or_default(flags.server_root.as_deref());
    let decision = match evaluate_triage_with_spec(&client, &spec.triage, context.state()).await {
        Ok(decision) => decision,
        Err(reason) => {
            // An unreachable gateway is not a verb failure: the operator asked
            // for a decision, and the answer is that none was reached.
            print_decision(&context, None, None, Some(reason), flags);
            return EXIT_OK;
        }
    };
    let applied = if args.apply && decision.confidence >= args.min_confidence {
        apply_recovery(flags, &context, &decision).await
    } else {
        None
    };
    print_decision(&context, Some(&decision), applied.as_deref(), None, flags);
    EXIT_OK
}

/// Everything one triage run read before it asked the model anything.
struct TriageContext {
    task_id: Option<String>,
    hook: Option<HookEvent>,
    faults: Vec<Value>,
}

impl TriageContext {
    /// Whether this run has anything to ask about at all.
    fn is_empty(&self) -> bool {
        self.hook.is_none() && self.task_id.is_none() && self.faults.is_empty()
    }

    /// The `state` the two questions are evaluated against.
    fn state(&self) -> Value {
        let mut state = serde_json::Map::new();
        if let Some(task) = &self.task_id {
            state.insert("task_id".to_string(), json!(task));
        }
        if let Some(event) = &self.hook {
            state.insert("event".to_string(), event.raw.clone());
        }
        if !self.faults.is_empty() {
            state.insert("faults".to_string(), json!(self.faults));
        }
        Value::Object(state)
    }

    /// The task a recovery action is aimed at: the one the operator named, the
    /// one the hook event names, or the one the first fault row names.
    fn target_task(&self) -> Option<&str> {
        self.task_id
            .as_deref()
            .or_else(|| self.hook.as_ref().and_then(HookEvent::task_id))
            .or_else(|| {
                self.faults.iter().find_map(|row| {
                    row.get("task_id")
                        .and_then(Value::as_str)
                        .filter(|task| !task.is_empty())
                })
            })
    }

    /// The role a recycle has to reach. The fault rows are what name one; a run
    /// whose rows name none has no recycle to apply.
    fn recycle_role(&self) -> Option<&str> {
        if let Some(role) = self.hook.as_ref().and_then(HookEvent::role) {
            return Some(role);
        }
        self.faults.iter().find_map(|row| {
            row.get("role")
                .and_then(Value::as_str)
                .filter(|role| !role.is_empty())
        })
    }
}

/// Read the context from the operator's flags and the resolved socket.
///
/// A socket that does not resolve, or one that refuses the faults query, leaves
/// `faults` empty rather than failing the verb: the decision is still worth
/// having with the hook event or the task id alone.
async fn gather(flags: &GlobalFlags, args: &TriageArgs, hook: Option<&HookEvent>) -> TriageContext {
    let task_id = args
        .task
        .clone()
        .or_else(|| hook.and_then(HookEvent::task_id).map(str::to_string));
    let faults = match socket::resolve_socket(flags) {
        Ok(target) => query_faults(flags, &target, task_id.as_deref()).await,
        Err(_) => Vec::new(),
    };
    TriageContext {
        task_id,
        hook: hook.cloned(),
        faults,
    }
}

/// The open faults this run reads, in the shape `onlyne faults` prints them.
async fn query_faults(
    flags: &GlobalFlags,
    target: &SocketTarget,
    task_id: Option<&str>,
) -> Vec<Value> {
    let request = Outbound::admin(
        new_id(),
        AdminOp::Faults(QueryFaultsArgs {
            role: None,
            task_id: task_id.map(str::to_string),
            kind: None,
            open_only: true,
            limit: FAULT_PAGE,
        }),
    );
    let Ok(mut stream) = crate::runtime::open(flags, target).await else {
        return Vec::new();
    };
    let Ok(body) = wire::request_res(&mut stream, &request, flags.timeout_ms).await else {
        return Vec::new();
    };
    if !body.ok {
        return Vec::new();
    }
    body.data
        .as_ref()
        .and_then(|data| data.get("faults"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Run the recovery the decision names, when there is a task to run it against.
///
/// `escalate` applies nothing: it is the answer that says a person decides, and
/// a verb that acted on it would be overriding the operator. Returns the action
/// it applied, or `None` when it applied nothing.
async fn apply_recovery(
    flags: &GlobalFlags,
    context: &TriageContext,
    decision: &TriageDecision,
) -> Option<String> {
    let Some(task_id) = context.target_task() else {
        eprintln!(
            "onlyne triage: no fault names a task; pass --task <id> to say which one to recover"
        );
        return None;
    };
    let target = socket::resolve_socket(flags).ok()?;
    if target.surface != socket::Surface::Admin {
        eprintln!(
            "onlyne triage: --apply needs the admin surface; pass --server-root <dir>, or \
             --socket <path> with --as admin"
        );
        return None;
    }
    let reason = format!(
        "decider triage: {action} (confidence={confidence:.2})",
        action = decision.action,
        confidence = decision.confidence
    );
    let op = match decision.action.as_str() {
        "retry" => AdminOp::RepairRetry(RepairTarget {
            task_id: task_id.to_string(),
            reason: Some(reason),
        }),
        "fail" => AdminOp::RepairFail(RepairFail {
            task_id: task_id.to_string(),
            reason,
        }),
        "recycle" => {
            let Some(role) = context.recycle_role() else {
                eprintln!(
                    "onlyne triage: no fault names a role for task {task_id}; \
                     pass --to on `onlyne control recycle` to say where it goes"
                );
                return None;
            };
            AdminOp::Control(AdminControl {
                from: "_supervisor".to_string(),
                to: Some(role.to_string()),
                op: ControlOp::Recycle {
                    task_id: task_id.to_string(),
                    reason,
                },
            })
        }
        _ => return None,
    };
    let request = Outbound::admin(new_id(), op);
    let mut stream = match crate::runtime::open(flags, &target).await {
        Ok(stream) => stream,
        Err(code) => {
            eprintln!("onlyne triage: the recovery op was not sent (exit {code})");
            return None;
        }
    };
    match wire::request_res(&mut stream, &request, flags.timeout_ms).await {
        Ok(body) if body.ok => Some(decision.action.clone()),
        Ok(body) => {
            eprintln!(
                "onlyne triage: the server refused the recovery op: {}",
                body.error
                    .as_ref()
                    .map(|error| error.message.clone())
                    .unwrap_or_else(|| "no reason given".to_string())
            );
            None
        }
        Err(error) => {
            eprintln!("onlyne triage: the recovery op failed: {error:?}");
            None
        }
    }
}

/// One `[[hook]]` event as it arrived on stdin.
#[derive(Debug, Clone)]
struct HookEvent {
    raw: Value,
}

impl HookEvent {
    /// The task id the event names, from `data.task_id` or `task_id`.
    fn task_id(&self) -> Option<&str> {
        self.raw
            .get("data")
            .and_then(|data| data.get("task_id"))
            .or_else(|| self.raw.get("task_id"))
            .and_then(Value::as_str)
            .filter(|task| !task.is_empty())
    }

    /// The role the event names, from `data.role` or `role`.
    fn role(&self) -> Option<&str> {
        self.raw
            .get("data")
            .and_then(|data| data.get("role"))
            .or_else(|| self.raw.get("role"))
            .and_then(Value::as_str)
            .filter(|role| !role.is_empty())
    }
}

/// Read one JSON object from stdin. A stream that carries no object leaves the
/// hook event absent, which is a hook run with nothing to triage rather than a
/// verb failure.
fn read_hook_event() -> HookEvent {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let parsed: Value = serde_json::from_str(raw.trim()).unwrap_or(Value::Null);
    HookEvent { raw: parsed }
}

/// Print the one JSON line this verb answers with. `error` is present only on a
/// degraded run, where the gateway gave no decision to report.
fn print_decision(
    context: &TriageContext,
    decision: Option<&TriageDecision>,
    applied: Option<&str>,
    error: Option<String>,
    flags: &GlobalFlags,
) {
    let mut data = json!({
        "task_id": context.target_task(),
        "action": decision.map(|d| d.action.clone()),
        "confidence": decision.map(|d| d.confidence),
        "severity": decision.map(|d| d.severity),
        "probabilities": decision.map(|d| d.probabilities.clone()),
        "applied": applied,
        "error": error,
    });
    // `extra` carries the answers to custom questions the spec added, so it is
    // omitted entirely when the reflex asks only the stock pair.
    if let Some(decision) = decision
        && !decision.extra.is_empty()
        && let Some(map) = data.as_object_mut()
    {
        map.insert("extra".to_string(), json!(decision.extra));
    }
    let body = json!({ "ok": true, "data": data });
    println!("{}", render::encode(flags.pretty, &body));
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_net::Question;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A mock System One endpoint answering `answers` on its one connection,
    /// handing the request it received back to the test.
    ///
    /// The request is captured rather than asserted inside the server task: a
    /// panic in a spawned task would not fail the test that caused it.
    async fn serve_once(answers: Value) -> (String, Arc<Mutex<Option<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let capture = Arc::clone(&seen);
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]);
            let body = request
                .split_once("\r\n\r\n")
                .map(|(_, body)| body)
                .unwrap_or_default();
            let parsed: Value = serde_json::from_str(body).unwrap();
            *capture.lock().unwrap() = Some(parsed);
            let bytes = serde_json::to_vec(&answers).unwrap();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                bytes.len()
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(&bytes).await.unwrap();
        });
        (format!("http://{addr}"), seen)
    }

    #[tokio::test]
    async fn evaluate_triage_reads_action_confidence_and_severity() {
        let (base, seen) = serve_once(json!({
            "model": "jev-latest",
            "answers": {
                "action": {
                    "type": "choice",
                    "choice": "recycle",
                    "confidence": 0.87,
                    "probabilities": {
                        "recycle": 0.87, "retry": 0.10, "fail": 0.02, "escalate": 0.01
                    }
                },
                "severity": {
                    "type": "score",
                    "score": 1.5,
                    "confidence": 0.9,
                    "probabilities": { "0": 0.1, "1": 0.7, "2": 0.2 }
                }
            }
        }))
        .await;

        let client = SystemOneClient::new(base, "jev-latest");
        let spec = DeciderSpec::defaults().triage;
        let decision = evaluate_triage_with_spec(&client, &spec, json!({"task_id": "t-1"}))
            .await
            .unwrap();
        assert_eq!(decision.action, "recycle");
        assert_eq!(decision.confidence, 0.87);
        assert_eq!(decision.severity, 1.5);
        assert_eq!(decision.probabilities.get("retry"), Some(&0.10));
        // The stock reflex asks exactly the two questions the verb acts on, so
        // nothing is left over for `extra`.
        assert!(decision.extra.is_empty());
        let request = seen.lock().unwrap().clone().expect("the request arrived");
        assert_eq!(request["questions"]["action"]["type"], "choice");
        assert_eq!(request["questions"]["severity"]["type"], "score");
        assert_eq!(request["state"]["task_id"], "t-1");
    }

    #[tokio::test]
    async fn evaluate_triage_refuses_an_answer_with_no_action() {
        let (base, _seen) = serve_once(json!({
            "model": "jev-latest",
            "answers": {
                "severity": { "type": "score", "score": 0.0, "confidence": 0.5 }
            }
        }))
        .await;

        let client = SystemOneClient::new(base, "jev-latest");
        let spec = DeciderSpec::defaults().triage;
        let error = evaluate_triage_with_spec(&client, &spec, json!({"task_id": "t-1"}))
            .await
            .unwrap_err();
        assert!(error.contains("action"), "unexpected error: {error}");
    }

    #[test]
    fn the_stock_spec_asks_action_and_severity() {
        let spec = DeciderSpec::defaults().triage;
        let questions = spec.render_questions(&json!({}));
        assert_eq!(questions.len(), 2);
        assert!(matches!(questions["action"], Question::Choice { .. }));
        assert!(matches!(questions["severity"], Question::Score { .. }));
    }

    /// A spec is the operator's to author: `{task_id}` in an instruction is
    /// filled from the state, and a question the verb does not interpret by
    /// name comes back in `extra` rather than being dropped.
    #[tokio::test]
    async fn a_custom_spec_interpolates_state_and_keeps_extra_answers() {
        let mut spec = DeciderSpec::defaults().triage;
        spec.questions.insert(
            "action".to_string(),
            Question::choice(
                "What recovery does task {task_id} need?",
                [("retry", Some("transient".to_string()))],
            ),
        );
        spec.questions.insert(
            "owner".to_string(),
            Question::choice(
                "Who should pick up task {task_id}?",
                [("_coder", Some("the coding role".to_string()))],
            ),
        );

        let (base, seen) = serve_once(json!({
            "model": "jev-latest",
            "answers": {
                "action": {
                    "type": "choice",
                    "choice": "retry",
                    "confidence": 0.75,
                    "probabilities": { "retry": 0.75 }
                },
                "severity": {
                    "type": "score",
                    "score": 0.0,
                    "confidence": 0.6,
                    "probabilities": { "0": 0.8, "1": 0.2 }
                },
                "owner": {
                    "type": "choice",
                    "choice": "_coder",
                    "confidence": 0.91,
                    "probabilities": { "_coder": 0.91 }
                }
            }
        }))
        .await;

        let client = SystemOneClient::new(base, "jev-latest");
        let decision = evaluate_triage_with_spec(&client, &spec, json!({"task_id": "t-42"}))
            .await
            .unwrap();
        assert_eq!(decision.action, "retry");
        assert_eq!(decision.severity, 0.0);

        let request = seen.lock().unwrap().clone().expect("the request arrived");
        assert_eq!(
            request["questions"]["action"]["instructions"],
            "What recovery does task t-42 need?"
        );
        assert_eq!(
            request["questions"]["owner"]["instructions"],
            "Who should pick up task t-42?"
        );
        assert_eq!(
            request["questions"]["owner"]["criteria"]["_coder"],
            "the coding role"
        );

        let owner = decision
            .extra
            .get("owner")
            .expect("the custom answer is kept");
        assert!(matches!(owner, Answer::Choice(answer) if answer.choice == "_coder"));
        // The two the verb interprets are not duplicated into `extra`.
        assert!(!decision.extra.contains_key("action"));
        assert!(!decision.extra.contains_key("severity"));
    }

    #[test]
    fn a_hook_event_names_its_task_and_role_from_either_spelling() {
        let event = HookEvent {
            raw: json!({"data": {"task_id": "t-nested", "role": "_coder"}}),
        };
        assert_eq!(event.task_id(), Some("t-nested"));
        assert_eq!(event.role(), Some("_coder"));
        let event = HookEvent {
            raw: json!({"task_id": "t-flat"}),
        };
        assert_eq!(event.task_id(), Some("t-flat"));
        assert_eq!(event.role(), None);
    }

    #[test]
    fn the_state_carries_what_the_run_read() {
        let context = TriageContext {
            task_id: Some("t-1".to_string()),
            hook: Some(HookEvent {
                raw: json!({"type": "fault"}),
            }),
            faults: vec![json!({"task_id": "t-1", "kind": "stalled"})],
        };
        let state = context.state();
        assert_eq!(state["task_id"], "t-1");
        assert_eq!(state["event"]["type"], "fault");
        assert_eq!(state["faults"][0]["kind"], "stalled");
        assert_eq!(context.target_task(), Some("t-1"));
        assert!(!context.is_empty());
    }

    #[test]
    fn the_target_task_falls_back_to_the_first_fault_row() {
        let context = TriageContext {
            task_id: None,
            hook: None,
            faults: vec![
                json!({"task_id": null, "kind": "spec_reload_failed"}),
                json!({"task_id": "t-9", "role": "_coder"}),
            ],
        };
        assert_eq!(context.target_task(), Some("t-9"));
        assert_eq!(context.recycle_role(), Some("_coder"));
    }

    #[test]
    fn a_run_that_read_nothing_has_no_state_to_ask_about() {
        let context = TriageContext {
            task_id: None,
            hook: None,
            faults: Vec::new(),
        };
        assert!(context.is_empty());
        assert_eq!(context.target_task(), None);
        assert_eq!(context.recycle_role(), None);
    }
}
