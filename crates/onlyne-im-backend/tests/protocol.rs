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
        at: "2026-10-10T12:00:00Z".into(),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["kind"], "message");
    assert_eq!(value["id"], "msg-9");
    assert_eq!(value["from"]["id"], "u-1");
    // An absent action is absent, not null: a script branches on presence.
    assert!(value.get("action").is_none(), "{value}");
    // The round trip is exact — this is the object a script receives.
    let back: ImEvent = serde_json::from_value(value).unwrap();
    assert_eq!(back, event);
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
