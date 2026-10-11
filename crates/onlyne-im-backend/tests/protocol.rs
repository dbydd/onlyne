//! The backend protocol is a wire contract, so its cases pin bytes.

use onlyne_im_backend::frame::{BackendOp, CoreOp, HelloArgs, SendArgs};
use onlyne_im_backend::payload::{ImAction, ImEvent, ImEventKind, ImFormat, ImPayload, ImSender};

#[test]
fn a_hello_frame_carries_the_platform_and_its_capabilities() {
    let msg = BackendOp::Hello(HelloArgs {
        platform: "telegram".into(),
        version: "2.1.2".into(),
        capabilities: vec!["message".into(), "action".into()],
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
