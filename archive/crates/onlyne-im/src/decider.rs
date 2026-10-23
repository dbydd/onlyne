//! The inbound reflex arc: what a System One decision adds to one event.
//!
//! A conversation the gateway serves is a doorway into a cluster that already
//! knows how to work. Most of what arrives there is work, and some of it is a
//! human talking to a human. Telling the two apart is a judgment, and a
//! judgment is what the TypeSafe System One API (`POST /v1/systemone`) answers
//! in one batched request (`docs/v2.9-SYSTEMONE-DECIDER-PLAN.md` §2.2):
//!
//! * **`should_handle`** (`noul`) — is this message asking the cluster for
//!   work, an answer, or task context?
//! * **`target_role`** (`choice`) — which role of the cluster should take it?
//! * **`reply_to_msg`** (`choice`) — which recent message this one continues,
//!   when the platform reported no reply of its own.
//! * **`delivery_kind`** (`choice`) — does the agent want this as a stateless
//!   note, or as a tracked task?
//!
//! The answers ride along with the event: on [`crate::trigger::PushInput`], so
//! an operator's script reads them off stdin, and as `ONLYNE_DECIDER_*`
//! environment variables, so a shell script routes on them without parsing
//! anything.
//!
//! Every call degrades to `None`: a gateway with no System One endpoint
//! configured, an endpoint that refuses, times out, or answers with a shape
//! this module did not ask for, all leave the trigger scripts exactly the
//! input they had before this module existed. Nothing here is authoritative —
//! the platform and the operator's own script decide what actually happens.

use std::collections::BTreeMap;

use onlyne_im_backend::payload::ImEvent;
use onlyne_net::{Answer, DeciderSpec, Question, ReflexSpec, SystemOneClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The role an unroutable message is left with: not the cluster's business.
pub const IGNORE_ROLE: &str = "ignore";

/// The role a message with no better match goes to: the cluster's operator.
pub const DEFAULT_CANDIDATE: &str = "_supervisor";

/// How many of the most recent message ids a reply-to question may name.
///
/// The question is read by a decision model, not by a resolver: a closed set of
/// eight is what keeps the answerable space small enough to be confident in,
/// and the ids stay full rather than shortened because the question asks which
/// *message* this one continues, not which token it starts with.
pub const MAX_REPLY_CANDIDATES: usize = 8;

/// What the System One endpoint decided about one inbound event.
///
/// `should_handle` is the probability the cluster has business here at all;
/// `target_role` and `target_confidence` name where it belongs; the reply pair
/// is what an implicit thread link would ride on, and is `None` when the
/// platform already reported a reply or the decision found nothing to
/// continue; `delivery_kind` asks whether the event is a conversational note or
/// a tracked task, and falls back to a note when the endpoint says nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeciderRouting {
    /// Probability in `[0.0, 1.0]` that the cluster should take this message.
    pub should_handle: f64,
    /// The role the message is for, or [`IGNORE_ROLE`].
    pub target_role: String,
    /// Concentration of the role decision in `[0.0, 1.0]`.
    pub target_confidence: f64,
    /// The message this one continues, when the decision named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_reply_to: Option<String>,
    /// Concentration of that reply decision in `[0.0, 1.0]`.
    pub reply_confidence: f64,
    /// Suggested delivery kind (`"note"` or `"task"`), defaulting to `"note"`.
    #[serde(default = "default_delivery_kind")]
    pub delivery_kind: String,
    /// Concentration of the delivery kind decision in `[0.0, 1.0]`.
    #[serde(default)]
    pub kind_confidence: f64,
    /// Additional answers from custom questions declared in `im_inbound.questions`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Answer>,
}

fn default_delivery_kind() -> String {
    "note".to_string()
}

impl DeciderRouting {
    /// `should_handle` as the four decimals a shell comparison reads.
    pub fn should_handle_str(&self) -> String {
        format!("{:.4}", self.should_handle)
    }

    /// `target_confidence` as the same four decimals.
    pub fn target_confidence_str(&self) -> String {
        format!("{:.4}", self.target_confidence)
    }
}

/// Ask System One how one inbound event should be routed, in one request.
///
/// `candidate_roles` are the roles the cluster offers; empty means the
/// operator's own seat. `recent_msg_ids` are the platform ids the gateway
/// already holds for this conversation, oldest first, and only ever feed the
/// reply question — a message the platform said answers something is not asked
/// about again.
///
/// Every failure returns `None`, which is the whole degradation contract: an
/// unreachable, slow, or unhelpful endpoint costs the caller nothing but the
/// wait.
pub async fn evaluate_inbound(
    client: &SystemOneClient,
    inbound: &ImEvent,
    candidate_roles: &[String],
    recent_msg_ids: &[String],
) -> Option<DeciderRouting> {
    let spec = DeciderSpec::from_env();
    evaluate_inbound_with_spec(
        client,
        &spec.im_inbound,
        inbound,
        candidate_roles,
        recent_msg_ids,
    )
    .await
}

/// Ask System One how one inbound event should be routed using `spec`.
pub async fn evaluate_inbound_with_spec(
    client: &SystemOneClient,
    spec: &ReflexSpec,
    inbound: &ImEvent,
    candidate_roles: &[String],
    recent_msg_ids: &[String],
) -> Option<DeciderRouting> {
    if !spec.enabled {
        return None;
    }
    let state = serde_json::json!({
        "platform": inbound.platform,
        "conversation": inbound.conversation,
        "sender": inbound.from.as_ref().map(|from| from.id.clone()),
        "text": inbound.text,
        "has_reply_parent": inbound.reply.is_some(),
        "recent_message_ids": recent_msg_ids,
    });
    let mut questions = spec.render_questions(&state);

    let default_candidate = spec.template("default_candidate", DEFAULT_CANDIDATE);
    let ignore_role = spec.template("ignore_role", IGNORE_ROLE);
    let ignore_rubric = spec.template("ignore_rubric", "Human chatter not directed at any agent");
    let none_reply_label = spec.template("none_reply_label", "none");
    let none_reply_rubric = spec.template("none_reply_rubric", "Starts a new topic");
    let max_reply = spec.threshold("max_reply_candidates", MAX_REPLY_CANDIDATES as f64) as usize;

    if let Some(Question::Choice { criteria, .. }) = questions.get_mut("target_role") {
        for (role, rubric) in role_options_with(
            candidate_roles,
            default_candidate,
            ignore_role,
            ignore_rubric,
        ) {
            criteria
                .entry(role)
                .or_insert_with(|| rubric.map(Value::String));
        }
    }

    let wants_reply = inbound.reply.is_none() && !recent_msg_ids.is_empty();
    if wants_reply {
        if let Some(Question::Choice { criteria, .. }) = questions.get_mut("reply_to_msg") {
            for (id, rubric) in reply_options_with(
                recent_msg_ids,
                max_reply,
                none_reply_label,
                none_reply_rubric,
            ) {
                criteria
                    .entry(id)
                    .or_insert_with(|| rubric.map(Value::String));
            }
        }
    } else {
        questions.remove("reply_to_msg");
    }

    let response = client.evaluate(state, questions).await.ok()?;

    let should_handle = response.noul("should_handle")?;
    let target = response.choice("target_role")?;
    let extra: BTreeMap<String, Answer> = response
        .answers
        .iter()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "should_handle" | "target_role" | "reply_to_msg" | "delivery_kind"
            )
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let (delivery_kind, kind_confidence) = match response.choice("delivery_kind") {
        Some(choice) if choice.choice == "task" || choice.choice == "note" => {
            (choice.choice.clone(), choice.confidence)
        }
        _ => (default_delivery_kind(), 0.0),
    };
    let mut routing = DeciderRouting {
        should_handle,
        target_role: target.choice.clone(),
        target_confidence: target.confidence,
        suggested_reply_to: None,
        reply_confidence: 0.0,
        delivery_kind,
        kind_confidence,
        extra,
    };
    if wants_reply {
        if let Some(reply) = response.choice("reply_to_msg") {
            routing.reply_confidence = reply.confidence;
            if reply.choice != none_reply_label {
                routing.suggested_reply_to = Some(reply.choice.clone());
            }
        }
    }
    Some(routing)
}

/// The closed set the role question answers over: the cluster's roles, then
/// the way out.
#[cfg(test)]
fn role_options(candidate_roles: &[String]) -> Vec<(String, Option<String>)> {
    let spec = DeciderSpec::defaults().im_inbound;
    role_options_with(
        candidate_roles,
        spec.template("default_candidate", DEFAULT_CANDIDATE),
        spec.template("ignore_role", IGNORE_ROLE),
        spec.template("ignore_rubric", "Human chatter not directed at any agent"),
    )
}

fn role_options_with(
    candidate_roles: &[String],
    default_candidate: &str,
    ignore_role: &str,
    ignore_rubric: &str,
) -> Vec<(String, Option<String>)> {
    // Bound before the branch, so the empty case has a slice that outlives it.
    let default = [default_candidate.to_string()];
    let roles: &[String] = if candidate_roles.is_empty() {
        &default
    } else {
        candidate_roles
    };
    let mut options: Vec<(String, Option<String>)> =
        roles.iter().map(|role| (role.clone(), None)).collect();
    options.push((ignore_role.to_string(), Some(ignore_rubric.to_string())));
    options
}

/// The reply question's set: the newest few ids, then the way out.
fn reply_options_with(
    recent_msg_ids: &[String],
    max_reply: usize,
    none_label: &str,
    none_rubric: &str,
) -> Vec<(String, Option<String>)> {
    let start = recent_msg_ids.len().saturating_sub(max_reply);
    let mut options: Vec<(String, Option<String>)> = recent_msg_ids[start..]
        .iter()
        .map(|id| (id.clone(), None))
        .collect();
    options.push((none_label.to_string(), Some(none_rubric.to_string())));
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_im_backend::payload::{ImEvent, ImEventKind, ImReply, ImSender};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn event() -> ImEvent {
        ImEvent {
            gateway: "tg1".into(),
            platform: "telegram".into(),
            channel: "telegram".into(),
            conversation: "-100".into(),
            kind: ImEventKind::Message,
            id: "msg-9".into(),
            from: Some(ImSender {
                id: "u1".into(),
                name: None,
            }),
            text: Some("please fix the build".into()),
            at: "2026-10-10T12:00:00Z".into(),
            ..ImEvent::default()
        }
    }

    /// A mock System One endpoint that answers every question it is asked, and
    /// hands back the request body it read so a test can check the batching.
    async fn mock_server(
        answers: serde_json::Value,
    ) -> (SystemOneClient, tokio::sync::oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16 * 1024];
            let n = stream.read(&mut buf).await.unwrap();
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
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(&bytes).await.unwrap();
        });
        (
            SystemOneClient::new(format!("http://{addr}"), "jev-latest"),
            seen_rx,
        )
    }

    fn answers_json(should_handle: f64, role: &str, confidence: f64) -> serde_json::Value {
        serde_json::json!({
            "model": "jev-latest",
            "answers": {
                "should_handle": { "type": "noul", "noul": should_handle },
                "target_role": {
                    "type": "choice",
                    "choice": role,
                    "confidence": confidence,
                    "probabilities": { role: confidence }
                },
                "reply_to_msg": {
                    "type": "choice",
                    "choice": "none",
                    "confidence": 0.1,
                    "probabilities": { "none": 0.1 }
                }
            }
        })
    }

    /// One batched request carries all three questions, and the answers become
    /// one `DeciderRouting`.
    #[tokio::test]
    async fn one_request_routes_the_message_to_a_role() {
        let (client, seen_rx) = mock_server(answers_json(0.91, "_supervisor", 0.77)).await;
        let recent = vec!["m1".to_string(), "m2".to_string()];
        let routing = evaluate_inbound(
            &client,
            &event(),
            &["_supervisor".to_string(), "builder".to_string()],
            &recent,
        )
        .await
        .expect("the endpoint answered");

        assert_eq!(
            routing,
            DeciderRouting {
                should_handle: 0.91,
                target_role: "_supervisor".into(),
                target_confidence: 0.77,
                suggested_reply_to: None,
                reply_confidence: 0.1,
                delivery_kind: "note".into(),
                kind_confidence: 0.0,
                extra: BTreeMap::new(),
            }
        );

        let body: serde_json::Value =
            serde_json::from_str(&seen_rx.await.unwrap()).expect("a JSON body");
        let state = &body["state"];
        assert_eq!(state["platform"], "telegram");
        assert_eq!(state["conversation"], "-100");
        assert_eq!(state["sender"], "u1");
        assert_eq!(state["text"], "please fix the build");
        assert_eq!(state["has_reply_parent"], false);
        assert_eq!(state["recent_message_ids"], serde_json::json!(["m1", "m2"]));
        let questions = body["questions"].as_object().expect("a question map");
        assert_eq!(questions.len(), 4, "one batched request, four questions");
        assert_eq!(questions["should_handle"]["type"], "noul");
        assert_eq!(
            questions["should_handle"]["criteria"]["true"],
            "Message asks for work, answers, status, or provides task context"
        );
        let roles = questions["target_role"]["criteria"]
            .as_object()
            .expect("a closed option set");
        assert!(roles.contains_key("_supervisor"));
        assert!(roles.contains_key("builder"));
        assert!(roles.contains_key("ignore"));
        let reply = questions["reply_to_msg"]["criteria"]
            .as_object()
            .expect("a closed option set");
        assert!(reply.contains_key("m1"));
        assert!(reply.contains_key("m2"));
        assert!(reply.contains_key("none"));
        let kind = questions["delivery_kind"]["criteria"]
            .as_object()
            .expect("a closed option set");
        assert!(kind.contains_key("note"));
        assert!(kind.contains_key("task"));
    }

    /// An empty candidate list still asks over a role: the operator's seat.
    #[tokio::test]
    async fn no_candidate_roles_falls_back_to_the_operator() {
        let (client, seen_rx) = mock_server(answers_json(0.5, "ignore", 0.0)).await;
        let routing = evaluate_inbound(&client, &event(), &[], &[]).await.unwrap();
        assert_eq!(routing.target_role, "ignore");
        assert!(routing.suggested_reply_to.is_none());
        assert_eq!(routing.delivery_kind, "note");

        let body: serde_json::Value = serde_json::from_str(&seen_rx.await.unwrap()).unwrap();
        let questions = body["questions"].as_object().unwrap();
        // No history to continue, so the reply question is not asked at all.
        assert_eq!(questions.len(), 3, "no reply question without history");
        assert!(
            questions["target_role"]["criteria"]
                .get("_supervisor")
                .is_some()
        );
    }

    /// The platform's own answer stands: no reply coordinate, no question.
    #[tokio::test]
    async fn a_message_the_platform_placed_is_not_asked_about_again() {
        let (client, seen_rx) = mock_server(answers_json(1.0, "builder", 0.4)).await;
        let mut placed = event();
        placed.reply = Some(ImReply {
            parent: Some("m1".into()),
            ..ImReply::default()
        });
        let routing = evaluate_inbound(
            &client,
            &placed,
            &["builder".to_string()],
            &["m1".to_string()],
        )
        .await
        .unwrap();
        assert!(routing.suggested_reply_to.is_none());
        assert_eq!(routing.reply_confidence, 0.0);

        let body: serde_json::Value = serde_json::from_str(&seen_rx.await.unwrap()).unwrap();
        assert_eq!(body["state"]["has_reply_parent"], true);
        assert!(
            body["questions"].get("reply_to_msg").is_none(),
            "the platform already said which message this answers"
        );
    }

    /// The delivery-kind answer rides out, and is not also left in `extra`.
    #[tokio::test]
    async fn a_returned_delivery_kind_becomes_task() {
        let answers = serde_json::json!({
            "model": "jev-latest",
            "answers": {
                "should_handle": { "type": "noul", "noul": 0.9 },
                "target_role": { "type": "choice", "choice": "builder", "confidence": 0.7 },
                "delivery_kind": {
                    "type": "choice",
                    "choice": "task",
                    "confidence": 0.83,
                    "probabilities": { "task": 0.83, "note": 0.17 }
                }
            }
        });
        let (client, _seen_rx) = mock_server(answers).await;
        let routing = evaluate_inbound(&client, &event(), &["builder".to_string()], &[])
            .await
            .unwrap();
        assert_eq!(routing.delivery_kind, "task");
        assert_eq!(routing.kind_confidence, 0.83);
        assert!(routing.extra.is_empty(), "the answer is not duplicated");
    }

    /// A decision that names a message to continue rides out as one.
    #[tokio::test]
    async fn a_chosen_message_becomes_the_suggested_reply_to() {
        let answers = serde_json::json!({
            "model": "jev-latest",
            "answers": {
                "should_handle": { "type": "noul", "noul": 0.8 },
                "target_role": { "type": "choice", "choice": "builder", "confidence": 0.6 },
                "reply_to_msg": { "type": "choice", "choice": "m2", "confidence": 0.72 }
            }
        });
        let (client, _seen_rx) = mock_server(answers).await;
        let recent = vec!["m1".to_string(), "m2".to_string()];
        let routing = evaluate_inbound(&client, &event(), &["builder".to_string()], &recent)
            .await
            .unwrap();
        assert_eq!(routing.suggested_reply_to.as_deref(), Some("m2"));
        assert_eq!(routing.reply_confidence, 0.72);
    }

    /// The newest eight ids are what the question may name.
    #[tokio::test]
    async fn only_the_newest_ids_are_offered() {
        let (client, seen_rx) = mock_server(answers_json(0.1, "ignore", 0.2)).await;
        let recent: Vec<String> = (1..=12).map(|n| format!("m{n}")).collect();
        evaluate_inbound(&client, &event(), &["_supervisor".to_string()], &recent)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(&seen_rx.await.unwrap()).unwrap();
        let options = body["questions"]["reply_to_msg"]["criteria"]
            .as_object()
            .unwrap();
        assert_eq!(options.len(), MAX_REPLY_CANDIDATES + 1);
        assert!(options.contains_key("m5"), "the oldest four scrolled past");
        assert!(options.contains_key("m12"));
    }

    /// An endpoint that refuses the connection is a `None`, not a failure.
    #[tokio::test]
    async fn a_dead_endpoint_degrades_to_nothing() {
        // A port nothing listens on: the client's own timeout and the
        // connection refusal both have to land as `None`.
        let client = SystemOneClient::new("http://127.0.0.1:1", "jev-latest")
            .with_timeout(std::time::Duration::from_millis(200));
        let routing = evaluate_inbound(&client, &event(), &["_supervisor".to_string()], &[]).await;
        assert!(routing.is_none());
    }

    /// An answer missing the noul is not a routing.
    #[tokio::test]
    async fn an_unusable_answer_degrades_to_nothing() {
        let answers = serde_json::json!({
            "model": "jev-latest",
            "answers": { "target_role": { "type": "choice", "choice": "builder" } }
        });
        let (client, _seen_rx) = mock_server(answers).await;
        assert!(
            evaluate_inbound(&client, &event(), &["builder".to_string()], &[])
                .await
                .is_none()
        );
    }

    /// The answer type has to be the one the question asked for.
    #[tokio::test]
    async fn a_mistyped_answer_degrades_to_nothing() {
        let answers = serde_json::json!({
            "model": "jev-latest",
            "answers": {
                "should_handle": { "type": "score", "score": 1.0 },
                "target_role": { "type": "choice", "choice": "builder" }
            }
        });
        let (client, _seen_rx) = mock_server(answers).await;
        assert!(
            evaluate_inbound(&client, &event(), &["builder".to_string()], &[])
                .await
                .is_none()
        );
    }

    /// The shape round-trips, because it rides as JSON on a trigger's stdin.
    #[test]
    fn the_routing_round_trips_as_json() {
        let routing = DeciderRouting {
            should_handle: 0.5,
            target_role: "builder".into(),
            target_confidence: 0.25,
            suggested_reply_to: Some("m2".into()),
            reply_confidence: 0.75,
            delivery_kind: "task".into(),
            kind_confidence: 0.6,
            extra: BTreeMap::new(),
        };
        let text = serde_json::to_string(&routing).unwrap();
        assert!(text.contains("\"suggested_reply_to\":\"m2\""));
        assert!(text.contains("\"delivery_kind\":\"task\""));
        let back: DeciderRouting = serde_json::from_str(&text).unwrap();
        assert_eq!(back, routing);

        let bare = DeciderRouting {
            should_handle: 0.0,
            target_role: "ignore".into(),
            target_confidence: 0.0,
            suggested_reply_to: None,
            reply_confidence: 0.0,
            delivery_kind: "note".into(),
            kind_confidence: 0.0,
            extra: BTreeMap::new(),
        };
        assert!(
            !serde_json::to_string(&bare)
                .unwrap()
                .contains("suggested_reply_to"),
            "an absent reply stays absent"
        );

        let partial: DeciderRouting = serde_json::from_str(
            r#"{"should_handle":0.0,"target_role":"ignore","target_confidence":0.0,"reply_confidence":0.0}"#,
        )
        .unwrap();
        assert_eq!(partial.delivery_kind, "note");
        assert_eq!(partial.kind_confidence, 0.0);
    }

    /// The question set is a closed one over the roles offered, whatever the
    /// order or the repetition.
    #[test]
    fn the_role_question_offers_the_roles_and_the_way_out() {
        let roles = vec!["builder".to_string(), "reviewer".to_string()];
        let options = role_options(&roles);
        let keys: Vec<&str> = options.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["builder", "reviewer", "ignore"]);
        assert_eq!(
            options.last().unwrap().1.as_deref(),
            Some("Human chatter not directed at any agent")
        );
    }

    #[tokio::test]
    async fn a_custom_spec_interpolates_state_and_captures_extra_answers() {
        let answers = serde_json::json!({
            "model": "jev-latest",
            "answers": {
                "should_handle": { "type": "noul", "noul": 0.95 },
                "target_role": { "type": "choice", "choice": "builder", "confidence": 0.85 },
                "is_urgent": { "type": "noul", "noul": 0.99 }
            }
        });
        let (client, seen_rx) = mock_server(answers).await;
        let mut spec = DeciderSpec::defaults().im_inbound;
        spec.questions.insert(
            "should_handle".to_string(),
            Question::noul("Custom prompt for {platform}"),
        );
        spec.questions.insert(
            "is_urgent".to_string(),
            Question::noul("Is chat {conversation} urgent?"),
        );
        let routing =
            evaluate_inbound_with_spec(&client, &spec, &event(), &["builder".to_string()], &[])
                .await
                .unwrap();
        assert_eq!(
            routing.extra.get("is_urgent"),
            Some(&Answer::Noul(onlyne_net::NoulAnswer { noul: 0.99 }))
        );
        let body: serde_json::Value = serde_json::from_str(&seen_rx.await.unwrap()).unwrap();
        assert_eq!(
            body["questions"]["should_handle"]["instructions"],
            "Custom prompt for telegram"
        );
        assert_eq!(
            body["questions"]["is_urgent"]["instructions"],
            "Is chat -100 urgent?"
        );
    }
}
