use serde::Serialize;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageSegment {
    Text { text: String },
    Mention { user_id: String },
    Image { file: String, url: String },
    Audio { file: String, url: String },
    Video { file: String, url: String },
    Reply { message_id: String },
    Unknown { kind: String },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct IncomingMessage {
    pub message_id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub is_group: bool,
    pub segments: Vec<MessageSegment>,
    pub timestamp: i64,
}

impl IncomingMessage {
    pub fn text_projection(&self) -> String {
        self.segments
            .iter()
            .map(|segment| match segment {
                MessageSegment::Text { text } => text.clone(),
                MessageSegment::Mention { user_id } => format!("@{user_id}"),
                MessageSegment::Image { .. } => "[图片]".to_owned(),
                MessageSegment::Audio { .. } => "[语音]".to_owned(),
                MessageSegment::Video { .. } => "[视频]".to_owned(),
                MessageSegment::Reply { .. } => "[引用消息]".to_owned(),
                MessageSegment::Unknown { kind } => format!("[{kind}]"),
            })
            .collect::<String>()
            .trim()
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_segments_without_retaining_media_payload_in_text() {
        let message = IncomingMessage {
            message_id: "42".into(),
            conversation_id: "group:7".into(),
            sender_id: "9".into(),
            is_group: true,
            segments: vec![
                MessageSegment::Text { text: "看".into() },
                MessageSegment::Image {
                    file: "local".into(),
                    url: "https://example.invalid".into(),
                },
            ],
            timestamp: 1,
        };
        assert_eq!(message.text_projection(), "看[图片]");
    }
}
