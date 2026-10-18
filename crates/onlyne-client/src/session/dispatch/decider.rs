//! The client's System One reflexes: what a decision model reads out of one
//! session's own words (`docs/v2.9-SYSTEMONE-DECIDER-PLAN.md` §2.3).
//!
//! Three checkpoints sit inside this client's dispatch, and each one asks a
//! question the deterministic rules underneath cannot answer:
//!
//! * a completion that claims `done` while its own text says otherwise
//!   ([`verify_done_outcome`]),
//! * a turn that ended without a completion, where the agent's last words say
//!   why ([`diagnose_turn_end_suffix`]),
//! * a beat whose observation carries prose, which is the only place a
//!   thrashing loop or a stalled session is visible
//!   ([`analyze_heartbeat_json`]).
//!
//! Every call is one batched `POST /v1/systemone`, because the questions at one
//! checkpoint are independent and the endpoint answers a map of them in one
//! round trip. Every call degrades to `None`: a client this process never
//! configured (`SystemOneClient::from_env` answering `None`), an endpoint that
//! refuses, times out, or answers a shape these functions did not ask for — all
//! leave the caller exactly the behaviour it had before this module existed.
//! Nothing here is authoritative: the verdict, the nudge and the observation are
//! still this client's own, and a decision model only rewrites the words they
//! carry.
//!
//! The gates in front of each call are what keep the offline path free of
//! network traffic. A completion report whose head and details together are
//! shorter than [`MIN_REPORT_BYTES`] is a synthetic frame — a unit test's
//! `"done"`, a probe's placeholder — and is never asked about; a beat that
//! carries no prose at all is a lifecycle tuple and is never asked about. The
//! scenario suite runs `ONLYNE_BACKEND=fake`, which
//! `SystemOneClient::from_env` already refuses, so both gates are a second lock
//! on a door that is already shut.

use super::*;

use std::collections::BTreeMap;

use onlyne_net::{Question, SystemOneClient};
use serde_json::Value;

/// The shortest completion report worth asking about.
///
/// A real report names what it did: a file, a command, a verdict. A synthetic
/// one — the unit test's `"done"`, the probe's `"mine"` — is a handful of bytes
/// that say nothing a decision model could contradict, and asking anyway would
/// spend a round trip per test frame to learn nothing.
pub const MIN_REPORT_BYTES: usize = 24;

/// The shortest closing text worth diagnosing.
///
/// Shorter than a sentence is a fragment, and a fragment says nothing about
/// whether the agent finished, is waiting, or broke.
pub const MIN_CLOSING_BYTES: usize = 16;

/// How sure the contradiction judgement has to be before a `done` is rewritten.
///
/// The question is loaded: it asks whether the report *states* the work is
/// unfinished, so a high probability is the model saying the sentence is there
/// in the text rather than inferred from it. Below this the completion stands as
/// the agent filed it.
pub const CONTRADICTION_FLOOR: f64 = 0.85;

/// How sure the status judgement has to be before a `done` is rewritten.
///
/// The choice is over three labels the model has to separate, and its
/// `confidence` is the concentration of that distribution. A near-uniform
/// answer is the model saying it cannot tell, which is not grounds to override
/// the agent's own verdict.
pub const STATUS_FLOOR: f64 = 0.65;

/// How sure the turn-end diagnosis has to be before the nudge changes.
///
/// A nudge that names the wrong tool call is worse than the neutral one, so the
/// bar is where a confident answer starts rather than where a probable one does.
pub const DIAGNOSIS_FLOOR: f64 = 0.60;

/// The observation fields that carry prose about what the session is doing.
///
/// A lifecycle tuple (`agent`, `resource`, `recovery`, …) is a dimension, and a
/// decision model has nothing to read in one. The four keys a plugin may add
/// beside them are the session's own account of its work, and that is the only
/// evidence a thrashing loop or a phase ever appears in.
const PROSE_KEYS: [&str; 4] = ["note", "text", "tool", "output"];

/// Read one completion report against its own claim, and answer the outcome it
/// should have carried.
///
/// A `done` is the agent's own verdict and this client records it as filed. What
/// it cannot see is the contradiction inside the report: `details` that open
/// with *"I could not finish because the credentials are missing"* settle a task
/// `done` and the board reads it as finished work. The report's own words are
/// the evidence, and a decision model is what reads them.
///
/// Two questions are asked together because they answer one judgement:
/// whether the text states the work is unfinished, and what status it actually
/// describes. The first alone cannot act — a `failed` task's report reads as a
/// contradiction too, and rewriting it `blocked` would be wrong — and the
/// second alone cannot act either, because `blocked` in a report that also says
/// the work is done is the honest answer for a task that waited and then
/// landed. Both floors have to clear.
///
/// `None` is every other answer: the text agrees, the model is unsure, the
/// endpoint did not answer, or the report is too short to hold a contradiction.
/// The caller then settles exactly what the agent reported.
pub async fn verify_done_outcome(
    client: &SystemOneClient,
    task_id: &str,
    head: Option<&str>,
    details: Option<&str>,
) -> Option<(Outcome, String)> {
    let head = head.unwrap_or("");
    let details = details.unwrap_or("");
    if head.len() + details.len() < MIN_REPORT_BYTES {
        return None;
    }

    let mut questions = BTreeMap::new();
    questions.insert(
        "contradicts_done".to_string(),
        Question::noul_with_criteria(
            "Does this completion report state that the work is actually unfinished, blocked on external input, or failed with an unresolved error?",
            "States the task could not be finished, is blocked, or failed",
            "Reports completed work",
        ),
    );
    questions.insert(
        "actual_status".to_string(),
        Question::choice(
            "What is the true status described by this report?",
            [
                ("done", Some("Work was completed".to_string())),
                (
                    "blocked",
                    Some("Blocked waiting on user, permission, or external dependency".to_string()),
                ),
                (
                    "failed",
                    Some("Failed due to an unrecoverable error".to_string()),
                ),
            ],
        ),
    );

    let state = serde_json::json!({
        "task_id": task_id,
        "head": head,
        "details": details,
    });
    let response = client.evaluate(state, questions).await.ok()?;
    let contradicts_done = response.noul("contradicts_done")?;
    let status = response.choice("actual_status")?;
    if contradicts_done < CONTRADICTION_FLOOR || status.confidence < STATUS_FLOOR {
        return None;
    }
    let adjusted = match status.choice.as_str() {
        "blocked" => Outcome::Blocked,
        "failed" => Outcome::Failed,
        // `done` is the verdict the agent filed, and a model that agrees with it
        // has nothing to say. Any other spelling is an answer this client did
        // not ask for, which is not grounds to rewrite a verdict either.
        _ => return None,
    };
    Some((
        adjusted,
        format!(
            "decider: completion text contradicts outcome=done (noul={contradicts_done:.2}, choice={})",
            status.choice
        ),
    ))
}

/// Ask why one turn ended without a completion, and answer the sentence that
/// names the way out.
///
/// The neutral nudge asks the agent to report its work. That is the right ask
/// for the commonest ending — the model finished, summarized, and never called
/// the tool — and it is the wrong ask for the other two: a session waiting on a
/// credential is told to report `done`, and a session that just hit an
/// unrecoverable error is told the same thing. The agent's own closing words
/// separate the three, and a decision model reads them where the client's
/// deterministic rules see only a turn that stopped.
///
/// The suffix rides the client's own `NUDGE_TEXT` rather than replacing it: the
/// standard sentence is a contract the plugin composes none of
/// (`docs/v2-CONTRACT.md` §3c), so what this adds is a second sentence naming
/// the one outcome the situation calls for. `None` keeps the neutral nudge.
pub async fn diagnose_turn_end_suffix(
    client: &SystemOneClient,
    task_id: &str,
    closing: Option<&str>,
) -> Option<&'static str> {
    let closing = closing?;
    if closing.len() < MIN_CLOSING_BYTES {
        return None;
    }

    let mut questions = BTreeMap::new();
    questions.insert(
        "reason".to_string(),
        Question::choice(
            "Why did the agent end its turn without calling onlyne_complete?",
            [
                (
                    "forgot_complete",
                    Some(
                        "Finished the work and summarized it without calling onlyne_complete"
                            .to_string(),
                    ),
                ),
                (
                    "blocked",
                    Some("Waiting on external input or blocker".to_string()),
                ),
                ("error", Some("Hit an error and stopped".to_string())),
            ],
        ),
    );

    let state = serde_json::json!({
        "task_id": task_id,
        "closing": closing,
    });
    let response = client.evaluate(state, questions).await.ok()?;
    let reason = response.choice("reason")?;
    if reason.confidence < DIAGNOSIS_FLOOR {
        return None;
    }
    Some(match reason.choice.as_str() {
        "forgot_complete" => {
            " If your work is finished, call onlyne_complete with outcome=\"done\" now."
        }
        "blocked" => {
            " If you are blocked on an external dependency, call onlyne_complete with outcome=\"blocked\" now."
        }
        "error" => {
            " If the task cannot succeed due to an error, call onlyne_complete with outcome=\"failed\" now."
        }
        _ => return None,
    })
}

/// Read one beat's prose, and answer the projection a board renders.
///
/// The observation a heartbeat carries is six dimensions and, on some drives,
/// the session's own account of what it is doing. The dimensions answer
/// *whether* a session is alive; the prose is the only place *how* it is going
/// is visible, and a session repeating one failing command looks perfectly
/// healthy on the dimensions alone. Three questions are asked together because
/// they read the same text and the endpoint answers a map in one round trip.
///
/// The answer rides the beat as `observed.decider`, which is where the TUI
/// reads the phase tag from and where a repair tool can see the thrashing mark
/// after the fact. It is a sibling of `host` rather than a dimension: nothing in
/// the reducer reads it and `is_legal` never constrains it, so a beat that
/// carries no prose keeps the exact bytes it carried before.
pub async fn analyze_heartbeat_json(
    client: &SystemOneClient,
    task_id: &str,
    observed: &Value,
) -> Option<Value> {
    if !carries_prose(observed) {
        return None;
    }

    let mut questions = BTreeMap::new();
    questions.insert(
        "is_thrashing".to_string(),
        Question::noul(
            "Is the agent stuck in a repetitive error loop repeating the same failing action?",
        ),
    );
    questions.insert(
        "progress".to_string(),
        Question::score(
            "How well is the session progressing?",
            [
                "0: stuck in repetitive error loop",
                "1: debugging errors",
                "2: making steady progress",
                "3: verifying and wrapping up",
            ],
        ),
    );
    questions.insert(
        "phase".to_string(),
        Question::choice(
            "Which activity phase is the agent currently in?",
            [
                ("reading", None),
                ("planning", None),
                ("coding", None),
                ("testing", None),
                ("debugging", None),
                ("waiting", None),
                ("wrapping_up", None),
            ],
        ),
    );

    let state = serde_json::json!({
        "task_id": task_id,
        "observed": observed,
    });
    let response = client.evaluate(state, questions).await.ok()?;
    let is_thrashing = response.noul("is_thrashing")?;
    let progress = response.score("progress")?;
    let phase = response.choice("phase")?;
    Some(serde_json::json!({
        "phase": phase.choice,
        "confidence": phase.confidence,
        "progress": progress.score,
        "thrashing": is_thrashing,
    }))
}

/// Whether one observation carries any prose about the work.
///
/// A lifecycle tuple is dimensions alone, and every beat the offline suite
/// drives is exactly that: asking a decision model what phase a session is in
/// from `{"agent": "running", "resource": "attached"}` spends a round trip to
/// learn nothing. A key that is present but empty is the same absence.
fn carries_prose(observed: &Value) -> bool {
    let Some(fields) = observed.as_object() else {
        return false;
    };
    PROSE_KEYS.iter().any(|key| match fields.get(*key) {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Null) | None => false,
        // A structured activity summary — the tool call a plugin reports — is
        // prose about the work even though it is not a string.
        Some(_) => true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    /// A mock System One endpoint that answers once and counts its callers.
    ///
    /// The counter is what a gate test reads: a call that should never have been
    /// made leaves it at zero, which is a stronger claim than "the answer was
    /// `None`" — a dead endpoint would answer that too.
    async fn mock(
        answers: serde_json::Value,
    ) -> (SystemOneClient, oneshot::Receiver<String>, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = oneshot::channel();
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&hits);
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            counted.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0u8; 64 * 1024];
            let Ok(n) = stream.read(&mut buf).await else {
                return;
            };
            let raw = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = raw
                .split_once("\r\n\r\n")
                .map(|(_, body)| body.to_string())
                .unwrap_or_default();
            let _ = seen_tx.send(body);
            let bytes = serde_json::to_vec(&answers).unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&bytes).await;
        });
        (
            SystemOneClient::new(format!("http://{addr}"), "group/decider"),
            seen_rx,
            hits,
        )
    }

    /// A listener nothing ever accepts from, so a test can prove a gate held.
    async fn unreachable_client() -> (SystemOneClient, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&hits);
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = stream.shutdown().await;
            }
        });
        (
            SystemOneClient::new(format!("http://{addr}"), "group/decider"),
            hits,
        )
    }

    fn answers(contradicts: f64, status: &str, confidence: f64) -> serde_json::Value {
        serde_json::json!({
            "model": "group/decider",
            "answers": {
                "contradicts_done": { "type": "noul", "noul": contradicts },
                "actual_status": {
                    "type": "choice",
                    "choice": status,
                    "confidence": confidence,
                    "probabilities": { status: confidence }
                }
            }
        })
    }

    /// A completion whose text says it is waiting settles blocked, and the
    /// reason names both numbers the judgement was made on.
    #[tokio::test]
    async fn a_contradicted_done_report_is_downgraded_to_blocked() {
        let (client, seen_rx, _hits) = mock(answers(0.92, "blocked", 0.71)).await;
        let (adjusted, reason) = verify_done_outcome(
            &client,
            "t-1",
            Some("installed the dependency"),
            Some("but the deploy is still blocked on the operator's credential"),
        )
        .await
        .expect("the endpoint answered with a contradiction");

        assert_eq!(adjusted, Outcome::Blocked);
        assert_eq!(
            reason,
            "decider: completion text contradicts outcome=done (noul=0.92, choice=blocked)"
        );

        let body: Value = serde_json::from_str(&seen_rx.await.unwrap()).expect("a JSON body");
        assert_eq!(body["state"]["task_id"], "t-1");
        assert_eq!(
            body["state"]["details"],
            "but the deploy is still blocked on the operator's credential"
        );
        let questions = body["questions"].as_object().expect("a question map");
        assert_eq!(questions.len(), 2, "one batched request, two questions");
        assert_eq!(questions["contradicts_done"]["type"], "noul");
        assert_eq!(
            questions["contradicts_done"]["criteria"]["true"],
            "States the task could not be finished, is blocked, or failed"
        );
        let statuses = questions["actual_status"]["criteria"]
            .as_object()
            .expect("a closed option set");
        assert!(statuses.contains_key("done"));
        assert!(statuses.contains_key("blocked"));
        assert!(statuses.contains_key("failed"));
    }

    /// An error the report names is a failure, not a wait.
    #[tokio::test]
    async fn a_contradicted_done_report_is_downgraded_to_failed() {
        let (client, _seen_rx, _hits) = mock(answers(0.97, "failed", 0.83)).await;
        let (adjusted, reason) = verify_done_outcome(
            &client,
            "t-2",
            Some("wrote the migration"),
            Some("and the suite fails with an unresolved compile error in the bridge"),
        )
        .await
        .expect("the endpoint answered with a contradiction");

        assert_eq!(adjusted, Outcome::Failed);
        assert_eq!(
            reason,
            "decider: completion text contradicts outcome=done (noul=0.97, choice=failed)"
        );
    }

    /// A report the model agrees with is left exactly as the agent filed it.
    #[tokio::test]
    async fn an_agreed_done_report_is_left_alone() {
        let (client, _seen_rx, _hits) = mock(answers(0.04, "done", 0.95)).await;
        let adjusted = verify_done_outcome(
            &client,
            "t-3",
            Some("moved the reducer"),
            Some("and the scenario suite passes with the new transition rows"),
        )
        .await;
        assert!(adjusted.is_none(), "an honest report stands as filed");
    }

    /// Either floor alone is not enough: a contradiction nobody is sure about,
    /// and a status nobody is sure about, both leave the verdict alone.
    #[tokio::test]
    async fn a_low_score_or_a_low_confidence_leaves_the_verdict_alone() {
        let (client, _seen_rx, _hits) = mock(answers(0.99, "blocked", 0.30)).await;
        assert!(
            verify_done_outcome(
                &client,
                "t-4",
                Some("wrote the parser"),
                Some("and something about the token stream still reads wrong here"),
            )
            .await
            .is_none(),
            "an unconfident status is not grounds to override the agent"
        );

        let (client, _seen_rx, _hits) = mock(answers(0.40, "blocked", 0.99)).await;
        assert!(
            verify_done_outcome(
                &client,
                "t-5",
                Some("wrote the parser"),
                Some("and the token stream reads the way the fixture says it should"),
            )
            .await
            .is_none(),
            "a contradiction the text does not state is not one"
        );
    }

    /// A short synthetic report is never asked about, which is what keeps the
    /// offline suite free of network traffic.
    #[tokio::test]
    async fn a_short_completion_report_is_never_asked_about() {
        let (client, hits) = unreachable_client().await;
        assert!(
            verify_done_outcome(&client, "t-6", Some("done"), Some("mine"))
                .await
                .is_none(),
            "a synthetic report is not evidence of anything"
        );
        assert!(
            verify_done_outcome(&client, "t-7", None, None)
                .await
                .is_none(),
            "and neither is an empty one"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no request was made");
    }

    /// An endpoint that does not answer is a `None`, not a failure.
    #[tokio::test]
    async fn a_dead_endpoint_leaves_the_verdict_alone() {
        let client = SystemOneClient::new("http://127.0.0.1:1", "group/decider")
            .with_timeout(std::time::Duration::from_millis(200));
        assert!(
            verify_done_outcome(
                &client,
                "t-8",
                Some("wrote the parser"),
                Some("and the fixture suite passes with the new token rows here"),
            )
            .await
            .is_none()
        );
    }

    /// An answer missing the noul is not a judgement.
    #[tokio::test]
    async fn an_unusable_answer_leaves_the_verdict_alone() {
        let answers = serde_json::json!({
            "model": "group/decider",
            "answers": {
                "actual_status": { "type": "choice", "choice": "blocked", "confidence": 0.9 }
            }
        });
        let (client, _seen_rx, _hits) = mock(answers).await;
        assert!(
            verify_done_outcome(
                &client,
                "t-9",
                Some("wrote the parser"),
                Some("and the fixture suite passes with the new token rows here"),
            )
            .await
            .is_none()
        );
    }

    fn closing_answers(reason: &str, confidence: f64) -> serde_json::Value {
        serde_json::json!({
            "model": "group/decider",
            "answers": {
                "reason": {
                    "type": "choice",
                    "choice": reason,
                    "confidence": confidence,
                    "probabilities": { reason: confidence }
                }
            }
        })
    }

    /// Each diagnosis names the one outcome that situation calls for, and rides
    /// the client's own nudge rather than replacing it.
    #[tokio::test]
    async fn each_diagnosis_names_its_own_outcome() {
        for (reason, expected) in [
            (
                "forgot_complete",
                " If your work is finished, call onlyne_complete with outcome=\"done\" now.",
            ),
            (
                "blocked",
                " If you are blocked on an external dependency, call onlyne_complete with outcome=\"blocked\" now.",
            ),
            (
                "error",
                " If the task cannot succeed due to an error, call onlyne_complete with outcome=\"failed\" now.",
            ),
        ] {
            let (client, seen_rx, _hits) = mock(closing_answers(reason, 0.78)).await;
            let suffix = diagnose_turn_end_suffix(
                &client,
                "t-10",
                Some("I have finished the refactor and here is what changed in the bridge"),
            )
            .await
            .expect("the endpoint answered");
            assert_eq!(suffix, expected);

            let body: Value = serde_json::from_str(&seen_rx.await.unwrap()).expect("a JSON body");
            assert_eq!(body["state"]["task_id"], "t-10");
            let questions = body["questions"].as_object().expect("a question map");
            assert_eq!(questions.len(), 1, "one question, one round trip");
            let options = questions["reason"]["criteria"]
                .as_object()
                .expect("a closed option set");
            assert!(options.contains_key("forgot_complete"));
            assert!(options.contains_key("blocked"));
            assert!(options.contains_key("error"));
        }
    }

    /// An unsure diagnosis keeps the neutral nudge.
    #[tokio::test]
    async fn an_unsure_diagnosis_adds_nothing() {
        let (client, _seen_rx, _hits) = mock(closing_answers("blocked", 0.44)).await;
        assert!(
            diagnose_turn_end_suffix(
                &client,
                "t-11",
                Some("I have finished the refactor and here is what changed in the bridge"),
            )
            .await
            .is_none(),
            "a nudge naming the wrong tool call is worse than the neutral one"
        );
    }

    /// A plugin beat carries no words, so there is nothing to diagnose.
    #[tokio::test]
    async fn no_closing_text_is_never_asked_about() {
        let (client, hits) = unreachable_client().await;
        assert!(
            diagnose_turn_end_suffix(&client, "t-12", None)
                .await
                .is_none()
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no request was made");
    }

    /// A fragment is not a closing text either.
    #[tokio::test]
    async fn a_fragment_of_closing_text_is_never_asked_about() {
        let (client, hits) = unreachable_client().await;
        assert!(
            diagnose_turn_end_suffix(&client, "t-13", Some("done"))
                .await
                .is_none()
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no request was made");
    }

    fn heartbeat_answers(
        thrashing: f64,
        progress: f64,
        phase: &str,
        confidence: f64,
    ) -> serde_json::Value {
        serde_json::json!({
            "model": "group/decider",
            "answers": {
                "is_thrashing": { "type": "noul", "noul": thrashing },
                "progress": {
                    "type": "score",
                    "score": progress,
                    "confidence": 0.9,
                    "probabilities": { "2": 0.9 }
                },
                "phase": {
                    "type": "choice",
                    "choice": phase,
                    "confidence": confidence,
                    "probabilities": { phase: confidence }
                }
            }
        })
    }

    /// One beat with prose becomes the projection the board renders, and the
    /// three questions travel as one request.
    #[tokio::test]
    async fn a_beat_with_prose_gets_a_phase_and_a_thrashing_score() {
        let (client, seen_rx, _hits) = mock(heartbeat_answers(0.91, 1.0, "debugging", 0.82)).await;
        let observed = serde_json::json!({
            "agent": "running",
            "note": "cargo test failed on the bridge again; rerunning the same case"
        });
        let decider = analyze_heartbeat_json(&client, "t-14", &observed)
            .await
            .expect("the endpoint answered");

        assert_eq!(
            decider,
            serde_json::json!({
                "phase": "debugging",
                "confidence": 0.82,
                "progress": 1.0,
                "thrashing": 0.91,
            })
        );

        let body: Value = serde_json::from_str(&seen_rx.await.unwrap()).expect("a JSON body");
        assert_eq!(body["state"]["task_id"], "t-14");
        assert_eq!(body["state"]["observed"]["note"], observed["note"]);
        let questions = body["questions"].as_object().expect("a question map");
        assert_eq!(questions.len(), 3, "one batched request, three questions");
        assert_eq!(questions["is_thrashing"]["type"], "noul");
        assert_eq!(questions["progress"]["type"], "score");
        assert_eq!(
            questions["progress"]["criteria"].as_array().unwrap().len(),
            4
        );
        let phases = questions["phase"]["criteria"]
            .as_object()
            .expect("a closed option set");
        for word in [
            "reading",
            "planning",
            "coding",
            "testing",
            "debugging",
            "waiting",
            "wrapping_up",
        ] {
            assert!(phases.contains_key(word), "the phase set names {word}");
        }
    }

    /// A structured activity summary is prose too, even though it is not a
    /// string.
    #[tokio::test]
    async fn a_structured_tool_summary_counts_as_prose() {
        let (client, _seen_rx, _hits) = mock(heartbeat_answers(0.02, 2.0, "coding", 0.88)).await;
        let observed = serde_json::json!({
            "agent": "running",
            "tool": { "name": "edit", "path": "crates/onlyne-client/src/lib.rs" }
        });
        let decider = analyze_heartbeat_json(&client, "t-15", &observed)
            .await
            .expect("the endpoint answered");
        assert_eq!(decider["phase"], "coding");
    }

    /// A lifecycle tuple is dimensions alone, which is what every beat the
    /// offline suite drives: it is never asked about at all.
    #[tokio::test]
    async fn a_bare_lifecycle_beat_is_never_asked_about() {
        let (client, hits) = unreachable_client().await;
        let observed = serde_json::json!({
            "version": { "generation": 1, "seq": 1005 },
            "generation_live": true,
            "agent": "running",
            "resource": "attached",
            "recovery": "none",
        });
        assert!(
            analyze_heartbeat_json(&client, "t-16", &observed)
                .await
                .is_none(),
            "a dimension is not something a decision model can read"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no request was made");
    }

    /// An empty prose key is the same as no prose key.
    #[tokio::test]
    async fn an_empty_prose_key_is_never_asked_about() {
        let (client, hits) = unreachable_client().await;
        for observed in [
            serde_json::json!({"agent": "idle", "note": ""}),
            serde_json::json!({"agent": "idle", "note": "   "}),
            serde_json::json!({"agent": "idle", "text": null}),
        ] {
            assert!(
                analyze_heartbeat_json(&client, "t-17", &observed)
                    .await
                    .is_none(),
                "an empty {observed} says nothing"
            );
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no request was made");
    }

    /// An observation that is not an object has no prose to read.
    #[tokio::test]
    async fn a_non_object_observation_is_never_asked_about() {
        let (client, hits) = unreachable_client().await;
        assert!(
            analyze_heartbeat_json(&client, "t-18", &Value::Null)
                .await
                .is_none()
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no request was made");
    }

    /// An answer missing one of the three questions is not a projection.
    #[tokio::test]
    async fn an_unusable_answer_is_not_a_projection() {
        let answers = serde_json::json!({
            "model": "group/decider",
            "answers": {
                "is_thrashing": { "type": "noul", "noul": 0.5 },
                "phase": { "type": "choice", "choice": "coding", "confidence": 0.9 }
            }
        });
        let (client, _seen_rx, _hits) = mock(answers).await;
        let observed = serde_json::json!({"agent": "running", "note": "editing the reducer"});
        assert!(
            analyze_heartbeat_json(&client, "t-19", &observed)
                .await
                .is_none()
        );
    }

    /// The answer type has to be the one the question asked for.
    #[tokio::test]
    async fn a_mistyped_answer_is_not_a_projection() {
        let answers = serde_json::json!({
            "model": "group/decider",
            "answers": {
                "is_thrashing": { "type": "score", "score": 2.0 },
                "progress": { "type": "score", "score": 2.0, "confidence": 0.9 },
                "phase": { "type": "choice", "choice": "coding", "confidence": 0.9 }
            }
        });
        let (client, _seen_rx, _hits) = mock(answers).await;
        let observed = serde_json::json!({"agent": "running", "note": "editing the reducer"});
        assert!(
            analyze_heartbeat_json(&client, "t-20", &observed)
                .await
                .is_none()
        );
    }
}
