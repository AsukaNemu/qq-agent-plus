use std::fs;

use rinbot::{
    adapters::napcat::normalize_event,
    domain::{event::DomainEvent, message::MessageSegment},
};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&fs::read_to_string(path).expect("fixture")).expect("valid JSON fixture")
}

#[test]
fn private_fixture_maps_to_canonical_message() {
    let DomainEvent::Message(message) = normalize_event(&fixture("private-text")).expect("event")
    else {
        panic!("not a message")
    };
    assert_eq!(message.conversation_id, "private:2001");
    assert_eq!(message.sender_id, "2001");
    assert_eq!(message.text_projection(), "hello RInBot");
}

#[test]
fn group_fixture_preserves_media_kinds_without_raw_onebot_fields() {
    let DomainEvent::Message(message) = normalize_event(&fixture("group-media")).expect("event")
    else {
        panic!("not a message")
    };
    assert_eq!(message.conversation_id, "group:3001");
    assert!(message
        .segments
        .iter()
        .any(|segment| matches!(segment, MessageSegment::Image { .. })));
    assert!(message
        .segments
        .iter()
        .any(|segment| matches!(segment, MessageSegment::Audio { .. })));
    assert_eq!(message.text_projection(), "@12345[图片][语音]");
}

#[test]
fn notice_fixture_is_retained_as_traceable_unknown_event() {
    let DomainEvent::Unknown {
        post_type,
        event_type,
    } = normalize_event(&fixture("notice")).expect("event")
    else {
        panic!("not an unknown event")
    };
    assert_eq!(post_type, "notice");
    assert_eq!(event_type, "group_recall");
}
