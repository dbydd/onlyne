//! The backend protocol is a wire contract, so its cases pin bytes.

use onlyne_im_backend::frame::{
    BackendOp, CoreOp, HelloArgs, LoginArgs, LoginResult, OnboardingKind, OnboardingPrompt,
    SendArgs,
};
use onlyne_im_backend::payload::{ImAction, ImEvent, ImEventKind, ImFormat, ImPayload, ImSender};

/// The auth half is a wire contract too: an unauthenticated backend says so in
/// its hello, and names the credential an operator must provide.
#[test]
fn a_hello_without_a_credential_says_so_and_names_what_is_missing() {
    let msg = BackendOp::Hello(HelloArgs {
        platform: "telegram".into(),
        version: "1".into(),
        capabilities: vec!["message".into()],
        authenticated: false,
        onboarding: Some(OnboardingPrompt {
            kind: OnboardingKind::ManualCode,
            payload: "paste the bot token".into(),
            expires_in: None,
        }),
    });
    let value = serde_json::to_value(&msg).unwrap();
    assert_eq!(value["args"]["authenticated"], false);
    assert_eq!(value["args"]["onboarding"]["kind"], "manual_code");
    assert_eq!(
        value["args"]["onboarding"]["payload"],
        "paste the bot token"
    );
    let back: BackendOp = serde_json::from_value(value).unwrap();
    assert_eq!(back, msg);
}

/// A pasted credential travels one way, and the answer names the identity the
/// platform reported, so an operator can see whose account they logged in as.
#[test]
fn a_login_round_trip_carries_the_credential_and_the_identity() {
    let request = CoreOp::Login(LoginArgs {
        gateway: "tg1".into(),
        credential: "123:abc".into(),
    });
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["op"], "login");
    assert_eq!(value["args"]["gateway"], "tg1");
    let back: CoreOp = serde_json::from_value(value).unwrap();
    assert_eq!(back, request);

    let answer = BackendOp::Login(LoginResult {
        ok: true,
        reason: None,
        identity: Some("onlyne_bot".into()),
    });
    let value = serde_json::to_value(&answer).unwrap();
    assert_eq!(value["op"], "login");
    assert_eq!(value["args"]["ok"], true);
    assert_eq!(value["args"]["identity"], "onlyne_bot");
    let back: BackendOp = serde_json::from_value(value).unwrap();
    assert_eq!(back, answer);
}

/// A refused credential is a sentence, not a dead process: the answer carries
/// the reason so an operator reads what was wrong with the token.
#[test]
fn a_refused_login_answers_with_its_reason() {
    let answer = BackendOp::Login(LoginResult {
        ok: false,
        reason: Some("Telegram refused this token".into()),
        identity: None,
    });
    let value = serde_json::to_value(&answer).unwrap();
    assert_eq!(value["args"]["ok"], false);
    assert_eq!(value["args"]["reason"], "Telegram refused this token");
    assert!(value["args"].get("identity").is_none());
}

#[test]
fn a_hello_frame_carries_the_platform_and_its_capabilities() {
    let msg = BackendOp::Hello(HelloArgs {
        platform: "telegram".into(),
        version: "2.1.2".into(),
        capabilities: vec!["message".into(), "action".into()],
        authenticated: true,
        onboarding: None,
    });
    let value = serde_json::to_value(&msg).unwrap();
    assert_eq!(value["op"], "hello");
    assert_eq!(value["args"]["platform"], "telegram");
    assert_eq!(value["args"]["capabilities"][1], "action");
    let back: BackendOp = serde_json::from_value(value).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn a_send_frame_carries_the_standard_payload_not_a_platform_shape() {
    let msg = CoreOp::Send(SendArgs {
        conversation: "12345".into(),
        payload: ImPayload {
            text: "*hi*".into(),
            format: ImFormat::Markdown,
            attachments: vec![],
            actions: vec![ImAction {
                id: "approve".into(),
                label: "Allow".into(),
                style: Some("primary".into()),
            }],
            reply_to: Some("msg-7".into()),
            reply_reference: None,
        },
    });
    let value = serde_json::to_value(&msg).unwrap();
    assert_eq!(value["op"], "send");
    assert_eq!(value["args"]["conversation"], "12345");
    assert_eq!(value["args"]["payload"]["format"], "markdown");
    assert_eq!(value["args"]["payload"]["actions"][0]["id"], "approve");
    assert_eq!(value["args"]["payload"]["reply_to"], "msg-7");
    let back: CoreOp = serde_json::from_value(value).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn an_inbound_event_is_the_shape_a_trigger_script_reads() {
    let event = ImEvent {
        gateway: "im-tg1".into(),
        platform: "telegram".into(),
        channel: "telegram".into(),
        conversation: "12345".into(),
        kind: ImEventKind::Message,
        id: "msg-9".into(),
        from: Some(ImSender {
            id: "u-1".into(),
            name: Some("Ada".into()),
        }),
        text: Some("deploy please".into()),
        attachments: vec![],
        action: None,
        reply: Some(onlyne_im_backend::payload::ImReply {
            parent: Some("msg-8".into()),
            root: Some("msg-1".into()),
            thread: None,
            quoted_text: Some("the message it answers".into()),
            self_handle: Some("REFIDX_quoted".into()),
        }),
        at: "2026-10-10T12:00:00Z".into(),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["kind"], "message");
    assert_eq!(value["id"], "msg-9");
    assert_eq!(value["from"]["id"], "u-1");
    // The coordinate a thread platform reports rides through to the script,
    // which is what lets a reply into an unrecorded discussion name its anchor.
    assert_eq!(value["reply"]["parent"], "msg-8");
    assert_eq!(value["reply"]["root"], "msg-1");
    assert_eq!(value["reply"]["quoted_text"], "the message it answers");
    // A platform that hands out a second id for quoting rides it through too.
    assert_eq!(value["reply"]["self_handle"], "REFIDX_quoted");
    // A coordinate the platform did not report is absent, like `action`.
    assert!(value["reply"].get("thread").is_none(), "{value}");
    // An absent action is absent, not null: a script branches on presence.
    assert!(value.get("action").is_none(), "{value}");
    // The round trip is exact — this is the object a script receives.
    let back: ImEvent = serde_json::from_value(value).unwrap();
    assert_eq!(back, event);
}

#[test]
fn an_event_that_answers_nothing_omits_the_coordinate_entirely() {
    // The v2.3 shape byte for byte: a platform that reports no reply link
    // sends the same object it always did.
    let event = ImEvent {
        gateway: "im-qq".into(),
        platform: "qqbot".into(),
        conversation: "c2c:u-1".into(),
        id: "m-1".into(),
        text: Some("hi".into()),
        at: "2026-10-14T00:00:00Z".into(),
        ..ImEvent::default()
    };
    let value = serde_json::to_value(&event).unwrap();
    assert!(value.get("reply").is_none(), "{value}");
}

#[test]
fn an_empty_payload_omits_what_it_does_not_carry() {
    let payload = ImPayload {
        text: "ok".into(),
        ..ImPayload::default()
    };
    let value = serde_json::to_value(&payload).unwrap();
    assert_eq!(value["text"], "ok");
    assert_eq!(value["format"], "plain");
    assert!(value.get("attachments").is_none(), "{value}");
    assert!(value.get("actions").is_none(), "{value}");
    assert!(value.get("reply_to").is_none(), "{value}");
}

#[test]
fn an_action_event_names_the_button_that_was_pressed() {
    let event = ImEvent {
        kind: ImEventKind::Action,
        action: Some("approve".into()),
        ..ImEvent::default()
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["kind"], "action");
    assert_eq!(value["action"], "approve");
}

#[test]
fn a_send_answers_with_the_ids_the_platform_handed_back() {
    // The frame that keeps a gateway's own messages in the reply chain: the
    // message id a later reply is sent against, and — on a platform with a
    // second one — the handle a later reply quotes it by.
    let sent = onlyne_im_backend::frame::SentArgs {
        conversation: "group:F67E0292".into(),
        message_id: Some("ROBOT1.0_sohz".into()),
        ref_handle: Some("REFIDX_4qO5".into()),
        error: None,
    };
    let value = serde_json::to_value(BackendOp::Sent(sent.clone())).unwrap();
    assert_eq!(value["op"], "sent");
    assert_eq!(value["args"]["message_id"], "ROBOT1.0_sohz");
    assert_eq!(value["args"]["ref_handle"], "REFIDX_4qO5");
    let back: BackendOp = serde_json::from_value(value).unwrap();
    assert_eq!(back, BackendOp::Sent(sent));
}

#[test]
fn a_send_a_platform_named_nothing_for_omits_both_ids() {
    // A failed send still answers, so a caller waiting for an id is not left
    // waiting; the absent id is a state the record already handles.
    let sent = onlyne_im_backend::frame::SentArgs {
        conversation: "-100".into(),
        ..Default::default()
    };
    let value = serde_json::to_value(BackendOp::Sent(sent)).unwrap();
    assert!(value["args"].get("message_id").is_none(), "{value}");
    assert!(value["args"].get("ref_handle").is_none(), "{value}");
    assert_eq!(value["args"]["conversation"], "-100");
}

/// A refusal travels in the answer, not only on the backend's stderr: the
/// gateway is the side that records what happened to a conversation, and it
/// cannot read a log it does not own.
#[test]
fn a_refused_send_answers_with_the_platforms_own_words() {
    let sent = onlyne_im_backend::frame::SentArgs {
        conversation: "group:F67E0292".into(),
        error: Some(
            "/v2/groups/…/messages refused (400 Bad Request): {\"message\":\"回复消息msg_id已过期\",\"code\":40034005}"
                .into(),
        ),
        ..Default::default()
    };
    let value = serde_json::to_value(BackendOp::Sent(sent)).unwrap();
    assert!(
        value["args"]["error"]
            .as_str()
            .unwrap()
            .contains("40034005"),
        "{value}"
    );
    assert!(value["args"].get("message_id").is_none(), "{value}");
}
