//! Pure translation tests for the Weixin gateway plugin.
//!
//! All fixtures are local JSON/structs; no test touches the network.

use super::*;
use onlyne_adapter::Outbound;
use onlyne_proto::{MsgKind, Principal};
use wechat_ilink::{MessageItemType, MessageState, MessageType, TextItem};

fn text_wire(user: &str, text: &str) -> WireMessage {
    WireMessage {
        seq: None,
        message_id: Some(9001),
        from_user_id: user.to_string(),
        to_user_id: "bot".to_string(),
        client_id: "client-1".to_string(),
        create_time_ms: 1_700_000_000_000,
        update_time_ms: None,
        delete_time_ms: None,
        session_id: Some("session-9".to_string()),
        group_id: None,
        message_type: MessageType::User,
        message_state: MessageState::Finish,
        context_token: "ctx-token".to_string(),
        item_list: vec![wechat_ilink::WireMessageItem {
            item_type: MessageItemType::Text,
            create_time_ms: None,
            update_time_ms: None,
            is_completed: None,
            msg_id: Some("item-1".to_string()),
            text_item: Some(TextItem {
                text: Some(text.to_string()),
            }),
            image_item: None,
            voice_item: None,
            file_item: None,
            video_item: None,
            ref_msg: None,
        }],
    }
}

fn image_wire(user: &str) -> WireMessage {
    WireMessage {
        seq: None,
        message_id: Some(9002),
        from_user_id: user.to_string(),
        to_user_id: "bot".to_string(),
        client_id: "client-2".to_string(),
        create_time_ms: 1_700_000_000_010,
        update_time_ms: None,
        delete_time_ms: None,
        session_id: None,
        group_id: None,
        message_type: MessageType::User,
        message_state: MessageState::Finish,
        context_token: "ctx-token".to_string(),
        item_list: vec![wechat_ilink::WireMessageItem {
            item_type: MessageItemType::Image,
            create_time_ms: None,
            update_time_ms: None,
            is_completed: None,
            msg_id: Some("item-2".to_string()),
            text_item: None,
            image_item: Some(wechat_ilink::ImageItem {
                media: None,
                thumb_media: None,
                aeskey: None,
                url: Some("https://cdn.example/img.jpg".to_string()),
                mid_size: Some(12),
                thumb_size: None,
                thumb_width: Some(100),
                thumb_height: Some(50),
                hd_size: None,
            }),
            voice_item: None,
            file_item: None,
            video_item: None,
            ref_msg: None,
        }],
    }
}

fn test_principal() -> Principal {
    Principal::role("planner")
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

fn plugin_with_context(conversation: &str) -> WeixinPlugin {
    let plugin = WeixinPlugin::new(WeixinConfig::default());
    let context = WechatContext {
        account_key: "account-1".to_string(),
        user_id: conversation.to_string(),
        context_token: "ctx-token".to_string(),
        observed_at_unix_ms: 1_700_000_000_000,
        source_message_id: Some("item-1".to_string()),
    };
    block_on(plugin.observe_context(context));
    plugin
}

#[test]
fn inbound_text_maps_to_gateway_note() {
    let event = WeixinInboundEvent {
        wire: text_wire("user-1", "hello planner"),
        account_key: Some("account-1".to_string()),
        context_token: None,
        external_id: None,
    };
    let envelope = inbound_event_to_envelope(&event, test_principal()).expect("note envelope");
    assert_eq!(envelope.kind, MsgKind::Note);
    assert_eq!(envelope.body.text.as_deref(), Some("hello planner"));
    assert!(envelope.body.image.is_none());
    match &envelope.from {
        Principal::Gateway {
            gateway,
            channel,
            conversation,
        } => {
            assert_eq!(gateway, GATEWAY_ID);
            assert_eq!(channel, CHANNEL_ID);
            assert_eq!(conversation.as_deref(), Some("user-1"));
        }
        other => panic!("expected gateway principal, got {other:?}"),
    }
}

#[test]
fn inbound_task_prefix_maps_to_gateway_task() {
    let event = WeixinInboundEvent {
        wire: text_wire("user-2", "task: summarize the thread"),
        account_key: Some("account-1".to_string()),
        context_token: None,
        external_id: None,
    };
    let envelope = inbound_event_to_envelope(&event, test_principal()).expect("task envelope");
    assert_eq!(envelope.kind, MsgKind::Task);
    assert!(envelope.causality.is_some());
    assert!(envelope.op_id.is_some());
}

#[test]
fn inbound_image_keeps_text_body_and_url() {
    let event = WeixinInboundEvent {
        wire: image_wire("user-3"),
        account_key: Some("account-1".to_string()),
        context_token: None,
        external_id: None,
    };
    let envelope = inbound_event_to_envelope(&event, test_principal()).expect("image envelope");
    let text = envelope.body.text.expect("image url body");
    assert!(text.contains("https://cdn.example/img.jpg"));
}

#[test]
fn outbound_text_builds_sdk_payload() {
    let plugin = plugin_with_context("user-1");
    let outbound = Outbound {
        conversation: "user-1".to_string(),
        text: "hello back".to_string(),
        image: None,
        reply_to: None,
        kind: MsgKind::Note,
    };
    let request = block_on(outbound_to_request(&plugin, &outbound)).expect("text request");
    assert_eq!(request.conversation, "user-1");
    assert_eq!(request.text.as_deref(), Some("hello back"));
    assert!(request.image.is_none());
    let payload = text_payload_json(&request, "client-99");
    assert_eq!(
        payload.get("to_user_id").and_then(|value| value.as_str()),
        Some("user-1")
    );
    assert_eq!(
        payload.get("context_token").and_then(|value| value.as_str()),
        Some("ctx-token")
    );
    let item_text = payload
        .get("item_list")
        .and_then(|items| items.get(0))
        .and_then(|item| item.get("text_item"))
        .and_then(|item| item.get("text"))
        .and_then(|text| text.as_str())
        .expect("text item");
    assert_eq!(item_text, "hello back");
}

#[test]
fn outbound_image_decodes_and_maps_to_sdk_content() {
    let plugin = plugin_with_context("user-1");
    let bytes = vec![7u8; 64];
    let part = ImagePart {
        data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        mime: "image/png".to_string(),
        name: Some("chart.png".to_string()),
    };
    let outbound = Outbound {
        conversation: "user-1".to_string(),
        text: String::new(),
        image: Some(part),
        reply_to: Some("envelope-1".to_string()),
        kind: MsgKind::Note,
    };
    let request = block_on(outbound_to_request(&plugin, &outbound)).expect("image request");
    let upload = request.image.expect("decoded upload");
    assert_eq!(upload.data, bytes);
    match send_content_for_upload(upload) {
        SendContent::Image { data, caption } => {
            assert_eq!(data, bytes);
            assert!(caption.is_none());
        }
        SendContent::Text(_) => panic!("expected image send content"),
        SendContent::Video { .. } => panic!("expected image send content"),
        SendContent::File { .. } => panic!("expected image send content"),
    }
}

#[test]
fn gateway_ref_round_trip_preserves_row() {
    let decoded = GatewayRef::decode(
        &GatewayRef::new(
            "user-7",
            Some("external-42".to_string()),
            Some("session-9".to_string()),
        )
        .encode(),
    )
    .expect("gateway ref round trip");
    assert_eq!(decoded.channel, CHANNEL_ID);
    assert_eq!(decoded.conversation, "user-7");
    assert_eq!(decoded.external_id.as_deref(), Some("external-42"));
    assert_eq!(decoded.scene.as_deref(), Some("session-9"));
}

#[test]
fn unsupported_voice_is_rejected_cleanly() {
    let mut wire = text_wire("user-4", "voice transcript");
    wire.item_list[0].item_type = MessageItemType::Voice;
    wire.item_list[0].text_item = None;
    wire.item_list[0].voice_item = Some(wechat_ilink::VoiceItem {
        media: None,
        encode_type: None,
        bits_per_sample: None,
        sample_rate: None,
        text: Some("voice transcript".to_string()),
        playtime: Some(1000),
    });
    let event = WeixinInboundEvent {
        wire,
        account_key: Some("account-1".to_string()),
        context_token: None,
        external_id: None,
    };
    let err = inbound_event_to_envelope(&event, test_principal()).expect_err("voice unsupported");
    assert!(err.to_string().contains("unsupported weixin message type: voice"));
}

#[test]
fn oversized_image_is_rejected_before_upload() {
    let oversized = vec![0u8; IMAGE_BYTES_MAX + 1];
    let part = ImagePart {
        data_base64: base64::engine::general_purpose::STANDARD.encode(&oversized),
        mime: "image/png".to_string(),
        name: None,
    };
    let err = decode_outbound_image(&part).expect_err("image ceiling must fail first");
    assert!(err.to_string().contains(&IMAGE_BYTES_MAX.to_string()));
}
