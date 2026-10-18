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
use onlyne_net::{Question, SystemOneClient};
use serde::{Deserialize, Serialize};

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
/// continue.
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
    let mut questions = BTreeMap::new();
    questions.insert(
        "should_handle".to_string(),
        Question::noul_with_criteria(
            "Should the coding-agent cluster handle or respond to this message?",
            "Message asks for work, answers, status, or provides task context",
            "Unrelated human smalltalk or noise to ignore",
        ),
    );
    questions.insert(
        "target_role".to_string(),
        Question::choice(
            "Which role in the cluster should handle this message?",
            role_options(candidate_roles),
        ),
    );
    let wants_reply = inbound.reply.is_none() && !recent_msg_ids.is_empty();
    if wants_reply {
        questions.insert(
            "reply_to_msg".to_string(),
            Question::choice(
                "Which recent message ID is this message continuing or replying to?",
                reply_options(recent_msg_ids),
            ),
        );
    }

    let state = serde_json::json!({
        "platform": inbound.platform,
        "conversation": inbound.conversation,
        "sender": inbound.from.as_ref().map(|from| from.id.clone()),
        "text": inbound.text,
        "recent_message_ids": recent_msg_ids,
    });
    let response = client.evaluate(state, questions).await.ok()?;

    let should_handle = response.noul("should_handle")?;
    let target = response.choice("target_role")?;
    let mut routing = DeciderRouting {
        should_handle,
        target_role: target.choice.clone(),
        target_confidence: target.confidence,
        suggested_reply_to: None,
        reply_confidence: 0.0,
    };
    if wants_reply {
        if let Some(reply) = response.choice("reply_to_msg") {
            routing.reply_confidence = reply.confidence;
            if reply.choice != "none" {
                routing.suggested_reply_to = Some(reply.choice.clone());
            }
        }
    }
    Some(routing)
}

/// The closed set the role question answers over: the cluster's roles, then
/// the way out.
fn role_options(candidate_roles: &[String]) -> Vec<(String, Option<String>)> {
    // Bound before the branch, so the empty case has a slice that outlives it.
    let default = [DEFAULT_CANDIDATE.to_string()];
    let roles: &[String] = if candidate_roles.is_empty() {
        &default
    } else {
        candidate_roles
    };
    let mut options: Vec<(String, Option<String>)> =
        roles.iter().map(|role| (role.clone(), None)).collect();
    options.push((
        IGNORE_ROLE.to_string(),
        Some("Human chatter not directed at any agent".to_string()),
    ));
    options
}

/// The reply question's set: the newest few ids, then the way out.
fn reply_options(recent_msg_ids: &[String]) -> Vec<(String, Option<String>)> {
    let start = recent_msg_ids.len().saturating_sub(MAX_REPLY_CANDIDATES);
    let mut options: Vec<(String, Option<String>)> = recent_msg_ids[start..]
        .iter()
        .map(|id| (id.clone(), None))
        .collect();
    options.push(("none".to_string(), Some("Starts a new topic".to_string())));
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
            SystemOneClient::new(format!("http://{addr}"), "group/decider"),
            seen_rx,
        )
    }

    fn answers_json(should_handle: f64, role: &str, confidence: f64) -> serde_json::Value {
        serde_json::json!({
            "model": "group/decider",
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
            }
        );

        let body: serde_json::Value =
            serde_json::from_str(&seen_rx.await.unwrap()).expect("a JSON body");
        let state = &body["state"];
        assert_eq!(state["platform"], "telegram");
        assert_eq!(state["conversation"], "-100");
        assert_eq!(state["sender"], "u1");
        assert_eq!(state["text"], "please fix the build");
        assert_eq!(state["recent_message_ids"], serde_json::json!(["m1", "m2"]));
        let questions = body["questions"].as_object().expect("a question map");
        assert_eq!(questions.len(), 3, "one batched request, three questions");
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
    }

    /// An empty candidate list still asks over a role: the operator's seat.
    #[tokio::test]
    async fn no_candidate_roles_falls_back_to_the_operator() {
        let (client, seen_rx) = mock_server(answers_json(0.5, "ignore", 0.0)).await;
        let routing = evaluate_inbound(&client, &event(), &[], &[]).await.unwrap();
        assert_eq!(routing.target_role, "ignore");
        assert!(routing.suggested_reply_to.is_none());

        let body: serde_json::Value = serde_json::from_str(&seen_rx.await.unwrap()).unwrap();
        let questions = body["questions"].as_object().unwrap();
        // No history to continue, so the reply question is not asked at all.
        assert_eq!(questions.len(), 2, "no reply question without history");
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
        assert!(
            body["questions"].get("reply_to_msg").is_none(),
            "the platform already said which message this answers"
        );
    }

    /// A decision that names a message to continue rides out as one.
    #[tokio::test]
    async fn a_chosen_message_becomes_the_suggested_reply_to() {
        let answers = serde_json::json!({
            "model": "group/decider",
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
        let client = SystemOneClient::new("http://127.0.0.1:1", "group/decider")
            .with_timeout(std::time::Duration::from_millis(200));
        let routing = evaluate_inbound(&client, &event(), &["_supervisor".to_string()], &[]).await;
        assert!(routing.is_none());
    }

    /// An answer missing the noul is not a routing.
    #[tokio::test]
    async fn an_unusable_answer_degrades_to_nothing() {
        let answers = serde_json::json!({
            "model": "group/decider",
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
            "model": "group/decider",
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
        };
        let text = serde_json::to_string(&routing).unwrap();
        assert!(text.contains("\"suggested_reply_to\":\"m2\""));
        let back: DeciderRouting = serde_json::from_str(&text).unwrap();
        assert_eq!(back, routing);

        let bare = DeciderRouting {
            should_handle: 0.0,
            target_role: "ignore".into(),
            target_confidence: 0.0,
            suggested_reply_to: None,
            reply_confidence: 0.0,
        };
        assert!(
            !serde_json::to_string(&bare)
                .unwrap()
                .contains("suggested_reply_to"),
            "an absent reply stays absent"
        );
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
}
